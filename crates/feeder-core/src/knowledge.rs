//! The Notebook's learning loop: agents flag knowledge that is missing, wrong or outdated while
//! they work, and the gardener (a background curator agent) resolves those flags by editing pages.
//!
//! Every page change made through [`Store::curate`] is logged with the reason and the content it
//! replaced, so the person can read why the Notebook changed and revert any change.

use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::gardener::{MAX_EDITS_PER_RUN, Run, Scope, SessionMode};
use crate::{Error, Kind, Page, PageDraft, Result, Standing, Store, now_ms, secret_in};

/// The name the gardener writes under, in page bylines and the change log.
pub const GARDENER: &str = "Gardener";

/// Length limits for a flag's note (what's missing or wrong).
pub const FLAG_NOTE_CHARS: std::ops::RangeInclusive<usize> = 4..=2_000;

/// Length limit for a flag's optional suggested fix.
pub const FLAG_SUGGESTION_MAX_CHARS: usize = 10_000;

/// Length limit for the reason logged with a change.
pub const REASON_MAX_CHARS: usize = 1_000;

/// What an agent found while working.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlagKind {
    /// The Notebook has nothing on something the agent needed.
    Missing,
    /// A page says something that turned out to be false.
    Wrong,
    /// A page was true once but no longer is (a renamed command, a moved file).
    Outdated,
}

impl FlagKind {
    pub const ALL: [FlagKind; 3] = [FlagKind::Missing, FlagKind::Wrong, FlagKind::Outdated];

    pub fn as_str(self) -> &'static str {
        match self {
            FlagKind::Missing => "missing",
            FlagKind::Wrong => "wrong",
            FlagKind::Outdated => "outdated",
        }
    }

    /// Sentence-case label for the app.
    pub fn label(self) -> &'static str {
        match self {
            FlagKind::Missing => "Missing",
            FlagKind::Wrong => "Wrong",
            FlagKind::Outdated => "Outdated",
        }
    }
}

impl fmt::Display for FlagKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FlagKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<FlagKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "missing" | "gap" => Ok(FlagKind::Missing),
            "wrong" | "incorrect" => Ok(FlagKind::Wrong),
            "outdated" | "stale" => Ok(FlagKind::Outdated),
            other => Err(Error::Invalid(format!("unknown flag kind `{other}`: use missing, wrong or outdated"))),
        }
    }
}

/// Where a flag is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlagStatus {
    Open,
    /// Fixed by a Notebook change.
    Resolved,
    /// Looked at and set aside (not a real gap, or a duplicate).
    Dismissed,
}

impl FlagStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FlagStatus::Open => "open",
            FlagStatus::Resolved => "resolved",
            FlagStatus::Dismissed => "dismissed",
        }
    }

    fn parse(s: &str) -> FlagStatus {
        match s {
            "resolved" => FlagStatus::Resolved,
            "dismissed" => FlagStatus::Dismissed,
            _ => FlagStatus::Open,
        }
    }
}

/// One report that the Notebook is missing something, or says something wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flag {
    pub id: i64,
    pub kind: FlagKind,
    /// The page the flag is about, when there is one.
    pub page_id: Option<i64>,
    /// The project the agent was working in.
    pub project_id: Option<i64>,
    pub agent: String,
    pub note: String,
    pub suggestion: Option<String>,
    pub status: FlagStatus,
    /// What was done about it, in a sentence.
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    /// The Notebook change that resolved it.
    pub change_id: Option<i64>,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    /// The Feed thread that raised this flag.
    pub source_update_id: Option<i64>,
}

/// A flag before it is saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewFlag {
    pub kind: FlagKind,
    pub page_id: Option<i64>,
    pub project_id: Option<i64>,
    pub note: String,
    pub suggestion: Option<String>,
}

/// How a flag was closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagOutcome {
    /// Fixed, optionally by a logged Notebook change.
    Resolved {
        note: String,
        change_id: Option<i64>,
    },
    Dismissed {
        note: String,
    },
}

/// What a logged change did to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeAction {
    Created,
    Revised,
    Archived,
    Restored,
}

impl ChangeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeAction::Created => "created",
            ChangeAction::Revised => "revised",
            ChangeAction::Archived => "archived",
            ChangeAction::Restored => "restored",
        }
    }

    /// Past-tense verb for the app ("Revised", "Archived").
    pub fn label(self) -> &'static str {
        match self {
            ChangeAction::Created => "Wrote",
            ChangeAction::Revised => "Revised",
            ChangeAction::Archived => "Archived",
            ChangeAction::Restored => "Restored",
        }
    }

    fn parse(s: &str) -> Result<ChangeAction> {
        match s {
            "created" => Ok(ChangeAction::Created),
            "revised" => Ok(ChangeAction::Revised),
            "archived" => Ok(ChangeAction::Archived),
            "restored" => Ok(ChangeAction::Restored),
            other => Err(Error::Invalid(format!("unknown change action `{other}` in the change log"))),
        }
    }
}

/// A page's content at one moment, kept so a revision can be undone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub kind: String,
    pub topic: String,
    pub title: String,
    pub tags: Vec<String>,
    pub body: String,
}

impl PageSnapshot {
    fn of(page: &Page) -> PageSnapshot {
        PageSnapshot {
            kind: page.kind.as_str().to_string(),
            topic: page.topic.clone(),
            title: page.title.clone(),
            tags: page.tags.clone(),
            body: page.body.clone(),
        }
    }

    fn into_draft(self) -> PageDraft {
        PageDraft {
            kind: self.kind.parse().unwrap_or(Kind::Note),
            topic: Some(self.topic),
            title: self.title,
            tags: self.tags,
            body: self.body,
        }
    }
}

/// One logged change to the Notebook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub id: i64,
    pub page_id: i64,
    /// The page's title at the time of the change.
    pub page_title: String,
    pub action: ChangeAction,
    pub agent: String,
    pub reason: Option<String>,
    /// What a revision replaced. `None` for every other action.
    pub before: Option<PageSnapshot>,
    pub created_at: i64,
    pub reverted_at: Option<i64>,
    /// The change this one undid, for a revert's own log entry.
    pub undoes: Option<i64>,
    /// Feed updates the change drew on.
    pub source_updates: Vec<i64>,
    /// The session that saved this change, if it was made in a tracked run.
    pub gardener_run_id: Option<i64>,
    /// Knowledge flags that supported this edit.
    pub source_flags: Vec<i64>,
}

impl Change {
    pub fn is_reverted(&self) -> bool {
        self.reverted_at.is_some()
    }

    /// Whether Revert applies: not yet reverted, and not itself an undo.
    pub fn is_revertible(&self) -> bool {
        !self.is_reverted() && self.undoes.is_none()
    }
}

/// An edit the gardener (or any curator) wants to make, with its reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Curation {
    Create { draft: PageDraft, project_id: Option<i64> },
    Revise { page_id: i64, draft: PageDraft },
    Archive { page_id: i64 },
}

/// Why an edit is made and what it came from. The gardener must name at least one source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    pub reason: String,
    /// Open flags the edit fixes; they are resolved with it.
    pub flags: Vec<i64>,
    /// Feed updates (agent traces) the edit draws on.
    pub updates: Vec<i64>,
}

/// The result of [`Store::curate`]: the page as it now stands and the logged change.
#[derive(Clone, Debug, PartialEq)]
pub struct Curated {
    pub page: Page,
    pub change: Change,
}

const FLAG_COLS: &str = "id, kind, page_id, project_id, agent, note, suggestion, status, resolution, resolved_by, change_id, created_at, resolved_at, source_update_id";
const CHANGE_COLS: &str = "c.id, c.page_id, c.page_title, c.action, c.agent, c.reason, c.before, c.created_at, c.reverted_at, c.undoes, c.source_updates, c.gardener_run_id, c.source_flags";

impl Store {
    /// Files a flag about missing or wrong knowledge.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the note is too short or long, or looks like it holds a secret;
    /// [`Error::NotFound`] when `page_id` names no page.
    pub fn flag_knowledge(&self, flag: NewFlag, agent: &str) -> Result<Flag> {
        self.insert_flag(flag, agent, None)
    }

    /// Flags a Feed thread for the Gardener, reusing an already-open flag for that thread.
    ///
    /// # Errors
    /// Returns an error when the update no longer exists or the store cannot save the flag.
    pub fn flag_update_for_notebook(&self, update_id: i64, author: &str) -> Result<Flag> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let update = self.update(update_id)?.ok_or_else(|| Error::NotFound(format!("no Feed update {update_id}")))?;
        let existing = tx
            .query_row(
                &format!("SELECT {FLAG_COLS} FROM knowledge_flags WHERE source_update_id = ?1 AND status = 'open'"),
                [update_id],
                flag_row,
            )
            .optional()?;
        if let Some(flag) = existing {
            return Ok(flag);
        }
        let flag = self.insert_flag(
            NewFlag {
                kind: FlagKind::Missing,
                page_id: None,
                project_id: Some(update.project_id),
                note: "Look for a reusable lesson in this Feed update and its replies.".into(),
                suggestion: None,
            },
            author,
            Some(update_id),
        )?;
        tx.commit()?;
        Ok(flag)
    }

    fn insert_flag(&self, flag: NewFlag, agent: &str, source_update_id: Option<i64>) -> Result<Flag> {
        let note = flag.note.trim().to_string();
        let count = note.chars().count();
        if !FLAG_NOTE_CHARS.contains(&count) {
            return Err(Error::Invalid(format!(
                "say what's missing or wrong in {}–{} characters (got {count})",
                FLAG_NOTE_CHARS.start(),
                FLAG_NOTE_CHARS.end()
            )));
        }
        let suggestion = flag.suggestion.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(s) = &suggestion
            && s.chars().count() > FLAG_SUGGESTION_MAX_CHARS
        {
            return Err(Error::Invalid(format!("keep the suggestion under {FLAG_SUGGESTION_MAX_CHARS} characters")));
        }
        for text in std::iter::once(&note).chain(suggestion.as_ref()) {
            if let Some(what) = secret_in(text) {
                return Err(Error::Invalid(format!("this flag looks like it contains {what}; leave secrets out")));
            }
        }
        if let Some(id) = flag.page_id
            && self.page(id)?.is_none()
        {
            return Err(Error::NotFound(format!("no notebook page {id}")));
        }
        self.conn.execute(
            "INSERT INTO knowledge_flags (kind, page_id, project_id, agent, note, suggestion, status, created_at, source_update_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?8)",
            params![flag.kind.as_str(), flag.page_id, flag.project_id, agent.trim(), note, suggestion, now_ms(), source_update_id],
        )?;
        let id = self.conn.last_insert_rowid();
        self.flag(id)?.ok_or_else(|| Error::NotFound(format!("flag {id} vanished")))
    }

    pub fn flag(&self, id: i64) -> Result<Option<Flag>> {
        Ok(self.conn.query_row(&format!("SELECT {FLAG_COLS} FROM knowledge_flags WHERE id = ?1"), params![id], flag_row).optional()?)
    }

    /// Flags in `status` (every flag when `None`), the newest first.
    pub fn flags(&self, status: Option<FlagStatus>, limit: usize) -> Result<Vec<Flag>> {
        self.flags_in_scope(status, Scope::Notebook, limit)
    }

    /// Lists flags in the selected scope before applying the limit.
    pub fn flags_in_scope(&self, status: Option<FlagStatus>, scope: Scope, limit: usize) -> Result<Vec<Flag>> {
        let limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FLAG_COLS} FROM knowledge_flags f
             WHERE (?1 IS NULL OR f.status = ?1)
               AND (?2 IS NULL OR (COALESCE(f.project_id, (SELECT p.project_id FROM notebook_pages p WHERE p.id = f.page_id)) = ?2
                   AND NOT EXISTS(SELECT 1 FROM notebook_pages p WHERE p.id = f.page_id AND p.project_id <> ?2)))
             ORDER BY f.created_at DESC, f.id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![status.map(FlagStatus::as_str), scope.project_id(), limit], flag_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn open_flag_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM knowledge_flags WHERE status = 'open'", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Closes an open flag.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown flag or change; [`Error::Invalid`] when the flag is
    /// already closed.
    pub fn close_flag(&self, id: i64, outcome: FlagOutcome, by: &str) -> Result<Flag> {
        let flag = self.flag(id)?.ok_or_else(|| Error::NotFound(format!("no flag {id}")))?;
        if flag.status != FlagStatus::Open {
            return Err(Error::Invalid(format!("flag {id} is already {}", flag.status.as_str())));
        }
        let (status, note, change_id) = match outcome {
            FlagOutcome::Resolved { note, change_id } => (FlagStatus::Resolved, note, change_id),
            FlagOutcome::Dismissed { note } => (FlagStatus::Dismissed, note, None),
        };
        if let Some(change) = change_id
            && self.change(change)?.is_none()
        {
            return Err(Error::NotFound(format!("no notebook change {change}")));
        }
        let note = note.trim().to_string();
        self.conn.execute(
            "UPDATE knowledge_flags SET status = ?1, resolution = ?2, resolved_by = ?3, change_id = ?4, resolved_at = ?5 WHERE id = ?6",
            params![status.as_str(), (!note.is_empty()).then_some(note), by.trim(), change_id, now_ms(), id],
        )?;
        self.flag(id)?.ok_or_else(|| Error::NotFound(format!("no flag {id}")))
    }

    /// Makes one Notebook edit, logs it with its provenance, and resolves the flags it fixes, all
    /// in one transaction.
    ///
    /// The [`GARDENER`] is held to tighter rules: it must name a source (a flag or a Feed update),
    /// and on pages a person wrote or edited by hand it may only add to the end, never rewrite or
    /// archive.
    ///
    /// # Errors
    /// Whatever the edit itself returns (limits, near-duplicates, unknown pages), and
    /// [`Error::Invalid`] for a long reason, a flag that isn't open, or a broken gardener rule.
    /// Nothing is written on error.
    pub fn curate(&self, curation: Curation, author: &str, provenance: &Provenance) -> Result<Curated> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let result = self.curate_inner(curation, author, provenance, None)?;
        tx.commit()?;
        Ok(result)
    }

    /// Saves an edit and its evidence under the active session's authority.
    ///
    /// # Errors
    /// Rejects inactive runs, out-of-scope inputs and edits over the session limit.
    pub fn curate_gardener_run(&self, run_id: i64, mut curation: Curation, provenance: &Provenance) -> Result<Curated> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let run = self.active_curator_review(run_id)?;
        if usize::try_from(run.edits).unwrap_or(usize::MAX) >= MAX_EDITS_PER_RUN {
            return Err(Error::Invalid(format!(
                "this session has made its {MAX_EDITS_PER_RUN} changes; leave the rest for the next session"
            )));
        }
        let mut evidence = provenance.clone();
        let mut project = run.scope.project_id();
        for &id in &evidence.flags {
            let flag = self.gardener_flag(run.scope, id)?;
            if let Some(id) = flag.source_update_id
                && !evidence.updates.contains(&id)
            {
                evidence.updates.push(id);
            }
            merge_evidence_project(&mut project, self.flag_project(&flag)?)?;
        }
        for &id in &evidence.updates {
            let update = self
                .update(id)?
                .filter(|u| run.scope.includes(Some(u.project_id)))
                .ok_or_else(|| Error::NotFound(format!("no Feed update {id} in this session")))?;
            merge_evidence_project(&mut project, Some(update.project_id))?;
        }
        match &mut curation {
            Curation::Create { project_id, .. } => *project_id = project,
            Curation::Revise { page_id, .. } | Curation::Archive { page_id } => {
                let page = self
                    .page(*page_id)?
                    .filter(|p| run.scope.includes(p.project_id))
                    .ok_or_else(|| Error::NotFound(format!("no Notebook page {page_id} in this session")))?;
                if project.is_some() && page.project_id != project {
                    return Err(Error::Invalid("the evidence belongs to a different project than this note".into()));
                }
            }
        }
        let result = self.curate_inner(curation, GARDENER, &evidence, Some(run_id))?;
        tx.execute("UPDATE gardener_runs SET edits = edits + 1 WHERE id = ?1", [run_id])?;
        tx.commit()?;
        Ok(result)
    }

    /// Dismisses an in-scope flag while the session is still active.
    pub fn dismiss_gardener_flag(&self, run_id: i64, id: i64, reason: String) -> Result<Flag> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.gardener_flag(self.active_curator_review(run_id)?.scope, id)?;
        let flag = self.close_flag(id, FlagOutcome::Dismissed { note: reason }, GARDENER)?;
        tx.commit()?;
        Ok(flag)
    }

    fn active_curator_review(&self, run_id: i64) -> Result<Run> {
        let run = self.active_gardener_run(run_id)?;
        if run.mode == SessionMode::Ask {
            return Err(Error::Invalid("this Curator session is read-only".into()));
        }
        Ok(run)
    }

    fn flag_project(&self, flag: &Flag) -> Result<Option<i64>> {
        match (flag.project_id, flag.page_id) {
            (None, Some(id)) => Ok(self.page(id)?.and_then(|p| p.project_id)),
            (project, _) => Ok(project),
        }
    }

    fn gardener_flag(&self, scope: Scope, id: i64) -> Result<Flag> {
        let missing = || Error::NotFound(format!("no flag {id} in this session"));
        let flag = self.flag(id)?.ok_or_else(missing)?;
        if !scope.includes(self.flag_project(&flag)?) {
            return Err(missing());
        }
        if let Some(page_id) = flag.page_id
            && let Some(project_id) = self.page(page_id)?.and_then(|p| p.project_id)
            && !scope.includes(Some(project_id))
        {
            return Err(missing());
        }
        Ok(flag)
    }

    fn curate_inner(&self, curation: Curation, author: &str, provenance: &Provenance, gardener_run_id: Option<i64>) -> Result<Curated> {
        let reason = provenance.reason.trim();
        if reason.chars().count() > REASON_MAX_CHARS {
            return Err(Error::Invalid(format!("keep the reason under {REASON_MAX_CHARS} characters")));
        }
        if author == GARDENER {
            self.check_gardener_edit(&curation, provenance)?;
        }
        let (page, action, before) = match curation {
            Curation::Create { draft, project_id } => {
                let similar = if gardener_run_id.is_some() { crate::OnSimilar::RefuseInScope } else { crate::OnSimilar::Refuse };
                let page = self.create_page(draft, author, project_id, similar)?;
                (page, ChangeAction::Created, None)
            }
            Curation::Revise { page_id, draft } => {
                let old = self.page(page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {page_id}")))?;
                let page = self.revise_page(page_id, draft, author)?;
                (page, ChangeAction::Revised, Some(PageSnapshot::of(&old)))
            }
            Curation::Archive { page_id } => {
                self.set_page_standing(page_id, Standing::Archived)?;
                let page = self.page(page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {page_id}")))?;
                (page, ChangeAction::Archived, None)
            }
        };
        let change_id = self.log_change(&page, action, author, reason, before.as_ref())?;
        let after = serde_json::to_string(&(PageSnapshot::of(&page), page.is_archived()))
            .map_err(|e| Error::Invalid(format!("can't record the saved note: {e}")))?;
        self.conn.execute(
            "UPDATE notebook_changes SET source_updates = ?1, source_flags = ?2, gardener_run_id = ?3, after_state = ?4 WHERE id = ?5",
            params![encode_ids(&provenance.updates), encode_ids(&provenance.flags), gardener_run_id, after, change_id],
        )?;
        // Cited flags must exist. Open ones are resolved by this change; a flag that's already
        // closed still counts as the source (say, a follow-up edit to the page that resolved it).
        for &flag in &provenance.flags {
            let found = self.flag(flag)?.ok_or_else(|| Error::NotFound(format!("no flag {flag} to cite")))?;
            if found.status == FlagStatus::Open {
                self.close_flag(flag, FlagOutcome::Resolved { note: reason.to_string(), change_id: Some(change_id) }, author)?;
            }
        }
        let change = self.change(change_id)?.ok_or_else(|| Error::NotFound(format!("change {change_id} vanished")))?;
        Ok(Curated { page, change })
    }

    fn check_gardener_edit(&self, curation: &Curation, provenance: &Provenance) -> Result<()> {
        if provenance.flags.is_empty() && provenance.updates.is_empty() {
            return Err(Error::Invalid("the gardener must name the flag or Feed update an edit comes from".into()));
        }
        if provenance.reason.trim().is_empty() {
            return Err(Error::Invalid("the gardener must say why it made an edit".into()));
        }
        for &id in &provenance.updates {
            if self.update(id)?.is_none() {
                return Err(Error::NotFound(format!("no Feed update {id} to cite")));
            }
        }
        let (page_id, draft) = match curation {
            Curation::Create { draft, project_id } => return self.check_gardener_links(*project_id, None, &draft.body),
            Curation::Revise { page_id, draft } => (*page_id, Some(draft)),
            Curation::Archive { page_id } => (*page_id, None),
        };
        let page = self.page(page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {page_id}")))?;
        if let Some(draft) = draft {
            self.check_gardener_links(page.project_id, Some(&page), &draft.body)?;
        }
        if !page.human_edited {
            return Ok(());
        }
        let appends = draft.is_some_and(|d| d.title.trim() == page.title && d.body.trim().starts_with(page.body.trim()));
        if appends {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "page #{page_id} was written by hand, so the gardener may only add to its end; flag it for the person instead"
            )))
        }
    }

    fn check_gardener_links(&self, project_id: Option<i64>, previous: Option<&Page>, body: &str) -> Result<()> {
        let existing: std::collections::BTreeSet<_> = previous.into_iter().flat_map(|page| notebook_link_ids(&page.body)).collect();
        for id in notebook_link_ids(body).filter(|id| !existing.contains(id)) {
            if previous.is_some_and(|page| page.id == id) {
                return Err(Error::Invalid(format!("page #{id} cannot link to itself")));
            }
            self.page(id)?.filter(|page| !page.is_archived() && (page.project_id.is_none() || page.project_id == project_id)).ok_or_else(
                || Error::Invalid(format!("link target #{id} must be a current note in the same project or the shared Notebook")),
            )?;
        }
        Ok(())
    }

    pub fn change(&self, id: i64) -> Result<Option<Change>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {CHANGE_COLS} FROM notebook_changes c WHERE c.id = ?1"))?;
        let row = stmt.query_row(params![id], change_row).optional()?;
        row.transpose()
    }

    /// Logged changes, the newest first, optionally only one author's or one page's.
    pub fn changes(&self, agent: Option<&str>, page_id: Option<i64>, limit: usize) -> Result<Vec<Change>> {
        let limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHANGE_COLS} FROM notebook_changes c
             WHERE (?1 IS NULL OR c.agent = ?1) AND (?2 IS NULL OR c.page_id = ?2)
             ORDER BY c.created_at DESC, c.id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![agent, page_id, limit], change_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().collect()
    }

    /// Changes saved by this run, newest first, including their current Revert state.
    pub fn gardener_run_changes(&self, run_id: i64) -> Result<Vec<Change>> {
        let mut stmt =
            self.conn.prepare(&format!("SELECT {CHANGE_COLS} FROM notebook_changes c WHERE c.gardener_run_id = ?1 ORDER BY c.id DESC"))?;
        let rows = stmt.query_map([run_id], change_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().collect()
    }

    /// Notebook changes that cite this Feed thread, newest first.
    pub fn changes_from_update(&self, update_id: i64) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHANGE_COLS} FROM notebook_changes c
             WHERE instr(' ' || c.source_updates || ' ', ' ' || ?1 || ' ') > 0
             ORDER BY c.id DESC"
        ))?;
        let rows = stmt.query_map([update_id], change_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().collect()
    }

    /// Undoes a logged change: a written page is archived, a revision goes back to the text it
    /// replaced, and an archived page comes back. The undo is logged too, and flags the change
    /// resolved open again.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the change was already reverted, or when the page has changed
    /// since (revert the newer change first, so nothing is silently lost).
    pub fn revert_change(&self, id: i64, by: &str) -> Result<Curated> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let change = self.change(id)?.ok_or_else(|| Error::NotFound(format!("no notebook change {id}")))?;
        if change.is_reverted() {
            return Err(Error::Invalid(format!("change {id} was already reverted")));
        }
        if change.undoes.is_some() {
            return Err(Error::Invalid(format!("change {id} is itself a revert")));
        }
        let newer_live: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM notebook_changes
                           WHERE page_id = ?1 AND id > ?2 AND reverted_at IS NULL AND undoes IS NULL)",
            params![change.page_id, id],
            |r| r.get(0),
        )?;
        if newer_live {
            return Err(Error::Invalid(format!(
                "page #{} changed after this; revert its newer change first so nothing is lost",
                change.page_id
            )));
        }
        let expected: Option<String> = tx.query_row("SELECT after_state FROM notebook_changes WHERE id = ?1", [id], |r| r.get(0))?;
        if let Some(expected) = expected {
            let state: (PageSnapshot, bool) =
                serde_json::from_str(&expected).map_err(|e| Error::Invalid(format!("change {id} has unreadable saved text: {e}")))?;
            let current = self.page(change.page_id)?.ok_or_else(|| Error::NotFound(format!("no Notebook page {}", change.page_id)))?;
            if (PageSnapshot::of(&current), current.is_archived()) != state {
                return Err(Error::Invalid("this note was edited after the gardener saved it; the current version was kept".into()));
            }
        }
        let reason = format!("Reverted: {}", change.reason.as_deref().unwrap_or(change.action.label()));
        let curated = match change.action {
            ChangeAction::Created | ChangeAction::Restored => {
                let old = self.page(change.page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {}", change.page_id)))?;
                self.set_page_standing(old.id, Standing::Archived)?;
                let page = self.page(old.id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {}", old.id)))?;
                let change_id = self.log_change(&page, ChangeAction::Archived, by, &reason, None)?;
                (page, change_id)
            }
            ChangeAction::Revised => {
                let before = change.before.clone().ok_or_else(|| Error::Invalid(format!("change {id} kept no earlier text")))?;
                let old = self.page(change.page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {}", change.page_id)))?;
                let page = self.revise_page(change.page_id, before.into_draft(), by)?;
                let change_id = self.log_change(&page, ChangeAction::Revised, by, &reason, Some(&PageSnapshot::of(&old)))?;
                (page, change_id)
            }
            ChangeAction::Archived => {
                self.set_page_standing(change.page_id, Standing::Current)?;
                let page = self.page(change.page_id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {}", change.page_id)))?;
                let change_id = self.log_change(&page, ChangeAction::Restored, by, &reason, None)?;
                (page, change_id)
            }
        };
        self.conn.execute("UPDATE notebook_changes SET reverted_at = ?1 WHERE id = ?2", params![now_ms(), id])?;
        self.conn.execute("UPDATE notebook_changes SET undoes = ?1 WHERE id = ?2", params![id, curated.1])?;
        // The flags this change fixed are unfixed again, so the gardener sees them next pass.
        self.conn.execute(
            "UPDATE knowledge_flags SET status = 'open', resolution = NULL, resolved_by = NULL, change_id = NULL, resolved_at = NULL
             WHERE change_id = ?1",
            params![id],
        )?;
        tx.commit()?;
        let (page, change_id) = curated;
        let change = self.change(change_id)?.ok_or_else(|| Error::NotFound(format!("change {change_id} vanished")))?;
        Ok(Curated { page, change })
    }

    /// Counts that change whenever flags or the change log do, for the app's poll loop.
    pub fn knowledge_stamp(&self) -> Result<(i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT (SELECT COALESCE(MAX(id), 0) FROM knowledge_flags),
                    (SELECT COUNT(*) FROM knowledge_flags WHERE status = 'open'),
                    (SELECT COALESCE(MAX(id), 0) FROM notebook_changes),
                    (SELECT COUNT(*) FROM notebook_changes WHERE reverted_at IS NOT NULL)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }

    fn log_change(&self, page: &Page, action: ChangeAction, agent: &str, reason: &str, before: Option<&PageSnapshot>) -> Result<i64> {
        let before =
            before.map(serde_json::to_string).transpose().map_err(|e| Error::Invalid(format!("can't record the old text: {e}")))?;
        self.conn.execute(
            "INSERT INTO notebook_changes (page_id, page_title, action, agent, reason, before, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![page.id, page.title, action.as_str(), agent.trim(), (!reason.is_empty()).then_some(reason), before, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
}

fn notebook_link_ids(body: &str) -> impl Iterator<Item = i64> + '_ {
    body.split("feeder://notebook/").skip(1).filter_map(|tail| {
        let end = tail.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(tail.len());
        tail[..end].parse().ok()
    })
}

fn merge_evidence_project(project: &mut Option<i64>, source: Option<i64>) -> Result<()> {
    if let Some(id) = source {
        if project.is_some_and(|existing| existing != id) {
            return Err(Error::Invalid("use evidence from one project per note".into()));
        }
        *project = Some(id);
    }
    Ok(())
}

fn flag_row(r: &Row<'_>) -> rusqlite::Result<Flag> {
    let kind: String = r.get(1)?;
    let status: String = r.get(7)?;
    Ok(Flag {
        id: r.get(0)?,
        kind: kind.parse().unwrap_or(FlagKind::Missing),
        page_id: r.get(2)?,
        project_id: r.get(3)?,
        agent: r.get(4)?,
        note: r.get(5)?,
        suggestion: r.get(6)?,
        status: FlagStatus::parse(&status),
        resolution: r.get(8)?,
        resolved_by: r.get(9)?,
        change_id: r.get(10)?,
        created_at: r.get(11)?,
        resolved_at: r.get(12)?,
        source_update_id: r.get(13)?,
    })
}

/// Reads a change row; the outer `Result` is SQLite's, the inner one a malformed log entry.
fn change_row(r: &Row<'_>) -> rusqlite::Result<Result<Change>> {
    let action: String = r.get(3)?;
    let before: Option<String> = r.get(6)?;
    let id: i64 = r.get(0)?;
    let parsed = ChangeAction::parse(&action).and_then(|action| {
        let before = before
            .map(|json| serde_json::from_str::<PageSnapshot>(&json))
            .transpose()
            .map_err(|e| Error::Invalid(format!("change {id} has unreadable earlier text: {e}")))?;
        Ok((action, before))
    });
    let (page_id, page_title, agent, reason) = (r.get(1)?, r.get(2)?, r.get(4)?, r.get(5)?);
    let (created_at, reverted_at, undoes) = (r.get(7)?, r.get(8)?, r.get(9)?);
    let source_updates = decode_ids(r.get::<_, Option<String>>(10)?.as_deref());
    let gardener_run_id = r.get(11)?;
    let source_flags = decode_ids(r.get::<_, Option<String>>(12)?.as_deref());
    Ok(parsed.map(|(action, before)| Change {
        id,
        page_id,
        page_title,
        action,
        agent,
        reason,
        before,
        created_at,
        reverted_at,
        undoes,
        source_updates,
        gardener_run_id,
        source_flags,
    }))
}

fn encode_ids(ids: &[i64]) -> Option<String> {
    (!ids.is_empty()).then(|| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(" "))
}

fn decode_ids(ids: Option<&str>) -> Vec<i64> {
    ids.unwrap_or_default().split_whitespace().filter_map(|id| id.parse().ok()).collect()
}

/// Creates the flag and change-log tables. Runs after the notebook's own migration.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notebook_changes (
            id INTEGER PRIMARY KEY,
            page_id INTEGER NOT NULL REFERENCES notebook_pages(id) ON DELETE CASCADE,
            page_title TEXT NOT NULL,
            action TEXT NOT NULL,
            agent TEXT NOT NULL,
            reason TEXT,
            before TEXT,
            created_at INTEGER NOT NULL,
            reverted_at INTEGER,
            undoes INTEGER REFERENCES notebook_changes(id) ON DELETE SET NULL,
            source_updates TEXT
        );
        CREATE INDEX IF NOT EXISTS notebook_changes_by_page ON notebook_changes(page_id, id DESC);
        CREATE INDEX IF NOT EXISTS notebook_changes_by_time ON notebook_changes(created_at DESC, id DESC);
        CREATE TABLE IF NOT EXISTS knowledge_flags (
            id INTEGER PRIMARY KEY,
            kind TEXT NOT NULL,
            page_id INTEGER REFERENCES notebook_pages(id) ON DELETE SET NULL,
            project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            agent TEXT NOT NULL,
            note TEXT NOT NULL,
            suggestion TEXT,
            status TEXT NOT NULL DEFAULT 'open',
            resolution TEXT,
            resolved_by TEXT,
            change_id INTEGER REFERENCES notebook_changes(id) ON DELETE SET NULL,
            created_at INTEGER NOT NULL,
            resolved_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS knowledge_flags_by_status ON knowledge_flags(status, created_at DESC, id DESC);",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempStore {
        store: Store,
        dir: std::path::PathBuf,
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_store(tag: &str) -> TempStore {
        let dir = std::env::temp_dir().join(format!("feeder-knowledge-{tag}-{}-{}", std::process::id(), fastrand::u32(..)));
        TempStore { store: Store::open(&dir).expect("store opens"), dir }
    }

    fn draft(title: &str, body: &str) -> PageDraft {
        PageDraft { kind: Kind::Note, topic: Some("tooling".into()), title: title.into(), tags: vec![], body: body.into() }
    }

    fn cites(reason: &str, flags: &[i64]) -> Provenance {
        Provenance { reason: reason.into(), flags: flags.to_vec(), updates: vec![] }
    }

    fn missing(note: &str) -> NewFlag {
        NewFlag { kind: FlagKind::Missing, page_id: None, project_id: None, note: note.into(), suggestion: None }
    }

    #[test]
    fn flags_are_checked_listed_and_closed_once() {
        let t = temp_store("flags");
        let s = &t.store;
        assert!(s.flag_knowledge(missing("no"), "Codex").is_err(), "a too-short note is refused");
        let leaked = NewFlag { suggestion: Some("use ghp_0123456789abcdefghijklmnopqrstuvwxyzAB".into()), ..missing("token for CI") };
        assert!(s.flag_knowledge(leaked, "Codex").is_err(), "a secret in the suggestion is refused");
        let bad_page = NewFlag { page_id: Some(999), ..missing("page 999 is wrong") };
        assert!(matches!(s.flag_knowledge(bad_page, "Codex"), Err(Error::NotFound(_))));

        let flag = s.flag_knowledge(missing("  How to run the site build  "), "Codex").expect("flag saves");
        assert_eq!(flag.note, "How to run the site build");
        assert_eq!(flag.status, FlagStatus::Open);
        assert_eq!(s.open_flag_count().expect("count"), 1);

        let closed = s.close_flag(flag.id, FlagOutcome::Dismissed { note: "Already in README".into() }, "You").expect("closes");
        assert_eq!(closed.status, FlagStatus::Dismissed);
        assert_eq!(closed.resolution.as_deref(), Some("Already in README"));
        assert!(s.close_flag(flag.id, FlagOutcome::Dismissed { note: String::new() }, "You").is_err(), "closing twice is refused");
        assert_eq!(s.open_flag_count().expect("count"), 0);
        assert_eq!(s.flags(Some(FlagStatus::Open), 10).expect("list").len(), 0);
        assert_eq!(s.flags(None, 10).expect("list").len(), 1);
    }

    #[test]
    fn curate_logs_the_change_and_resolves_its_flags_atomically() {
        let t = temp_store("curate");
        let s = &t.store;
        let flag = s.flag_knowledge(missing("Nothing on running the site build"), "Codex").expect("flag");
        let made = s
            .curate(
                Curation::Create { draft: draft("Run the site build", "Run `pnpm --dir site build`."), project_id: None },
                GARDENER,
                &cites("Codex couldn't find how to build the site", &[flag.id]),
            )
            .expect("curates");
        assert_eq!(made.change.action, ChangeAction::Created);
        assert_eq!(made.page.agent, GARDENER);
        let flag = s.flag(flag.id).expect("read").expect("exists");
        assert_eq!(flag.status, FlagStatus::Resolved);
        assert_eq!(flag.change_id, Some(made.change.id));

        // A bad edit writes nothing: no page, no log entry, the flag stays open.
        let other = s.flag_knowledge(missing("Nothing on deploys"), "Codex").expect("flag");
        let before = s.changes(None, None, 50).expect("log").len();
        let refused = s.curate(Curation::Revise { page_id: made.page.id, draft: draft("x", "") }, GARDENER, &cites("bad", &[other.id]));
        assert!(refused.is_err());
        assert_eq!(s.changes(None, None, 50).expect("log").len(), before);
        assert_eq!(s.flag(other.id).expect("read").expect("exists").status, FlagStatus::Open);
    }

    #[test]
    fn reverting_does_not_archive_a_note_someone_edited_after_it_was_saved() {
        let t = temp_store("revert-after-human-edit");
        let s = &t.store;
        let flag = s.flag_knowledge(missing("Missing a release check"), "Codex").expect("flag");
        let saved = s
            .curate(
                Curation::Create { draft: draft("Run the release check", "Run the check."), project_id: None },
                GARDENER,
                &Provenance { flags: vec![flag.id], reason: "The flag has evidence.".into(), updates: vec![] },
            )
            .expect("save the note");
        let edited = s
            .revise_page(saved.page.id, draft("Run the release check", "Run the check after building."), crate::HUMAN_AUTHOR)
            .expect("edit the note");
        assert!(s.revert_change(saved.change.id, crate::HUMAN_AUTHOR).is_err());
        assert_eq!(s.page(edited.id).expect("load current note"), Some(edited));
    }

    #[test]
    fn reverts_restore_the_old_text_in_order_and_reopen_flags() {
        let t = temp_store("revert");
        let s = &t.store;
        let page = s.create_page(draft("Release steps", "Run release.sh."), "Codex", None, crate::OnSimilar::Refuse).expect("page");
        let flag = s.flag_knowledge(NewFlag { page_id: Some(page.id), ..missing("release.sh moved to scripts/") }, "Codex").expect("flag");
        let project = s.project_named("Alpha", None).expect("project");
        let update = s
            .post(
                project.id,
                crate::NewUpdate { agent: "Codex".into(), body: "Done: moved docs to AGENTS.md.".into(), ..Default::default() },
            )
            .expect("post")
            .id;
        let first = s
            .curate(
                Curation::Revise { page_id: page.id, draft: draft("Release steps", "Run scripts/release.sh.") },
                GARDENER,
                &cites("The script moved", &[flag.id]),
            )
            .expect("revise");
        assert_eq!(first.change.before.as_ref().map(|b| b.body.as_str()), Some("Run release.sh."));
        let second = s
            .curate(
                Curation::Archive { page_id: page.id },
                GARDENER,
                &Provenance { reason: "Superseded by AGENTS.md".into(), flags: vec![], updates: vec![update] },
            )
            .expect("archive");

        let early = s.revert_change(first.change.id, "You");
        assert!(matches!(early, Err(Error::Invalid(_))), "an older change can't jump the queue");

        let undo = s.revert_change(second.change.id, "You").expect("unarchive");
        assert_eq!(undo.change.action, ChangeAction::Restored);
        assert!(!undo.page.is_archived());

        let undo = s.revert_change(first.change.id, "You").expect("restore text");
        assert_eq!(undo.page.body, "Run release.sh.");
        assert_eq!(s.flag(flag.id).expect("read").expect("exists").status, FlagStatus::Open, "its flag is open again");
        assert!(s.change(first.change.id).expect("read").expect("exists").is_reverted());
        assert!(s.revert_change(first.change.id, "You").is_err(), "a change reverts once");
        assert!(!undo.change.is_revertible(), "an undo isn't offered for revert");
        assert_eq!(undo.change.undoes, Some(first.change.id));
    }

    #[test]
    fn the_gardener_cites_a_source_and_only_adds_to_hand_written_pages() {
        let t = temp_store("guard");
        let s = &t.store;
        let mine =
            s.create_page(draft("Deploy checklist", "1. Run checks."), crate::HUMAN_AUTHOR, None, crate::OnSimilar::Refuse).expect("page");
        assert!(mine.human_edited);
        let flag = s.flag_knowledge(NewFlag { page_id: Some(mine.id), ..missing("Missing the signing step") }, "Codex").expect("flag");

        let unsourced = s.curate(
            Curation::Create { draft: draft("Something new", "Body."), project_id: None },
            GARDENER,
            &Provenance { reason: "Seemed useful".into(), ..Provenance::default() },
        );
        assert!(unsourced.is_err(), "no flag or update, no edit");

        let rewrite = s.curate(
            Curation::Revise { page_id: mine.id, draft: draft("Deploy checklist", "1. Sign.") },
            GARDENER,
            &cites("Add signing", &[flag.id]),
        );
        assert!(rewrite.is_err(), "a hand-written page isn't rewritten");
        let archive = s.curate(Curation::Archive { page_id: mine.id }, GARDENER, &cites("Old", &[flag.id]));
        assert!(archive.is_err(), "a hand-written page isn't archived");

        let added = s
            .curate(
                Curation::Revise { page_id: mine.id, draft: draft("Deploy checklist", "1. Run checks.\n2. Sign the build.") },
                GARDENER,
                &cites("Add signing", &[flag.id]),
            )
            .expect("appending is allowed");
        assert!(added.page.human_edited, "the page stays marked as hand-written");

        // The person can still do anything by hand.
        s.curate(Curation::Archive { page_id: mine.id }, crate::HUMAN_AUTHOR, &Provenance::default()).expect("person archives");
    }

    #[test]
    fn curator_links_current_local_and_shared_notes_with_provenance() {
        use crate::{HUMAN_AUTHOR, OnSimilar, gardener::Backend};

        let scratch = temp_store("links");
        let store = &scratch.store;
        let alpha = store.project_named("Alpha", None).unwrap();
        let beta = store.project_named("Beta", None).unwrap();
        let page = |title, project_id| {
            store.create_page(draft(title, "Use the signing checklist."), HUMAN_AUTHOR, project_id, OnSimilar::Allow).unwrap()
        };
        let local = page("Signing Alpha", Some(alpha.id));
        let shared = page("Signing checklist", None);
        let other = page("Signing Beta", Some(beta.id));
        let archived = page("Signing old builds", Some(alpha.id));
        store.set_page_standing(archived.id, Standing::Archived).unwrap();
        let flag =
            store.flag_knowledge(NewFlag { project_id: Some(alpha.id), ..missing("Missing the signing workflow") }, "Codex").unwrap();
        let run = store.start_scoped_gardener_run(Backend::Codex, Scope::Project(alpha.id)).unwrap();
        let provenance = cites("The flag shows a missing workflow.", &[flag.id]);
        let link = |id| format!("[Signing checklist](feeder://notebook/{id})");
        let write = |body: &str| {
            store.curate_gardener_run(
                run,
                Curation::Create { draft: draft("Release signing workflow", body), project_id: None },
                &provenance,
            )
        };
        for id in [other.id, archived.id, i64::MAX] {
            assert!(write(&link(id)).is_err());
        }
        assert_eq!(store.flag(flag.id).unwrap().unwrap().status, FlagStatus::Open);
        let result = write(&format!("Read {} and {} before you sign.", link(local.id), link(shared.id))).unwrap();
        assert_eq!(result.change.source_flags, [flag.id]);
        assert!(result.page.body.contains(&link(shared.id)));
        store.revert_change(result.change.id, HUMAN_AUTHOR).unwrap();
        assert_eq!(store.flag(flag.id).unwrap().unwrap().status, FlagStatus::Open);
    }

    #[test]
    fn a_follow_up_edit_may_cite_the_flag_its_page_already_resolved() {
        let t = temp_store("follow-up");
        let s = &t.store;
        let flag = s.flag_knowledge(missing("Nothing on appcast signing"), "Codex").expect("flag");
        let made = s
            .curate(
                Curation::Create { draft: draft("How the appcast is signed", "Signed with an EdDSA key. Extra tip."), project_id: None },
                GARDENER,
                &cites("From the person", &[flag.id]),
            )
            .expect("creates");
        let trimmed = s
            .curate(
                Curation::Revise { page_id: made.page.id, draft: draft("How the appcast is signed", "Signed with an EdDSA key.") },
                GARDENER,
                &cites("Trim the unsupported tip", &[flag.id]),
            )
            .expect("a closed flag still counts as the source");
        assert_eq!(trimmed.page.body, "Signed with an EdDSA key.");
        assert_eq!(
            s.flag(flag.id).expect("read").expect("exists").change_id,
            Some(made.change.id),
            "the flag keeps the change that resolved it"
        );
        let ghost = s.curate(
            Curation::Revise { page_id: made.page.id, draft: draft("How the appcast is signed", "x y z") },
            GARDENER,
            &cites("Made up", &[999]),
        );
        assert!(matches!(ghost, Err(Error::NotFound(_))), "a flag that doesn't exist isn't a source");
    }

    #[test]
    fn flag_kinds_parse_from_their_names_and_synonyms() {
        for kind in FlagKind::ALL {
            assert_eq!(kind.as_str().parse::<FlagKind>().expect("round-trips"), kind);
        }
        assert_eq!("Stale".parse::<FlagKind>().expect("synonym"), FlagKind::Outdated);
        assert!("bogus".parse::<FlagKind>().is_err());
    }
}
