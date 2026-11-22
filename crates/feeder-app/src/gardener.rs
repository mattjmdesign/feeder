//! The Notebook's Curator page: a focused interface for reviewing open flags and running the
//! Curator agent to update Notebook pages. Built with native `rune_kit` components (`Card`,
//! `Collapsible`, `TextInput`, `Spinner`, `Badge`, `Button`, `Divider`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use feeder_core::gardener::{
    Backend, ChatOpening, ChatSummary, Event, OPENING, RunStatus, Scope, Session, SessionEnv, SessionMode, StoredEvent, ToolCall,
};
use feeder_core::{Change, Flag, ago};
use rune::{Cursor, El, Key, KeyEvent, TextEdit, Tx, col, el, icon, row, text};
use rune_kit::diff::{FileDiff, LineKind};
use rune_kit::shell::{Sidebar, SidebarRow, SidebarSection};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, Card, ControlSize, Divider, Kit, Placement, Select, Spinner, TextArea};

use crate::State;
use crate::style::{self, Palette};
use crate::watch::NotebookOp;

/// How far a list row's hover surface reaches past the text column, matching the Notebook index.
const ROW_BLEED: f32 = 10.0;
/// The trailing action column, so Revert, Reverted, and Dismiss align down the page.
const ACTION_W: f32 = 84.0;
/// Small ghost button inline padding.
const BUTTON_PAD: f32 = 10.0;
/// Maximum lines of tool output shown inside an expanded step.
const TOOL_OUTPUT_LINES: usize = 24;
/// Fold consecutive finished read/search steps once there are more than this many.
const FOLD_AFTER: usize = 2;
/// Reading width for the conversation, about 75 characters of body text (same cap as Home's preview row).
const CHAT_MAX_W: f32 = 640.0;
/// The person's bubble stops at three quarters of the reading width.
const USER_BUBBLE_MAX_W: f32 = 480.0;

/// Curator's jobs that don't need typing. Asking a question is the composer itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// Turn open flags and Feed replies into Notebook edits.
    ReviewFlags,
    /// Find overlapping notes so the person can decide what to keep.
    FindDuplicates,
}

impl Job {
    fn label(self) -> &'static str {
        match self {
            Job::ReviewFlags => "Review Open Flags",
            Job::FindDuplicates => "Find Duplicate Notes",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Job::ReviewFlags => OPENING,
            Job::FindDuplicates => {
                "Look for overlapping Notebook pages. Link the pages and suggest what to keep or combine. Say which notes you checked."
            }
        }
    }
}

/// A saved chat's run id and transcript.
type SavedChat = (i64, Vec<StoredEvent>);

/// Where the current Curator run is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// No active run on this page.
    #[default]
    Idle,
    /// Curator is running; the user can stop it.
    Working,
    /// Curator finished a turn; the user can send a follow-up.
    Ready,
    /// The run ended; submitting starts a new one.
    Ended,
}

/// One entry in the session transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Only shown when the user typed a custom prompt or follow-up.
    You(String),
    Thinking {
        text: String,
        open: bool,
    },
    Reply(String),
    Tool {
        call: ToolCall,
        result: Option<ToolResult>,
        open: bool,
    },
    SignIn(Backend),
    Failed(String),
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub output: String,
    pub failed: bool,
}

/// Session state: transcript, composer, active scope, and running CLI handle.
pub struct Chat {
    pub items: Vec<Item>,
    pub input: TextEdit,
    pub phase: Phase,
    pub backend: Option<Backend>,
    pub scope: Scope,
    /// Ask answers from the Notebook without editing; Review may make cited edits.
    pub mode: SessionMode,
    /// The saved chat on screen, read-only; typing starts a new chat.
    pub viewing: Option<i64>,
    /// A saved transcript loaded off the UI thread, waiting to be shown.
    saved: Arc<Mutex<Option<SavedChat>>>,
    /// Folded groups of finished reads that the user expanded, keyed by first item index.
    pub expanded: Vec<usize>,
    session: Arc<Mutex<Option<Session>>>,
    inbox: Arc<Mutex<Vec<Event>>>,
    stop_requested: Arc<AtomicBool>,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            input: TextEdit::default(),
            phase: Phase::default(),
            backend: None,
            scope: Scope::Notebook,
            mode: SessionMode::Ask,
            viewing: None,
            saved: Arc::new(Mutex::new(None)),
            expanded: Vec::new(),
            session: Arc::new(Mutex::new(None)),
            inbox: Arc::new(Mutex::new(Vec::new())),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Chat {
    fn draft(&self) -> Option<(SessionMode, &str)> {
        let text = self.input.text.trim();
        (!text.is_empty()).then_some((SessionMode::Ask, text))
    }

    /// Moves events from the background CLI thread into the transcript.
    pub fn drain(&mut self) -> bool {
        let saved = self.saved.lock().unwrap_or_else(|e| e.into_inner()).take();
        let reopened = saved.is_some();
        if let Some((run_id, stored)) = saved
            && self.viewing == Some(run_id)
        {
            show_saved(self, stored);
        }
        let events: Vec<Event> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        let changed = reopened || !events.is_empty();
        for event in events {
            let ended = matches!(event, Event::Ended);
            apply(&mut self.items, &mut self.phase, self.backend, event);
            if ended {
                let session = Arc::clone(&self.session);
                std::thread::spawn(move || drop(session.lock().unwrap_or_else(|e| e.into_inner()).take()));
            }
        }
        changed
    }

    fn session_send(&self, message: String) {
        let session = Arc::clone(&self.session);
        let inbox = Arc::clone(&self.inbox);
        std::thread::spawn(move || {
            let sent = session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.send(&message));
            if let Some(Err(e)) = sent {
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(Event::Failed(e.to_string()));
            }
        });
    }
}

/// Lays a saved transcript out as a finished, read-only chat.
fn show_saved(chat: &mut Chat, stored: Vec<StoredEvent>) {
    chat.items.clear();
    chat.expanded.clear();
    chat.phase = Phase::Working;
    for entry in stored {
        match entry {
            StoredEvent::You(text) => chat.items.push(Item::You(text)),
            StoredEvent::Event(Event::Ended) => {}
            StoredEvent::Event(event) => apply(&mut chat.items, &mut chat.phase, chat.backend, event),
        }
    }
    // A saved chat can't take follow-ups, so it always reads as finished.
    chat.phase = Phase::Ended;
}

/// Folds one CLI event into the transcript.
pub fn apply(items: &mut Vec<Item>, phase: &mut Phase, backend: Option<Backend>, event: Event) {
    match event {
        Event::Started => *phase = Phase::Working,
        Event::Thinking(piece) => match items.last_mut() {
            Some(Item::Thinking { text, .. }) => text.push_str(&piece),
            _ => items.push(Item::Thinking { text: piece, open: false }),
        },
        Event::Text(piece) => match items.last_mut() {
            Some(Item::Reply(text)) => text.push_str(&piece),
            _ if piece.trim().is_empty() => {}
            _ => items.push(Item::Reply(piece)),
        },
        Event::ToolStarted(call) => items.push(Item::Tool { call, result: None, open: false }),
        Event::ToolFinished { id, output, failed } => {
            let pending = items.iter_mut().rev().find_map(|item| match item {
                Item::Tool { call, result, .. } if call.id == id && result.is_none() => Some(result),
                _ => None,
            });
            if let Some(result) = pending {
                *result = Some(ToolResult { output, failed });
            }
        }
        Event::TurnFinished => *phase = Phase::Ready,
        Event::NeedsSignIn => {
            if let Some(b) = backend {
                items.push(Item::SignIn(b));
            }
        }
        Event::Failed(why) => items.push(Item::Failed(why)),
        Event::Ended => {
            *phase = Phase::Ended;
            items.push(Item::Ended);
        }
    }
}

impl State {
    fn gardener_send_op(&self, op: NotebookOp) {
        if let Some(w) = &self.watcher {
            w.notebook(op);
        }
    }

    /// Sends a follow-up or starts a new Curator run.
    fn gardener_submit(&mut self) {
        let Some((mode, typed)) = self.notebook.chat.draft() else { return };
        let typed = typed.to_string();
        match self.notebook.chat.phase {
            Phase::Working => {}
            Phase::Ready => {
                let chat = &mut self.notebook.chat;
                chat.input = TextEdit::new("");
                chat.items.push(Item::You(typed.clone()));
                chat.phase = Phase::Working;
                chat.session_send(typed);
            }
            Phase::Idle | Phase::Ended => self.gardener_start(self.notebook.chat.scope, mode, typed, None),
        }
    }

    /// Starts a run for one of Curator's preset jobs.
    fn gardener_job(&mut self, job: Job) {
        if matches!(self.notebook.chat.phase, Phase::Idle | Phase::Ended) {
            let scope = self.notebook.chat.scope;
            let mode = match job {
                Job::ReviewFlags => SessionMode::Review,
                Job::FindDuplicates => SessionMode::Ask,
            };
            self.gardener_start(scope, mode, job.message().to_string(), Some(job.label()));
        }
    }

    /// Shows a saved chat read-only, loading its transcript off the UI thread.
    fn gardener_open_chat(&mut self, summary: &ChatSummary) {
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        let chat = &mut self.notebook.chat;
        *chat = Chat::default();
        chat.viewing = Some(summary.run_id);
        chat.mode = summary.mode;
        chat.scope = summary.scope;
        chat.phase = Phase::Ended;
        let (run_id, saved, wake) = (summary.run_id, Arc::clone(&chat.saved), self.wake.clone());
        std::thread::spawn(move || {
            let stored = feeder_core::Store::open_default().and_then(|s| s.curator_chat_events(run_id)).unwrap_or_default();
            *saved.lock().unwrap_or_else(|e| e.into_inner()) = Some((run_id, stored));
            if let Some(wake) = wake {
                wake();
            }
        });
    }

    fn gardener_delete_chat(&mut self, run_id: i64) {
        self.gardener_send_op(NotebookOp::DeleteChat(run_id));
        self.notebook.gardener.chats.retain(|c| c.run_id != run_id);
        if self.notebook.chat.viewing == Some(run_id) {
            self.notebook.chat = Chat::default();
        }
    }

    /// Carries a Notebook search with no matches over to Curator as a question.
    pub fn ask_curator(&mut self, question: String) {
        self.notebook.query = TextEdit::new("");
        self.notebook_shelf(crate::notebook::Shelf::Gardener);
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        if self.notebook.chat.phase != Phase::Idle {
            self.gardener_close();
        }
        let input = &mut self.notebook.chat.input;
        *input = TextEdit::new(question);
        input.focused = true;
    }

    /// Opens the Curator page pre-scoped to `project_id` so the user can review scope before starting.
    pub fn start_project_curator(&mut self, project_id: i64) {
        self.open = None;
        self.tab = crate::Tab::Notebook;
        self.notebook_shelf(crate::notebook::Shelf::Gardener);
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        if matches!(self.notebook.chat.phase, Phase::Ready | Phase::Ended) && self.notebook.chat.scope != Scope::Project(project_id) {
            self.gardener_close();
        }
        self.notebook.chat.scope = Scope::Project(project_id);
    }

    /// Starts a chat. `job` is a preset's label: its prompt stays out of the transcript.
    fn gardener_start(&mut self, scope: Scope, mode: SessionMode, message: String, job: Option<&'static str>) {
        let Some(backend) = selected_backend(self) else { return };
        let Some(wake) = self.wake.clone() else { return };
        if self.notebook.gardener.running.is_some() {
            return;
        }
        let chat = &mut self.notebook.chat;
        *chat = Chat::default();
        if job.is_none() {
            chat.items.push(Item::You(message.clone()));
        }
        chat.phase = Phase::Working;
        chat.backend = Some(backend);
        chat.scope = scope;
        chat.mode = mode;
        let (slot, inbox) = (Arc::clone(&chat.session), Arc::clone(&chat.inbox));
        let stop_requested = Arc::clone(&chat.stop_requested);
        std::thread::spawn(move || {
            let sink = {
                let (inbox, wake) = (Arc::clone(&inbox), wake.clone());
                move |event: Event| {
                    inbox.lock().unwrap_or_else(|e| e.into_inner()).push(event);
                    wake();
                }
            };
            let opening = match job {
                Some(title) => ChatOpening::Job { title, prompt: &message },
                None => ChatOpening::Message(&message),
            };
            match Session::start_curator_chat(backend, SessionEnv::detect(), scope, mode, opening, sink) {
                Ok(session) => {
                    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if stop_requested.load(Ordering::Acquire) {
                        session.stop();
                    } else {
                        *slot = Some(session);
                    }
                }
                Err(e) => {
                    let mut pending = inbox.lock().unwrap_or_else(|e| e.into_inner());
                    pending.push(Event::Failed(e.to_string()));
                    pending.push(Event::Ended);
                    drop(pending);
                    wake();
                }
            }
        });
    }

    fn gardener_stop(&mut self) {
        let chat = &self.notebook.chat;
        chat.stop_requested.store(true, Ordering::Release);
        let session = Arc::clone(&chat.session);
        std::thread::spawn(move || {
            if let Some(s) = session.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                s.stop();
            }
        });
    }

    /// Clears a finished transcript and returns to the idle view.
    fn gardener_close(&mut self) {
        self.gardener_stop();
        self.notebook.chat = Chat::default();
    }
}

/// The provider a new chat runs on: the person's pick if it's still installed, else the first found.
pub fn selected_backend(s: &State) -> Option<Backend> {
    let installed = &s.notebook.gardener.backends;
    s.notebook.curator_backend.filter(|b| installed.contains(b)).or_else(|| installed.first().copied())
}

/// The provider picker beside the send button. Locked to the running chat's provider mid-chat.
fn provider_picker(s: &State, k: &Kit, layer: &mut Vec<El>) -> Option<El> {
    let installed = &s.notebook.gardener.backends;
    let chat = &s.notebook.chat;
    let locked = matches!(chat.phase, Phase::Working | Phase::Ready);
    let current = if locked { chat.backend } else { selected_backend(s) }?;
    let labels: Vec<&str> = if locked { vec![current.label()] } else { installed.iter().map(|b| b.label()).collect() };
    let choices = installed.clone();
    Some(
        Select::new("gardener-provider", &labels)
            .label("Provider")
            .current(Some(current.label()))
            .open(s.notebook.provider_open && !locked)
            .placement(Placement::TopEnd)
            .on_toggle(|s: &mut State| s.notebook.provider_open = !s.notebook.provider_open)
            .on_close(|s: &mut State| s.notebook.provider_open = false)
            .on_choose(move |s: &mut State, chosen: &str, _| {
                s.notebook.curator_backend = choices.iter().copied().find(|b| b.label() == chosen);
                s.notebook.provider_open = false;
            })
            .build(k, layer),
    )
}

/// Current live status text while Curator is working.
fn working_status(chat: &Chat) -> String {
    match chat.items.iter().rev().find(|i| !matches!(i, Item::You(_))) {
        Some(Item::Tool { call, result: None, .. }) => live_label(call),
        Some(Item::Thinking { .. } | Item::Tool { .. } | Item::Reply(_)) => "Thinking…".into(),
        _ => "Starting…".into(),
    }
}

/// The page's scrolling content.
pub fn view(s: &State, p: &Palette, k: &Kit, max_w: f32, reduced_motion: bool) -> El {
    let mut page = col().key("gardener").w_full().max_w(max_w.min(CHAT_MAX_W));
    if let Some(bar) = top_bar(s, p, k) {
        page = page.child(bar);
    }
    if is_fresh(s) { page.child(idle_view(s, p, k)) } else { page.child(transcript(s, p, k, reduced_motion)) }
}

/// No conversation on screen yet.
fn is_fresh(s: &State) -> bool {
    let chat = &s.notebook.chat;
    chat.items.is_empty() && chat.phase == Phase::Idle
}

/// Compact bar shown only when there is an active control, scope chip, or status to display.
fn top_bar(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    let chat = &s.notebook.chat;
    let mut bar = row().w_full().center_y().gap(8.0);
    let mut has_bar = false;

    if let Scope::Project(id) = chat.scope
        && let Some(sum) = s.summary(id)
    {
        bar = bar.child(Badge::new(sum.project.name.clone()).variant(BadgeVariant::Subtle).build(k));
        if chat.phase == Phase::Idle {
            bar = bar.child(
                Button::new("All projects")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Small)
                    .on_press(|s: &mut State| s.notebook.chat.scope = Scope::Notebook)
                    .build(k)
                    .key("gardener-clear-scope"),
            );
        }
        has_bar = true;
    }

    let status = status_line(s, p);
    if let Some(sub) = status {
        bar = bar.child(sub);
        has_bar = true;
    }

    let action = match (chat.phase, chat.viewing) {
        (_, Some(run_id)) => Some(
            Button::new("Delete")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut State| s.gardener_delete_chat(run_id))
                .build(k)
                .key("gardener-delete-chat"),
        ),
        _ => None,
    };
    if let Some(action) = action {
        bar = bar.child(rune::spacer()).child(action);
        has_bar = true;
    }

    has_bar.then(|| col().key("gardener-header").w_full().pb(12.0).child(bar))
}

/// Only renders when there is something active, failed, or missing a CLI.
fn status_line(s: &State, p: &Palette) -> Option<El> {
    let g = &s.notebook.gardener;
    let chat = &s.notebook.chat;
    if chat.phase != Phase::Idle {
        return None;
    }
    let backend = chat.backend.or_else(|| g.backends.first().copied());
    let line = match (backend, &g.running, &g.last_run) {
        (None, _, _) => "Install and sign in to Claude Code or Codex to run Curator.".to_string(),
        (_, Some(run), _) => format!("Active run started {}", ago(run.started_at, s.now)),
        (_, None, Some(run)) if run.status == RunStatus::Failed => {
            format!("Last run failed {}: {}", ago(run.started_at, s.now), run.error.clone().unwrap_or_default())
        }
        (_, None, _) => return None,
    };
    Some(text(line).font(style::META).color(p.text_3).wrap())
}

/// Pinned composer using native `rune_kit::TextInput` and `Button`.
pub fn composer(s: &State, _p: &Palette, k: &Kit, max_w: f32, layer: &mut Vec<El>) -> El {
    let chat = &s.notebook.chat;
    // A saved thread is a record; New Chat in the sidebar starts the next conversation.
    if chat.viewing.is_some() {
        return el();
    }
    let backend = chat.backend.or_else(|| selected_backend(s));
    let placeholder = match chat.phase {
        Phase::Working => "Working…",
        Phase::Ready => "Ask a follow-up…",
        Phase::Idle | Phase::Ended => "Ask Curator…",
    };
    let input = TextArea::new(&chat.input)
        .placeholder(placeholder)
        .label("Message Curator")
        .auto_grow(1, 8)
        .width(None)
        .disabled(backend.is_none() || chat.phase == Phase::Working)
        .on_edit(|s: &mut State| &mut s.notebook.chat.input)
        .on_submit(|s: &mut State, _tx: &mut Tx| s.gardener_submit())
        .on_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| match e.key {
            // Enter sends and Shift-Enter starts a new line, as in every AI chat.
            Key::Enter if !e.mods.shift => {
                s.gardener_submit();
                true
            }
            Key::Escape if s.notebook.chat.phase == Phase::Working => {
                s.gardener_stop();
                true
            }
            _ => false,
        })
        .build(k)
        .w_full()
        .key("gardener-input");

    let draft = chat.draft();
    let send_label = match (chat.phase, draft.map(|(mode, _)| mode)) {
        (Phase::Idle | Phase::Ended, Some(SessionMode::Review)) => "Update Note",
        (Phase::Idle | Phase::Ended, _) => "Ask",
        _ => "Send",
    };
    let send = if chat.phase == Phase::Working {
        // Stop takes Send's place while Curator works.
        Button::new("Stop").variant(ButtonVariant::Outline).on_press(|s: &mut State| s.gardener_stop()).build(k).key("gardener-stop")
    } else {
        Button::new(send_label)
            .variant(ButtonVariant::Submit)
            .disabled(
                backend.is_none()
                    || s.notebook.gardener.running.is_some() && matches!(chat.phase, Phase::Idle | Phase::Ended)
                    || chat.phase == Phase::Working
                    || draft.is_none(),
            )
            .on_press(|s: &mut State| s.gardener_submit())
            .build(k)
            .key("gardener-send")
    };

    // One row under the box: Curator's jobs on the left before a chat starts, provider and send on the right.
    let mut actions = row().w_full().gap(8.0).center_y();
    if is_fresh(s) {
        actions = actions.child(jobs(s, k).grow(1.0).min_w(0.0));
    } else {
        actions = actions.child(rune::spacer());
    }
    if let Some(picker) = provider_picker(s, k, layer) {
        actions = actions.child(picker);
    }
    let bar = col().w_full().gap(8.0).child(input).child(actions.child(send));
    let stack = col().w_full().max_w(max_w.min(CHAT_MAX_W)).child(bar);
    col().w_full().items_center().px(40.0).pt(8.0).pb(12.0).child(stack)
}

/// Curator's preset jobs, shown above the composer before a chat starts.
fn jobs(s: &State, k: &Kit) -> El {
    let g = &s.notebook.gardener;
    let ready = !g.backends.is_empty() && g.running.is_none();
    let has_flags = !scoped_flags(s).is_empty();
    let mut jobs = row().gap(8.0).wrap();
    for job in [Job::ReviewFlags, Job::FindDuplicates] {
        if job == Job::ReviewFlags && !has_flags {
            continue;
        }
        jobs = jobs.child(
            Button::new(job.label())
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .disabled(!ready)
                .on_press(move |s: &mut State| s.gardener_job(job))
                .build(k)
                .key(format!("gardener-job-{job:?}")),
        );
    }
    jobs
}

/// Open flags inside the chat's scope.
fn scoped_flags(s: &State) -> Vec<&Flag> {
    let scope = s.notebook.chat.scope;
    let page_project = |id| s.notebook.page(id).and_then(|page| page.project_id);
    s.notebook
        .gardener
        .open_flags
        .iter()
        .filter(|flag| {
            let page_project_id = flag.page_id.and_then(page_project);
            scope.includes(flag.project_id.or(page_project_id)) && page_project_id.is_none_or(|id| scope.includes(Some(id)))
        })
        .collect()
}

/// Whether an item is a finished read/search step that folds into a collapsible group.
fn quiet_step(item: &Item) -> bool {
    matches!(item, Item::Tool { call, result: Some(r), .. } if !r.failed && !is_edit(&call.name))
}

fn is_edit(tool: &str) -> bool {
    matches!(tool, "write_page" | "archive_page")
}

fn transcript(s: &State, p: &Palette, k: &Kit, reduced_motion: bool) -> El {
    let chat = &s.notebook.chat;
    let mut list = col().key("gardener-transcript").w_full().pt(14.0).gap(10.0);
    let mut i = 0;
    while i < chat.items.len() {
        let run = chat.items[i..].iter().take_while(|it| quiet_step(it)).count();
        if run > FOLD_AFTER {
            let open = chat.expanded.contains(&i);
            list = list.child(fold_group(s, i, run, open, p, k));
            i += run;
            continue;
        }
        // A new turn gets more room than the steps inside one.
        let turn = i > 0 && matches!(chat.items[i], Item::You(_));
        // The reply still streaming in renders live; finished replies use the full article renderer.
        let live = chat.phase == Phase::Working && i + 1 == chat.items.len();
        let view = match &chat.items[i] {
            Item::Reply(reply) if live => {
                crate::notebook::render_live_markdown(reply, reduced_motion, p, k).key(format!("gardener-reply-{i}"))
            }
            _ => item_view(s, i, p, k),
        };
        list = list.child(view.mt(if turn { 18.0 } else { 0.0 }));
        i += 1;
    }
    // While working, the live status sits where the reply will appear, unless a live step already shows it.
    if chat.phase == Phase::Working && !matches!(chat.items.last(), Some(Item::Tool { result: None, .. })) {
        list = list.child(
            row()
                .key("gardener-working")
                .gap(8.0)
                .center_y()
                .py(4.0)
                .child(Spinner::new().size(13.0).color(p.text_2).build(k))
                .child(text(working_status(chat)).font(style::META).color(p.text_2)),
        );
    }
    list
}

fn item_view(s: &State, i: usize, p: &Palette, k: &Kit) -> El {
    let chat = &s.notebook.chat;
    match &chat.items[i] {
        Item::You(message) => user_prompt(message, p),
        Item::Thinking { text: thought, open } => thinking(i, thought, *open, p, k),
        Item::Reply(reply) => crate::notebook::render_article_markdown(reply, None, p, k).key(format!("gardener-reply-{i}")),
        Item::Tool { call, result: Some(r), .. } if is_edit(&call.name) && !r.failed => edit_card(s, i, call, r, p, k),
        Item::Tool { call, result, open } => tool_step(s, i, call, result.as_ref(), *open, p, k),
        Item::SignIn(backend) => Card::new()
            .accent(p.amber)
            .title(format!("{} isn't signed in", backend.label()), None)
            .description(format!("Run `{}` in Terminal, sign in, then press Start again.", backend.sign_in_command()))
            .build(k)
            .key(format!("gardener-signin-{i}")),
        Item::Failed(why) => {
            Card::new().accent(p.amber).title("Run stopped", None).description(why.clone()).build(k).key(format!("gardener-failed-{i}"))
        }
        Item::Ended => text("Run ended").font(style::META).color(p.text_3).pt(2.0),
    }
}

/// Compact collapsible group of finished read/search steps using `rune_kit::Collapsible`.
fn fold_group(s: &State, start: usize, run: usize, open: bool, p: &Palette, k: &Kit) -> El {
    let labels: Vec<String> = s.notebook.chat.items[start..start + run]
        .iter()
        .filter_map(|it| match it {
            Item::Tool { call, .. } => Some(short_label(call)),
            _ => None,
        })
        .take(3)
        .collect();
    let more = if run > labels.len() { ", …" } else { "" };
    let title = format!("Checked {run} sources · {}{more}", labels.join(", "));
    let toggle = disclosure(format!("gardener-fold-{start}"), title, open, p, k, move |s: &mut State| {
        let expanded = &mut s.notebook.chat.expanded;
        match expanded.iter().position(|&x| x == start) {
            Some(at) => {
                expanded.remove(at);
            }
            None => expanded.push(start),
        }
    });
    let mut group = col().w_full().child(toggle);
    if open {
        let mut inner = col().w_full();
        for j in start..start + run {
            inner = inner.child(item_view(s, j, p, k));
        }
        group = group.child(disclosed(inner));
    }
    group
}

fn short_label(call: &ToolCall) -> String {
    match call.name.as_str() {
        "list_flags" => "flags".into(),
        "read_traces" => "updates".into(),
        "search_notebook" => format!("“{}”", call.text_arg("query").unwrap_or("")),
        "read_page" => call.int_arg("id").map_or_else(|| "a page".into(), |id| format!("page #{id}")),
        other => other.replace('_', " "),
    }
}

/// User-typed follow-up prompt: quiet subtle surface, left-aligned with the stream.
/// The person's message: a right-aligned bubble, as in Claude and ChatGPT, so turns read at a glance.
fn user_prompt(message: &str, p: &Palette) -> El {
    row().w_full().justify_end().child(
        col()
            .max_w(USER_BUBBLE_MAX_W)
            .px(14.0)
            .py(10.0)
            .radius(12.0)
            .fill(p.hover)
            .child(text(message.to_string()).font(style::BODY).color(p.text).wrap().selectable(true)),
    )
}

fn thinking(i: usize, thought: &str, open: bool, p: &Palette, k: &Kit) -> El {
    let toggle = disclosure(format!("gardener-thinking-{i}"), "Reasoning".into(), open, p, k, move |s: &mut State| {
        if let Some(Item::Thinking { open, .. }) = s.notebook.chat.items.get_mut(i) {
            *open = !*open;
        }
    });
    let mut block = col().w_full().child(toggle);
    if open {
        block = block.child(disclosed(text(thought.trim().to_string()).font(style::META).color(p.text_3).wrap().selectable(true)));
    }
    block
}

fn tool_label(s: &State, call: &ToolCall) -> String {
    let arg = |key: &str| call.text_arg(key).unwrap_or("").to_string();
    let id = call.int_arg("id");
    let page_title = || match id {
        Some(id) => s.notebook.page(id).map_or_else(|| format!("page #{id}"), |pg| format!("“{}”", pg.title)),
        None => "a page".to_string(),
    };
    match call.name.as_str() {
        "list_flags" => "Read open flags".into(),
        "read_traces" => "Read recent Feed updates".into(),
        "search_notebook" if arg("query").trim().is_empty() => "Searched Notebook".into(),
        "search_notebook" => format!("Searched “{}”", arg("query")),
        "read_page" => format!("Read {}", page_title()),
        "write_page" if id.is_some() => format!("Revised “{}”", arg("title")),
        "write_page" => format!("Wrote “{}”", arg("title")),
        "archive_page" => format!("Archived {}", page_title()),
        "dismiss_flag" => format!("Dismissed flag #{}", id.unwrap_or_default()),
        other => other.replace('_', " "),
    }
}

fn live_label(call: &ToolCall) -> String {
    let query = call.text_arg("query").map(str::trim).filter(|q| !q.is_empty());
    match (call.name.as_str(), query) {
        ("search_notebook", Some(q)) => format!("Searching “{q}”…"),
        ("search_notebook", None) => "Searching Notebook…".into(),
        ("list_flags", _) => "Reading open flags…".into(),
        ("read_traces", _) => "Reading recent Feed updates…".into(),
        ("read_page", _) => "Reading page…".into(),
        ("write_page", _) => match call.text_arg("title") {
            Some(t) => format!("Writing “{t}”…"),
            None => "Writing page…".into(),
        },
        ("archive_page", _) => "Archiving page…".into(),
        ("dismiss_flag", _) => "Dismissing flag…".into(),
        (other, _) => format!("{}…", other.replace('_', " ")),
    }
}

fn number_after(output: &str, keys: &[&str]) -> Option<i64> {
    let rest = keys.iter().find_map(|key| output.split_once(key).map(|(_, rest)| rest))?;
    rest.trim_start().chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

fn change_id(output: &str) -> Option<i64> {
    number_after(output, &["\"change_id\":", "Change #"])
}

fn edited_page(output: &str) -> Option<i64> {
    number_after(output, &["\"page\":{\"id\":", "page #"])
}

/// An edited Notebook page: what happened, the page title, a short excerpt, then Open and Revert.
fn edit_card(s: &State, i: usize, call: &ToolCall, result: &ToolResult, p: &Palette, k: &Kit) -> El {
    let page = edited_page(&result.output).or_else(|| call.int_arg("id"));
    let (verb, fallback) = match call.name.as_str() {
        "archive_page" => ("Archived", String::new()),
        _ if call.int_arg("id").is_some() => ("Updated", call.text_arg("title").unwrap_or("").to_string()),
        _ => ("New page", call.text_arg("title").unwrap_or("").to_string()),
    };
    let found = page.and_then(|id| s.notebook.page(id));
    let title = found.map_or(fallback, |pg| pg.title.clone());
    let mut card = col()
        .key(format!("gardener-edit-{i}"))
        .role("group")
        .label(format!("{verb}: {title}"))
        .w_full()
        .gap(6.0)
        .px(12.0)
        .py(10.0)
        .radius(8.0)
        .border(1.0, p.hairline)
        .child(text(verb).font(style::META).color(p.text_3))
        .child(text(title).font(style::NAME).color(p.text).wrap());
    // What actually changed, the way a pull request shows it: the previous text for a revision,
    // nothing for a new page, so every edit is reviewable without leaving the chat.
    let changes = &s.notebook.gardener.changes;
    let change = match change_id(&result.output) {
        Some(cid) => changes.iter().find(|c| c.id == cid),
        // Older outputs name only the page: use Curator's latest change to it.
        None => page.and_then(|id| changes.iter().find(|c| c.page_id == id && c.undoes.is_none())),
    };
    let before = change.and_then(|c| c.before.as_ref()).map(|b| b.body.as_str());
    match found.filter(|pg| !pg.is_archived()) {
        Some(pg) if verb != "Updated" || before.is_some() => {
            if let Some(diff) = diff_preview(before.unwrap_or(""), &pg.body, p, k) {
                card = card.child(diff);
            }
        }
        Some(pg) if !pg.excerpt(160).is_empty() => {
            card = card.child(text(pg.excerpt(160)).font(style::META).color(p.text_2).wrap().max_lines(2));
        }
        _ => {}
    }
    let mut actions = row().gap(4.0).pt(2.0);
    if let Some(id) = page.filter(|_| found.is_some()) {
        actions = actions.child(
            Button::new("Open")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut State| s.notebook_open_page(id))
                .build(k)
                .key(format!("gardener-open-{i}")),
        );
    }
    if let Some(cid) = change.map(|c| c.id) {
        let reverted = s.notebook.gardener.changes.iter().find(|c| c.id == cid).is_some_and(Change::is_reverted);
        actions = actions.child(if reverted {
            Badge::new("Reverted").variant(BadgeVariant::Subtle).build(k)
        } else {
            Button::new("Revert")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut State| s.gardener_send_op(NotebookOp::Revert(cid)))
                .build(k)
                .key(format!("gardener-revert-{cid}"))
        });
    }
    card.child(actions)
}

/// Changed lines shown in an edit card before "N more".
const DIFF_LINES: usize = 6;

/// A compact inline diff from Rune's diff model: added lines on the success tint, removed lines
/// on the danger tint, unchanged lines left out. `None` when nothing differs.
fn diff_preview(old: &str, new: &str, p: &Palette, k: &Kit) -> Option<El> {
    let diff = FileDiff::from_texts("page.md", old, new);
    let changed: Vec<_> =
        diff.hunks.iter().flat_map(|h| h.lines.iter()).filter(|l| l.kind != LineKind::Context && !l.text.trim().is_empty()).collect();
    if changed.is_empty() {
        return None;
    }
    let mut lines = col().w_full().gap(2.0).label(format!("{} lines added, {} removed", diff.additions(), diff.deletions()));
    for line in changed.iter().take(DIFF_LINES) {
        let (mark, tint) = match line.kind {
            LineKind::Added => ("+", k.c.success),
            _ => ("−", k.c.danger),
        };
        lines = lines.child(
            row()
                .w_full()
                .gap(8.0)
                .px(8.0)
                .py(2.0)
                .radius(4.0)
                .fill(tint.bg)
                .child(text(mark).font(style::PATH).color(tint.fg).a11y_hidden())
                .child(text(line.text.trim_end().to_string()).font(style::META).color(p.text).wrap().shrink().min_w(0.0)),
        );
    }
    if changed.len() > DIFF_LINES {
        lines = lines.child(text(format!("{} more lines", changed.len() - DIFF_LINES)).font(style::META).color(p.text_3).pl(8.0));
    }
    Some(lines)
}

/// One tool step using `rune_kit::Collapsible` (native disclosure & expanded a11y state).
fn tool_step(s: &State, i: usize, call: &ToolCall, result: Option<&ToolResult>, open: bool, p: &Palette, k: &Kit) -> El {
    if result.is_none() {
        return row()
            .key(format!("gardener-tool-{i}"))
            .w_full()
            .gap(8.0)
            .center_y()
            .py(4.0)
            .child(Spinner::new().size(13.0).color(p.text_2).build(k))
            .child(text(live_label(call)).font(style::META).color(p.text_2).truncate().shrink().min_w(0.0));
    }
    let label = match result {
        Some(r) if r.failed => format!("{} · Refused", tool_label(s, call)),
        _ => tool_label(s, call),
    };
    let toggle = disclosure(format!("gardener-tool-{i}"), label, open, p, k, move |s: &mut State| {
        if let Some(Item::Tool { open, .. }) = s.notebook.chat.items.get_mut(i) {
            *open = !*open;
        }
    });
    let mut step = col().w_full().child(toggle);
    if open {
        let output =
            result.map_or_else(|| "Running…".to_string(), |r| r.output.lines().take(TOOL_OUTPUT_LINES).collect::<Vec<_>>().join("\n"));
        step = step.child(disclosed(text(format!("{}\n\n{output}", call.input)).font(style::PATH).color(p.text_2).wrap().selectable(true)));
    }
    step
}

/// A quiet step row: always-visible ▸/▾, small secondary text, so steps never outrank the reply.
fn disclosure(key: String, label: String, open: bool, p: &Palette, k: &Kit, toggle: impl Fn(&mut State) + 'static) -> El {
    let glyph = if open { k.glyph.triangle_down } else { k.glyph.triangle_right };
    row()
        .key(key)
        .role("button")
        .label(label.clone())
        .expanded(open)
        .focusable(true)
        .focus_ring(2.0, 0.0, k.c.border_focus)
        .cursor(Cursor::Pointer)
        .radius(6.0)
        .gap(4.0)
        .py(4.0)
        .center_y()
        .on_press(toggle)
        .child(el().size(14.0, 14.0).center().child(icon(glyph, 12.0).color(p.text_3)).a11y_hidden())
        .child(text(label).font(style::META).color(p.text_2).truncate().shrink().min_w(0.0).a11y_hidden())
}

/// Content under an open step, indented to the step's text.
fn disclosed(body: El) -> El {
    col().w_full().pl(18.0).pb(4.0).child(body)
}

fn action_slot(child: El) -> El {
    row().w(ACTION_W).justify_end().center_y().child(child)
}

/// Between runs: recent Curator edits, each revertible. Nothing when there are none.
fn idle_view(s: &State, p: &Palette, k: &Kit) -> El {
    let scope = s.notebook.chat.scope;
    let changes: Vec<&Change> =
        s.notebook.gardener.changes.iter().filter(|c| scope.includes(s.notebook.page(c.page_id).and_then(|pg| pg.project_id))).collect();
    let mut page = col().w_full();
    if !changes.is_empty() {
        page = page.child(section_title("Recent changes", p)).child(change_list(s, &changes, p, k));
    }
    page
}

/// The run id of the chat on screen, live or saved, so its thread row reads as selected.
fn current_run(chat: &Chat) -> Option<i64> {
    chat.viewing.or_else(|| chat.session.lock().ok().and_then(|g| g.as_ref().map(Session::run_id)))
}

/// Recency bucket for a thread, like Claude and ChatGPT group their history.
fn bucket(started_at: i64, now: i64) -> &'static str {
    match feeder_core::day_key(now) - feeder_core::day_key(started_at) {
        ..=0 => "Today",
        1 => "Yesterday",
        2..=7 => "Previous 7 days",
        _ => "Older",
    }
}

/// Curator's saved chats for the Notebook sidebar, on Rune's `Sidebar`, grouped by recency like
/// Claude's and ChatGPT's history. One Tab stop; Up and Down move through threads.
pub fn thread_list(s: &State, k: &Kit) -> El {
    let scope = s.notebook.chat.scope;
    let selected = current_run(&s.notebook.chat);
    let mut sidebar = Sidebar::new().label("Curator chats");
    let mut section: Option<(&str, Vec<El>)> = None;
    for c in s.notebook.gardener.chats.iter().filter(|c| scope == Scope::Notebook || c.scope == scope) {
        let label = bucket(c.started_at, s.now);
        if section.as_ref().is_some_and(|(l, _)| *l != label)
            && let Some((l, rows)) = section.take()
        {
            sidebar = sidebar.section(SidebarSection::new(l).key(format!("threads-{l}")).rows(rows));
        }
        let summary = c.clone();
        let row_el = SidebarRow::new(c.title.clone())
            .selected(selected == Some(c.run_id))
            .on_select(move |s: &mut State, _tx: &mut Tx| s.gardener_open_chat(&summary))
            .build(k)
            .key(format!("gardener-thread-{}", c.run_id));
        section.get_or_insert_with(|| (label, Vec::new())).1.push(row_el);
    }
    if let Some((l, rows)) = section {
        sidebar = sidebar.section(SidebarSection::new(l).key(format!("threads-{l}")).rows(rows));
    }
    sidebar.build(k)
}

/// The sidebar's primary button while Curator is open.
pub fn new_chat_button(s: &State, k: &Kit) -> El {
    Button::new("New Chat")
        .icon(&crate::notebook::PLUS_ICON)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Regular)
        .disabled(s.notebook.chat.phase == Phase::Working)
        .on_press(|s: &mut State| s.gardener_close())
        .build(k)
        .w_full()
        .key("gardener-new-chat")
}

fn section_title(label: &str, p: &Palette) -> El {
    text(label.to_string()).font(style::NAME).color(p.text).pb(4.0)
}

fn change_list(s: &State, changes: &[&Change], p: &Palette, k: &Kit) -> El {
    let mut list = col().w_full();
    for (i, change) in changes.iter().enumerate() {
        if i > 0 {
            list = list.child(Divider::horizontal().build(k));
        }
        list = list.child(change_row(s, change, p, k));
    }
    list
}

fn change_row(s: &State, c: &Change, p: &Palette, k: &Kit) -> El {
    let (id, page_id) = (c.id, c.page_id);
    let verb = c.action.label();
    let title = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(text(verb).font(style::META).color(p.text_3))
        .child(text(c.page_title.clone()).font(style::NAME).color(p.text).truncate().shrink().min_w(0.0))
        .child(rune::spacer())
        .child(text(ago(c.created_at, s.now)).font(style::META).color(p.text_3));
    let mut words = col().grow(1.0).min_w(0.0).gap(4.0).child(title);
    if let Some(reason) = &c.reason {
        words = words.child(text(reason.clone()).font(style::META).color(p.text_2).wrap().max_lines(3));
    }
    let trailing = if c.is_revertible() {
        Button::new("Revert")
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut State| s.gardener_send_op(NotebookOp::Revert(id)))
            .build(k)
            .key(format!("gardener-revert-{id}"))
    } else {
        text("Reverted").font(style::META).color(p.text_3).pr(BUTTON_PAD)
    };
    row()
        .key(format!("gardener-change-{id}"))
        .w_full()
        .gap(12.0)
        .center_y()
        .mx(-ROW_BLEED)
        .px(ROW_BLEED)
        .py(12.0)
        .radius(8.0)
        .hover(p.hover)
        .role("button")
        .label(format!("{verb} {}", c.page_title))
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.notebook_open_page(page_id))
        .child(words)
        .child(action_slot(trailing))
}

/// Shows a saved chat read-only from recorded events, for snapshot scenes.
pub fn preview_saved(chat: &mut Chat, backend: Backend, run_id: i64, question: &str, events: Vec<Event>) {
    chat.backend = Some(backend);
    chat.viewing = Some(run_id);
    let mut stored = vec![StoredEvent::You(question.to_string())];
    stored.extend(events.into_iter().map(StoredEvent::Event));
    show_saved(chat, stored);
}

/// Feeds a recorded event list through the reducer, for snapshot scenes.
pub fn replay(chat: &mut Chat, backend: Backend, events: Vec<Event>) {
    chat.backend = Some(backend);
    // The recorded session edits pages, so it's a Review session.
    chat.mode = SessionMode::Review;
    for event in events {
        apply(&mut chat.items, &mut chat.phase, Some(backend), event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), input: Default::default() }
    }

    #[test]
    fn events_fold_into_a_readable_transcript() {
        let (mut items, mut phase) = (vec![], Phase::Working);
        let events = vec![
            Event::Started,
            Event::Thinking("Check ".into()),
            Event::Thinking("flags.".into()),
            Event::Text("\n\n".into()),
            Event::Text("Reading ".into()),
            Event::Text("flags.".into()),
            Event::ToolStarted(call("a", "list_flags")),
            Event::ToolStarted(call("b", "read_traces")),
            Event::ToolFinished { id: "b".into(), output: "traces".into(), failed: false },
            Event::ToolFinished { id: "a".into(), output: "flags".into(), failed: false },
            Event::Text("Done.".into()),
            Event::TurnFinished,
        ];
        for e in events {
            apply(&mut items, &mut phase, Some(Backend::Codex), e);
        }
        assert_eq!(phase, Phase::Ready);
        assert_eq!(items[0], Item::Thinking { text: "Check flags.".into(), open: false });
        assert_eq!(items[1], Item::Reply("Reading flags.".into()), "a lone paragraph break doesn't start a reply");
        let outputs: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                Item::Tool { call, result, .. } => Some((call.id.as_str(), result.as_ref().map(|r| r.output.as_str()))),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, [("a", Some("flags")), ("b", Some("traces"))], "results match their calls even out of order");
        assert_eq!(items.last(), Some(&Item::Reply("Done.".into())));
    }

    #[test]
    fn saved_chats_reopen_finished_and_read_only() {
        let mut chat = Chat { viewing: Some(7), ..Chat::default() };
        let stored = vec![
            StoredEvent::You("How do we sign the appcast?".into()),
            StoredEvent::Event(Event::Started),
            StoredEvent::Event(Event::Text("Nothing in the Notebook says.".into())),
            StoredEvent::Event(Event::TurnFinished),
            StoredEvent::Event(Event::Ended),
        ];
        show_saved(&mut chat, stored);
        assert_eq!(chat.phase, Phase::Ended);
        assert_eq!(
            chat.items,
            [Item::You("How do we sign the appcast?".into()), Item::Reply("Nothing in the Notebook says.".into())],
            "no Run ended notice on a saved chat"
        );
    }

    #[test]
    fn diffs_show_only_changed_lines() {
        let p = style::palette(false);
        let k = style::kit(&p);
        assert!(diff_preview("same\n", "same\n", &p, &k).is_none(), "no card diff when nothing changed");
        let diff = FileDiff::from_texts("page.md", "a\nold\nc\n", "a\nnew\nc\nadded\n");
        assert_eq!((diff.additions(), diff.deletions()), (2, 1));
        assert!(diff_preview("a\nold\nc\n", "a\nnew\nc\nadded\n", &p, &k).is_some());
    }

    #[test]
    fn sign_in_and_end_show_as_notices() {
        let (mut items, mut phase) = (vec![], Phase::Working);
        apply(&mut items, &mut phase, Some(Backend::ClaudeCode), Event::NeedsSignIn);
        apply(&mut items, &mut phase, Some(Backend::ClaudeCode), Event::Ended);
        assert_eq!(items, [Item::SignIn(Backend::ClaudeCode), Item::Ended]);
        assert_eq!(phase, Phase::Ended);
    }

    #[test]
    fn working_status_tracks_active_tool_and_thinking() {
        let mut chat = Chat { phase: Phase::Working, ..Chat::default() };
        assert_eq!(working_status(&chat), "Starting…");
        chat.items.push(Item::Tool { call: call("a", "search_notebook"), result: None, open: false });
        assert_eq!(working_status(&chat), "Searching Notebook…");
        chat.items.push(Item::Reply("Done".into()));
        assert_eq!(working_status(&chat), "Thinking…");
    }

    #[test]
    fn edited_pages_come_from_either_cli_output() {
        assert_eq!(edited_page(r#"{"change_id":3,"action":"created","page":{"id":6,"kind":"note"}}"#), Some(6));
        assert_eq!(edited_page("Revised page #1: SQLite. Change #2 can be reverted by the person."), Some(1));
    }

    #[test]
    fn change_ids_come_from_either_cli_output() {
        assert_eq!(change_id(r#"{"change_id":12,"action":"created"}"#), Some(12));
        assert_eq!(change_id("Revised page #1: x. Change #7 can be reverted by the person."), Some(7));
        assert_eq!(change_id("No change"), None);
    }
}
