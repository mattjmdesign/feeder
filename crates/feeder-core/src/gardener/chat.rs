//! Local, bounded transcripts for Curator sessions.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::{Event, Scope, SessionMode, ToolCall, clip};
use crate::{Error, Result, Store, secret_in};

const TITLE_CHARS: usize = 100;
const TEXT_BYTES: usize = 32 * 1024;
const CHAT_BYTES: i64 = 512 * 1024;
const CHAT_EVENTS: i64 = 2_048;
const END_RESERVE_BYTES: i64 = 64;
const REDACTED: &str = "[redacted]";
const OMITTED: &str = "[message exceeds saved history limit]";

/// What the person chose to send in the first turn.
#[derive(Clone, Copy, Debug)]
pub enum ChatOpening<'a> {
    /// A message typed by the person.
    Message(&'a str),
    /// A built-in action. Only the title is saved in the transcript's metadata.
    Job { title: &'a str, prompt: &'a str },
}

impl<'a> ChatOpening<'a> {
    pub(super) fn prompt(self) -> &'a str {
        match self {
            Self::Message(message) => message,
            Self::Job { prompt, .. } => prompt,
        }
    }
}

/// A saved chat's entry in the history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSummary {
    pub run_id: i64,
    pub started_at: i64,
    pub mode: SessionMode,
    pub scope: Scope,
    pub title: String,
    pub edits: i64,
    /// Whether the chat hit its saved history limit.
    pub truncated: bool,
}

/// One ordered transcript entry, separate from the live session controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StoredEvent {
    You(String),
    Event(Event),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TranscriptRow {
    Batch(Vec<StoredEvent>),
    Single(StoredEvent),
}

impl Store {
    /// Lists saved chats newest first, including partial chats from interrupted sessions.
    ///
    /// # Errors
    /// Returns an error if the database cannot be read.
    pub fn curator_chats(&self, limit: usize) -> Result<Vec<ChatSummary>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.started_at, r.read_only, r.project_id, c.title, r.edits, c.truncated
             FROM curator_chats c JOIN gardener_runs r ON r.id = c.run_id
             ORDER BY r.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(ChatSummary {
                run_id: row.get(0)?,
                started_at: row.get(1)?,
                mode: if row.get(2)? { SessionMode::Ask } else { SessionMode::Review },
                scope: row.get::<_, Option<i64>>(3)?.map_or(Scope::Notebook, Scope::Project),
                title: row.get(4)?,
                edits: row.get(5)?,
                truncated: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Loads the committed transcript for replay, in stream order.
    ///
    /// # Errors
    /// Returns an error if the chat is missing, deleted, or could not be read.
    pub fn curator_chat_events(&self, run_id: i64) -> Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.payload FROM curator_chats c
             LEFT JOIN curator_chat_events e ON e.run_id = c.run_id
             WHERE c.run_id = ?1 ORDER BY e.position",
        )?;
        let mut rows = stmt.query([run_id])?;
        let Some(mut row) = rows.next()? else { return Err(Error::NotFound(format!("no saved Curator chat {run_id}"))) };
        let mut events = Vec::new();
        loop {
            if let Some(payload) = row.get::<_, Option<String>>(0)? {
                let entry = serde_json::from_str(&payload).map_err(|_| Error::Invalid("couldn't read a saved Curator event".into()))?;
                match entry {
                    TranscriptRow::Batch(batch) => events.extend(batch),
                    TranscriptRow::Single(event) => events.push(event),
                }
            }
            let Some(next) = rows.next()? else { break };
            row = next;
        }
        Ok(events)
    }

    /// Deletes a chat and clears its saved summary and error. Keeps the run and its Notebook edits.
    ///
    /// # Errors
    /// Returns an error for an unknown run, an active session, or a database failure.
    pub fn delete_curator_chat(&self, run_id: i64) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if self.running_gardener()?.is_some_and(|run| run.id == run_id) {
            return Err(Error::Invalid("end the active Curator session before deleting its chat".into()));
        }
        if tx.execute("UPDATE gardener_runs SET summary = NULL, error = NULL WHERE id = ?1", [run_id])? == 0 {
            return Err(Error::NotFound(format!("no Curator run {run_id}")));
        }
        tx.execute("DELETE FROM curator_chats WHERE run_id = ?1", [run_id])?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS curator_chats (
            run_id INTEGER PRIMARY KEY REFERENCES gardener_runs(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            event_count INTEGER NOT NULL DEFAULT 0,
            payload_bytes INTEGER NOT NULL DEFAULT 0,
            truncated INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS curator_chat_events (
            run_id INTEGER NOT NULL REFERENCES curator_chats(run_id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            payload TEXT NOT NULL,
            PRIMARY KEY (run_id, position)
        ) WITHOUT ROWID;",
    )?;
    Ok(())
}

pub(super) fn create(conn: &Connection, run_id: i64, opening: ChatOpening<'_>) -> Result<()> {
    let title = match opening {
        ChatOpening::Message(text) => text,
        ChatOpening::Job { title, .. } => title,
    };
    let title = if title.trim().is_empty() {
        "Curator chat".to_string()
    } else {
        private_text(title, TITLE_CHARS).split_whitespace().collect::<Vec<_>>().join(" ")
    };
    conn.execute("INSERT INTO curator_chats (run_id, title) VALUES (?1, ?2)", params![run_id, title])?;
    if let ChatOpening::Message(text) = opening {
        append(conn, run_id, &[StoredEvent::You(bounded_private_text(text.to_string()))])?;
    }
    Ok(())
}

fn append(conn: &Connection, run_id: i64, events: &[StoredEvent]) -> Result<bool> {
    let ended = matches!(events, [StoredEvent::Event(Event::Ended)]);
    let payload = serde_json::to_string(events).map_err(|_| Error::Invalid("couldn't encode a Curator event".into()))?;
    let payload_bytes = i64::try_from(payload.len()).map_err(|_| Error::Invalid("Curator event is too large".into()))?;
    let event_count = i64::try_from(events.len()).map_err(|_| Error::Invalid("too many Curator events".into()))?;
    let byte_limit = if ended { CHAT_BYTES } else { CHAT_BYTES - END_RESERVE_BYTES };
    let event_limit = if ended { CHAT_EVENTS } else { CHAT_EVENTS - 1 };
    let position: Option<i64> = conn
        .query_row(
            "UPDATE curator_chats
             SET event_count = event_count + ?6, payload_bytes = payload_bytes + ?2
             WHERE run_id = ?1 AND event_count + ?6 <= ?3 AND payload_bytes + ?2 <= ?4 AND (truncated = 0 OR ?5)
             RETURNING event_count",
            params![run_id, payload_bytes, event_limit, byte_limit, ended, event_count],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(position) = position {
        conn.execute(
            "INSERT INTO curator_chat_events (run_id, position, payload) VALUES (?1, ?2, ?3)",
            params![run_id, position, payload],
        )?;
    } else {
        conn.execute("UPDATE curator_chats SET truncated = 1 WHERE run_id = ?1", [run_id])?;
    }
    Ok(position.is_some())
}

/// Event writes share the session's SQLite connection, in the order they are recorded.
pub(super) struct Recorder {
    store: Store,
    run_id: i64,
    pending: Option<PendingText>,
    capped: bool,
    pub(super) warned: bool,
}

enum PendingText {
    Reply(Deltas),
    Thinking(Deltas),
}

impl Recorder {
    pub(super) fn new(store: Store, run_id: i64) -> Self {
        Self { store, run_id, pending: None, capped: false, warned: false }
    }

    pub(super) fn record(&mut self, event: StoredEvent) -> Result<()> {
        let event = match (&mut self.pending, event) {
            (Some(PendingText::Reply(text)), StoredEvent::Event(Event::Text(delta)))
            | (Some(PendingText::Thinking(text)), StoredEvent::Event(Event::Thinking(delta))) => {
                text.push(delta);
                return Ok(());
            }
            (_, event) => event,
        };
        self.flush_text()?;
        match event {
            StoredEvent::Event(Event::Text(text)) => self.pending = Some(PendingText::Reply(Deltas::new(text))),
            StoredEvent::Event(Event::Thinking(text)) => self.pending = Some(PendingText::Thinking(Deltas::new(text))),
            event => self.save(&[sanitized_event(event)])?,
        }
        Ok(())
    }

    fn flush_text(&mut self) -> Result<()> {
        let events = match self.pending.take() {
            Some(PendingText::Reply(text)) => text.finish(Event::Text),
            Some(PendingText::Thinking(text)) => text.finish(Event::Thinking),
            None => return Ok(()),
        };
        self.save(&events)
    }

    fn save(&mut self, events: &[StoredEvent]) -> Result<()> {
        if self.capped {
            return Ok(());
        }
        let tx = Transaction::new_unchecked(&self.store.conn, TransactionBehavior::Immediate)?;
        let saved = append(&tx, self.run_id, events)?;
        tx.commit()?;
        self.capped |= !saved;
        Ok(())
    }

    pub(super) fn finish(&mut self, summary: Option<&str>, error: Option<&str>) -> Result<()> {
        let text = self.flush_text();
        let tx = Transaction::new_unchecked(&self.store.conn, TransactionBehavior::Immediate)?;
        append(&tx, self.run_id, &[StoredEvent::Event(Event::Ended)])?;
        self.store.finish_gardener_run(self.run_id, summary, error)?;
        tx.commit()?;
        text
    }
}

#[derive(Default)]
struct Deltas {
    parts: Vec<String>,
    bytes: usize,
    omitted: bool,
}

impl Deltas {
    fn new(delta: String) -> Self {
        let mut deltas = Self::default();
        deltas.push(delta);
        deltas
    }

    fn push(&mut self, delta: String) {
        if self.omitted {
            return;
        }
        if self.bytes + delta.len() > TEXT_BYTES || i64::try_from(self.parts.len()).unwrap_or(i64::MAX) >= CHAT_EVENTS {
            self.omitted = true;
            self.parts.clear();
            return;
        }
        self.bytes += delta.len();
        self.parts.push(delta);
    }

    fn finish(self, event: fn(String) -> Event) -> Vec<StoredEvent> {
        if self.omitted {
            return vec![StoredEvent::Event(event(OMITTED.into()))];
        }
        // Check the full text before saving any delta. A key can span multiple frames.
        if sensitive(&self.parts.concat()) {
            return vec![StoredEvent::Event(event(REDACTED.into()))];
        }
        self.parts.into_iter().map(|part| StoredEvent::Event(event(part))).collect()
    }
}

fn sensitive(text: &str) -> bool {
    secret_in(text).is_some()
        || text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_').any(|word| secret_in(word).is_some())
}

pub(super) fn private_text(text: &str, chars: usize) -> String {
    if sensitive(text) { REDACTED.into() } else { clip(text, chars) }
}

fn bounded_private_text(text: String) -> String {
    if text.len() > TEXT_BYTES {
        OMITTED.into()
    } else if sensitive(&text) {
        REDACTED.into()
    } else {
        text
    }
}

fn sanitized_event(event: StoredEvent) -> StoredEvent {
    let event = match event {
        StoredEvent::You(text) => return StoredEvent::You(bounded_private_text(text)),
        StoredEvent::Event(event) => match event {
            Event::Text(text) => Event::Text(bounded_private_text(text)),
            Event::Thinking(text) => Event::Thinking(bounded_private_text(text)),
            Event::Failed(text) => Event::Failed(bounded_private_text(text)),
            Event::ToolStarted(ToolCall { id, name, input }) => {
                let encoded = input.to_string();
                let input = if encoded.len() > TEXT_BYTES {
                    OMITTED.into()
                } else if sensitive(&encoded) {
                    REDACTED.into()
                } else {
                    input
                };
                Event::ToolStarted(ToolCall { id: bounded_private_text(id), name: bounded_private_text(name), input })
            }
            Event::ToolFinished { id, output, failed } => {
                Event::ToolFinished { id: bounded_private_text(id), output: bounded_private_text(output), failed }
            }
            event => event,
        },
    };
    StoredEvent::Event(event)
}
