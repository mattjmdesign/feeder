use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use feeder_core::gardener::{
    Backend, ChatOpening, ChatSummary, Event, OPENING, RunConfig, RunStatus, Scope, Session, SessionEnv, SessionMode, StoredEvent, ToolCall,
};
use feeder_core::{Change, ChangeAction, Flag, ProposalStatus, ago};
use rune::{Cursor, El, Icon, Key, KeyEvent, ScrollSpec, TextEdit, Transition, Tx, Waker, col, el, icon, row, text};
use rune_kit::diff::{FileDiff, LineKind};
use rune_kit::search::Fold;
use rune_kit::shell::{Sidebar, SidebarPeek, SidebarRow, SidebarSection, SidebarToggle};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, ControlSize, IconButton, IconButtonVariant, Kit, Spinner, SplitView, TextInput,
    Toolbar, scroll_bar,
};

use crate::State;
use crate::chrome;
use crate::style::{self, Palette};
use crate::watch::NotebookOp;

const TOOL_OUTPUT_LINES: usize = 24;
const FOLD_AFTER: usize = 2;
const CHAT_MAX_W: f32 = 640.0;
const USER_BUBBLE_MAX_W: f32 = 480.0;

type SavedChat = (i64, Vec<StoredEvent>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Working,
    Ready,
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    You(String),
    Thinking { text: String, open: bool },
    Reply(String),
    Tool { call: ToolCall, result: Option<ToolResult>, open: bool },
    SignIn(Backend),
    Failed(String),
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub output: String,
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum LastLaunch {
    Message(String),
    Job { title: &'static str, prompt: String },
}

pub struct Chat {
    pub items: Vec<Item>,
    pub input: TextEdit,
    pub phase: Phase,
    pub backend: Option<Backend>,
    run_config: RunConfig,
    pub scope: Scope,
    pub mode: SessionMode,
    pub viewing: Option<i64>,
    pub loading_saved: bool,
    pub delete_confirm: bool,
    last_launch: Option<LastLaunch>,
    saved: Arc<Mutex<Option<SavedChat>>>,
    pub expanded: Vec<usize>,
    pub expanded_diffs: HashSet<String>,
    pub in_flight_proposals: HashSet<i64>,
    pub in_flight_changes: HashSet<i64>,
    session: Arc<Mutex<Option<Session>>>,
    last_run: Arc<Mutex<Option<i64>>>,
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
            run_config: RunConfig::default(),
            scope: Scope::Notebook,
            mode: SessionMode::Review,
            viewing: None,
            loading_saved: false,
            delete_confirm: false,
            last_launch: None,
            saved: Arc::new(Mutex::new(None)),
            expanded: Vec::new(),
            expanded_diffs: HashSet::new(),
            in_flight_proposals: HashSet::new(),
            in_flight_changes: HashSet::new(),
            session: Arc::new(Mutex::new(None)),
            last_run: Arc::new(Mutex::new(None)),
            inbox: Arc::new(Mutex::new(Vec::new())),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn drop_session_bg(session: Arc<Mutex<Option<Session>>>) {
    std::thread::spawn(move || drop(session.lock().unwrap_or_else(|e| e.into_inner()).take()));
}

impl Chat {
    fn draft(&self) -> Option<(SessionMode, &str)> {
        let text = self.input.text.trim();
        (!text.is_empty()).then_some((SessionMode::Review, text))
    }

    fn can_reuse_session(&self, backend: Backend, run_config: &RunConfig) -> bool {
        self.phase == Phase::Ready && self.backend == Some(backend) && self.run_config == *run_config
    }

    pub fn drain(&mut self) -> bool {
        let saved = self.saved.lock().unwrap_or_else(|e| e.into_inner()).take();
        let reopened = saved.is_some();
        if let Some((run_id, stored)) = saved
            && self.viewing == Some(run_id)
        {
            self.loading_saved = false;
            show_saved(self, stored);
        }
        let events: Vec<Event> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        let changed = reopened || !events.is_empty();
        for event in events {
            let ended = matches!(event, Event::Ended);
            apply(&mut self.items, &mut self.phase, self.backend, event);
            if ended {
                drop_session_bg(Arc::clone(&self.session));
            }
        }
        changed
    }

    fn session_send(&self, message: String, wake: Option<Waker>) {
        let session = Arc::clone(&self.session);
        let inbox = Arc::clone(&self.inbox);
        std::thread::spawn(move || {
            let sent = session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.send(&message));
            let err = match sent {
                Some(Ok(())) => None,
                Some(Err(e)) => Some(e.to_string()),
                None => Some("This Curator session has ended. Send your message again to continue.".into()),
            };
            if let Some(why) = err {
                let mut pending = inbox.lock().unwrap_or_else(|e| e.into_inner());
                pending.push(Event::Failed(why));
                pending.push(Event::Ended);
                drop(pending);
                if let Some(wake) = wake {
                    wake();
                }
            }
        });
    }
}

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
    chat.phase = Phase::Ended;
}

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
            if !matches!(items.last(), Some(Item::SignIn(_) | Item::Failed(_))) {
                items.push(Item::Ended);
            }
        }
    }
}

impl State {
    fn gardener_send_op(&self, op: NotebookOp) {
        if let Some(w) = &self.watcher {
            w.notebook(op);
        }
    }

    fn gardener_submit(&mut self) {
        let Some((mode, typed)) = self.notebook.chat.draft() else { return };
        let typed = typed.to_string();
        let chat = &self.notebook.chat;
        if chat.phase == Phase::Working {
            return;
        }
        let Some(backend) = selected_backend(self) else { return };
        if chat.can_reuse_session(backend, &self.curator.prefs.run(backend)) {
            let wake = self.wake.clone();
            let chat = &mut self.notebook.chat;
            chat.input = TextEdit::new("");
            chat.last_launch = Some(LastLaunch::Message(typed.clone()));
            chat.items.push(Item::You(typed.clone()));
            chat.phase = Phase::Working;
            chat.session_send(typed, wake);
            return;
        }

        match current_run(chat) {
            Some(from) => self.gardener_launch(chat.scope, chat.mode, typed, None, Some(from)),
            None if chat.phase != Phase::Ready => self.gardener_start(chat.scope, mode, typed, None),
            None => {}
        }
    }

    fn gardener_retry(&mut self) {
        let launch = self.notebook.chat.last_launch.clone().or_else(|| {
            self.notebook.chat.items.iter().rev().find_map(|item| match item {
                Item::You(text) => Some(LastLaunch::Message(text.clone())),
                _ => None,
            })
        });
        let Some(launch) = launch else { return };
        let scope = self.notebook.chat.scope;
        let mode = self.notebook.chat.mode;
        let backend = self.notebook.chat.backend;
        let mut from = current_run(&self.notebook.chat);
        while matches!(self.notebook.chat.items.last(), Some(Item::SignIn(_) | Item::Failed(_) | Item::Ended)) {
            self.notebook.chat.items.pop();
        }
        if matches!(&launch, LastLaunch::Message(_)) && matches!(self.notebook.chat.items.last(), Some(Item::You(_))) {
            self.notebook.chat.items.pop();
        }
        let preserve = from.is_some() && !self.notebook.chat.items.is_empty();
        if !preserve {
            from = None;
            self.gardener_close();
            self.notebook.chat.scope = scope;
        }
        if let Some(backend) = backend {
            self.notebook.curator_backend = Some(backend);
        }
        match launch {
            LastLaunch::Job { title, prompt } => self.gardener_start(scope, SessionMode::Review, prompt, Some(title)),
            LastLaunch::Message(message) => {
                let mode = if from.is_some() { mode } else { SessionMode::Review };
                self.gardener_launch(scope, mode, message, None, from);
            }
        }
    }

    fn gardener_open_chat(&mut self, summary: &ChatSummary) {
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        drop_session_bg(Arc::clone(&self.notebook.chat.session));
        if let Some(w) = &self.watcher {
            w.curator_chat(Some(summary.run_id));
        }
        let chat = &mut self.notebook.chat;
        *chat = Chat::default();
        chat.viewing = Some(summary.run_id);
        chat.loading_saved = true;
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

    pub fn ask_curator(&mut self, question: String) {
        self.notebook.query = TextEdit::new("");
        self.notebook.apply_tree_filter();
        self.open = None;
        self.dismiss_menus();
        self.tab = crate::Tab::Curator;
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        if self.notebook.chat.phase != Phase::Idle {
            self.gardener_close();
        }
        self.notebook.chat.input = TextEdit::new(question);
        self.gardener_submit();
    }

    pub fn start_project_curator(&mut self, project_id: i64, tx: &mut Tx) {
        self.select_tab(crate::Tab::Curator, tx);
        if self.notebook.chat.phase == Phase::Working {
            return;
        }
        if matches!(self.notebook.chat.phase, Phase::Ready | Phase::Ended) && self.notebook.chat.scope != Scope::Project(project_id) {
            self.gardener_close();
        }
        self.notebook.chat.scope = Scope::Project(project_id);
    }

    fn gardener_start(&mut self, scope: Scope, mode: SessionMode, message: String, job: Option<&'static str>) {
        self.gardener_launch(scope, mode, message, job, None);
    }

    fn gardener_launch(&mut self, scope: Scope, mode: SessionMode, message: String, job: Option<&'static str>, from: Option<i64>) {
        let Some(backend) = selected_backend(self) else { return };
        let Some(wake) = self.wake.clone() else { return };
        let mut env = SessionEnv::detect();
        env.run = self.curator.prefs.run(backend);
        let chat = &self.notebook.chat;
        let previous_session =
            from.filter(|id| chat.viewing.is_none() && current_run(chat) == Some(*id)).map(|_| Arc::clone(&chat.session));
        if self.notebook.gardener.running.as_ref().is_some_and(|run| previous_session.is_none() || Some(run.id) != from) {
            return;
        }
        self.curator_sign_in.begin_run();
        let chat = &mut self.notebook.chat;
        let mut earlier = if from.is_some() { std::mem::take(&mut chat.items) } else { Vec::new() };
        while matches!(earlier.last(), Some(Item::Ended)) {
            earlier.pop();
        }
        *chat = Chat::default();
        chat.items = earlier;
        chat.last_launch = Some(match job {
            Some(title) => LastLaunch::Job { title, prompt: message.clone() },
            None => LastLaunch::Message(message.clone()),
        });
        if job.is_none() {
            chat.items.push(Item::You(message.clone()));
        }
        chat.phase = Phase::Working;
        chat.backend = Some(backend);
        chat.run_config = env.run.clone();
        chat.last_run = Arc::new(Mutex::new(from));
        chat.scope = scope;
        chat.mode = mode;
        let (slot, inbox, last_run) = (Arc::clone(&chat.session), Arc::clone(&chat.inbox), Arc::clone(&chat.last_run));
        let stop_requested = Arc::clone(&chat.stop_requested);
        if let Some(w) = &self.watcher {
            w.curator_chat(from);
        }
        std::thread::spawn(move || {
            if let Some(previous) = previous_session {
                let session = previous.lock().unwrap_or_else(|e| e.into_inner()).take();
                drop(session);
            }
            let sink = {
                let (inbox, wake) = (Arc::clone(&inbox), wake.clone());
                move |event: Event| {
                    inbox.lock().unwrap_or_else(|e| e.into_inner()).push(event);
                    wake();
                }
            };
            let opening = match (job, from) {
                (Some(title), _) => ChatOpening::Job { title, prompt: &message },
                (None, Some(from)) => ChatOpening::Continue { from, message: &message },
                (None, None) => ChatOpening::Message(&message),
            };
            match Session::start_curator_chat(backend, env, scope, mode, opening, sink) {
                Ok(session) => {
                    *last_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.run_id());
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

    pub(crate) fn gardener_stop(&mut self) {
        let chat = &self.notebook.chat;
        chat.stop_requested.store(true, Ordering::Release);
        let session = Arc::clone(&chat.session);
        std::thread::spawn(move || {
            if let Some(s) = session.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                s.stop();
            }
        });
    }

    fn gardener_close(&mut self) {
        self.gardener_stop();
        drop_session_bg(Arc::clone(&self.notebook.chat.session));
        if let Some(w) = &self.watcher {
            w.curator_chat(None);
        }
        self.notebook.chat = Chat::default();
    }

    pub fn open_curator(&mut self, tx: &mut Tx) {
        let already_here = self.open.is_none() && self.tab == crate::Tab::Curator;
        self.select_tab(crate::Tab::Curator, tx);
        if already_here && self.notebook.chat.viewing.is_some() && self.notebook.chat.phase != Phase::Working {
            self.gardener_close();
        }
        if self.notebook.chat.phase != Phase::Working {
            tx.focus_when_shown("gardener-input", false);
        }
    }

    fn gardener_review_flags(&mut self) {
        if matches!(self.notebook.chat.phase, Phase::Idle | Phase::Ended) && self.notebook.chat.viewing.is_none() {
            let scope = self.notebook.chat.scope;
            self.gardener_start(scope, SessionMode::Review, OPENING.to_string(), Some(REVIEW_JOB));
        }
    }
}

const REVIEW_JOB: &str = "Review flagged lessons";

pub fn scoped_flags(s: &State) -> Vec<&Flag> {
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

pub fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => (*a).to_string(),
        [a, b] => format!("{a} and {b}"),
        [head @ .., last] => format!("{}, and {last}", head.join(", ")),
    }
}

fn intro(s: &State, p: &Palette, k: &Kit) -> El {
    const ORB: f32 = 64.0;
    let shadow = el().w(ORB * 0.7).h(8.0).radius(4.0).radial_gradient(
        [0.5, 0.5],
        ORB * 0.35,
        [(0.0, rune::oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.55 } else { 0.18 })), (1.0, rune::oklch(0.0, 0.0, 0.0).alpha(0.0))],
    );
    let orb_wrap = col().items_center().gap(10.0).child(crate::orb::orb(&chrome::CURATOR_ORB, ORB)).child(shadow);
    let backends = &s.notebook.gardener.backends;
    let scanning = s.curator_sign_in.is_scanning();
    let rescan_btn = |variant: ButtonVariant| {
        Button::new(if scanning { "Scanning…" } else { "Scan Again" })
            .variant(variant)
            .control_size(ControlSize::Small)
            .disabled(scanning)
            .on_press(|s: &mut State| s.rescan_clis())
            .build(k)
            .key("gardener-rescan")
    };

    if backends.is_empty() {
        let actions = row().gap(8.0).center_y().child(rescan_btn(ButtonVariant::Contrast)).child(
            Button::new("Curator Settings…")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(crate::curator_settings::open_settings)
                .build(k)
                .key("gardener-open-settings"),
        );
        return col()
            .key("gardener-intro")
            .items_center()
            .gap(14.0)
            .pb(24.0)
            .child(orb_wrap)
            .child(
                col()
                    .items_center()
                    .gap(6.0)
                    .max_w(440.0)
                    .child(text("No Curator CLI detected").font(style::SECTION_TITLE).color(p.text).align(rune::Align::Center))
                    .child(
                        text("Curator runs on your own Claude Code, Codex, OpenCode, or Antigravity CLI.")
                            .font(style::BODY)
                            .color(p.text_2)
                            .wrap()
                            .align(rune::Align::Center),
                    ),
            )
            .child(actions);
    }

    if selected_backend(s).is_none() {
        let names: Vec<&str> = backends.iter().map(|b| b.label()).collect();
        let actions = row()
            .gap(8.0)
            .center_y()
            .child(
                Button::new("Choose Subscription…")
                    .variant(ButtonVariant::Contrast)
                    .control_size(ControlSize::Small)
                    .on_press(crate::curator_settings::open_settings)
                    .build(k)
                    .key("intro-choose-subscription"),
            )
            .child(rescan_btn(ButtonVariant::Outline));
        return col()
            .key("gardener-intro")
            .items_center()
            .gap(14.0)
            .pb(24.0)
            .child(orb_wrap)
            .child(
                col()
                    .items_center()
                    .gap(6.0)
                    .max_w(440.0)
                    .child(
                        text("Turn on a subscription to use Curator").font(style::SECTION_TITLE).color(p.text).align(rune::Align::Center),
                    )
                    .child(
                        text(format!(
                            "Found {} on this Mac. Turn one on in Settings to ask questions or draft Library notes.",
                            join_names(&names)
                        ))
                        .font(style::BODY)
                        .color(p.text_2)
                        .wrap()
                        .align(rune::Align::Center),
                    ),
            )
            .child(actions);
    }

    let waiting = scoped_flags(s);
    let mut root = col()
        .key("gardener-intro")
        .w_full()
        .items_center()
        .gap(14.0)
        .px(24.0)
        .pb(16.0)
        .child(orb_wrap)
        .child(text("Ask about your work").font(style::DISPLAY).color(p.text).wrap().align(rune::Align::Center));

    if !waiting.is_empty() {
        let mut preview = col().w_full().max_w(460.0).gap(6.0);
        for flag in waiting.iter().take(3) {
            preview = preview.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .px(10.0)
                    .py(6.0)
                    .center_y()
                    .radius(8.0)
                    .fill(p.card)
                    .border(1.0, p.hairline)
                    .child(Badge::new(flag.kind.label()).variant(BadgeVariant::Subtle).build(k))
                    .child(text(flag.note.clone()).font(style::META).color(p.text_2).truncate().grow(1.0).min_w(0.0)),
            );
        }
        let n = waiting.len();
        let label = if n == 1 { "Review 1 Flagged Lesson".to_string() } else { format!("Review {n} Flagged Lessons") };
        root = root.child(preview).child(
            Button::new(label)
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .disabled(s.notebook.gardener.running.is_some())
                .on_press(|s: &mut State| s.gardener_review_flags())
                .build(k)
                .key("gardener-review-flags"),
        );
    } else if s.projects.is_empty() && !s.tools_connected() {
        root = root.child(
            Button::new("Connect Your Tools")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(chrome::open_agent_setup)
                .build(k)
                .key("gardener-connect-tools"),
        );
    }
    root
}

pub fn selected_backend(s: &State) -> Option<Backend> {
    crate::curator_settings::selected(s)
}

fn provider_picker(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> Option<El> {
    let current = selected_backend(s)?;
    Some(crate::curator_settings::picker(s, current, p, k, layer))
}

fn working_status(chat: &Chat) -> String {
    match chat.items.iter().rev().find(|i| !matches!(i, Item::You(_))) {
        Some(Item::Tool { call, result: None, .. }) => live_label(call),
        Some(Item::Thinking { .. } | Item::Tool { .. } | Item::Reply(_)) => "Thinking…".into(),
        _ => "Starting…".into(),
    }
}

pub fn view(s: &State, p: &Palette, k: &Kit, max_w: f32, reduced_motion: bool) -> El {
    let mut page = col().key("gardener").w_full().max_w(max_w.min(CHAT_MAX_W));
    if let Some(bar) = top_bar(s, p, k) {
        page = page.child(bar);
    }
    let chat = &s.notebook.chat;
    if chat.viewing.is_some() && chat.loading_saved && chat.items.is_empty() {
        return page.child(
            row()
                .key("gardener-loading")
                .gap(8.0)
                .center_y()
                .py(18.0)
                .child(Spinner::new().size(13.0).color(p.text_2).build(k))
                .child(text("Loading chat…").font(style::META).color(p.text_2)),
        );
    }
    if is_fresh(s) { page } else { page.child(transcript(s, p, k, reduced_motion)) }
}

fn is_fresh(s: &State) -> bool {
    let chat = &s.notebook.chat;
    chat.items.is_empty() && chat.phase == Phase::Idle
}

pub fn is_empty_idle(s: &State) -> bool {
    is_fresh(s) && s.notebook.chat.viewing.is_none()
}

pub fn empty_view(s: &State, p: &Palette, k: &Kit, max_w: f32, layer: &mut Vec<El>) -> El {
    let mut stack = col().key("gardener-empty").w_full().items_center();
    if let Some(bar) = top_bar(s, p, k) {
        stack = stack.child(col().w_full().max_w(max_w.min(CHAT_MAX_W)).px(40.0).child(bar));
    }
    stack = stack.child(intro(s, p, k)).child(composer(s, p, k, max_w, layer));
    col().w_full().grow(1.0).min_h(0.0).justify_center().items_center().pb(48.0).child(stack)
}

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
                Button::new("All Projects")
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

    let action = match (chat.delete_confirm, chat.viewing) {
        (true, Some(run_id)) => Some(
            row()
                .gap(6.0)
                .center_y()
                .child(text("Delete chat?").font(style::META).color(p.text_2))
                .child(
                    Button::new("Keep")
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .on_press(|s: &mut State| s.notebook.chat.delete_confirm = false)
                        .build(k)
                        .key("gardener-delete-keep"),
                )
                .child(
                    Button::new("Delete Chat")
                        .variant(ButtonVariant::Danger)
                        .control_size(ControlSize::Small)
                        .on_press(move |s: &mut State| s.gardener_delete_chat(run_id))
                        .build(k)
                        .key("gardener-delete-chat-confirm"),
                ),
        ),
        (false, Some(_)) => Some(
            Button::new("Delete")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(|s: &mut State| s.notebook.chat.delete_confirm = true)
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

fn status_line(s: &State, p: &Palette) -> Option<El> {
    let g = &s.notebook.gardener;
    let chat = &s.notebook.chat;
    if chat.phase != Phase::Idle {
        return None;
    }
    let backend = chat.backend.or_else(|| g.backends.first().copied());
    let line = match (backend, &g.running, &g.last_run) {
        (None, _, _) => return None,
        (_, Some(run), _) => format!("Active run started {}", ago(run.started_at, s.now)),
        (_, None, Some(run)) if run.status == RunStatus::Failed => {
            let when = ago(run.started_at, s.now);
            match run.error.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
                Some(err) => format!("Last run failed {when}: {err}"),
                None => format!("Last run failed {when}"),
            }
        }
        (_, None, _) => return None,
    };
    Some(text(line).font(style::META).color(p.text_3).wrap().shrink().min_w(0.0))
}

pub fn composer(s: &State, p: &Palette, k: &Kit, max_w: f32, layer: &mut Vec<El>) -> El {
    let chat = &s.notebook.chat;
    let backend = selected_backend(s);
    let placeholder = match chat.phase {
        Phase::Working => "Working…",
        Phase::Ready | Phase::Ended => "Ask a follow-up…",
        Phase::Idle => "Ask a question…",
    };
    let disabled = backend.is_none() || chat.phase == Phase::Working;
    let mut field = rune::text_input(&chat.input, style::BODY)
        .key("gardener-input")
        .w_full()
        .color(p.text)
        .placeholder(placeholder, p.text_3)
        .caret(p.text, p.selection)
        .label("Message Curator")
        .disabled(disabled);
    if !disabled {
        field = field.on_edit(|s: &mut State| &mut s.notebook.chat.input).on_submit(|s: &mut State, _tx: &mut Tx| s.gardener_submit());
    }
    let input = rune::auto_grow(field, 1, 8);

    let draft = chat.draft();
    let send = if chat.phase == Phase::Working {
        IconButton::new(&STOP_ICON)
            .variant(IconButtonVariant::Neutral)
            .label("Stop")
            .on_press(|s: &mut State| s.gardener_stop())
            .build(k)
            .key("gardener-stop")
    } else {
        IconButton::new(&SEND_ICON)
            .variant(IconButtonVariant::Submit)
            .label(if matches!(chat.phase, Phase::Idle | Phase::Ended) { "Ask" } else { "Send" })
            .disabled(
                backend.is_none()
                    || s.notebook.gardener.running.as_ref().is_some_and(|run| Some(run.id) != current_run(chat))
                    || draft.is_none(),
            )
            .on_press(|s: &mut State| s.gardener_submit())
            .build(k)
            .key("gardener-send")
    };

    let mut toolbar = row().w_full().gap(6.0).center_y();
    toolbar = toolbar.child(rune::spacer());
    if let Some(picker) = provider_picker(s, p, k, layer) {
        toolbar = toolbar.child(picker);
    } else if !s.notebook.gardener.backends.is_empty() && !is_empty_idle(s) {
        toolbar = toolbar.child(
            Button::new("Choose Subscription…")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(crate::curator_settings::open_settings)
                .build(k)
                .key("curator-choose-subscription"),
        );
    }
    let card = col()
        .key("gardener-composer")
        .w_full()
        .max_w(max_w.min(CHAT_MAX_W))
        .gap(8.0)
        .pl(14.0)
        .pr(8.0)
        .pt(12.0)
        .pb(8.0)
        .radius(COMPOSER_RADIUS)
        .fill(p.card)
        .border(1.0, if chat.input.focused { p.text_3 } else { p.hairline })
        .child(col().w_full().pr(6.0).child(input))
        .child(toolbar.child(send));
    col().w_full().items_center().px(40.0).pt(8.0).pb(14.0).child(card)
}

const COMPOSER_RADIUS: f32 = 12.0;

pub(crate) static SEND_ICON: Icon = Icon {
    name: "curator-send",
    view: 16.0,
    body: r#"<path d="M8 12.75V3.5M3.75 7.5L8 3.25L12.25 7.5" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static STOP_ICON: Icon =
    Icon { name: "curator-stop", view: 16.0, body: r#"<rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="currentColor"/>"# };

fn sign_in_panel(s: &State, key: impl Into<String>, backend: Backend, p: &Palette, k: &Kit, retry: Option<fn(&mut State)>) -> El {
    col()
        .key(key.into())
        .role("group")
        .label(format!("Sign in to {}", backend.label()))
        .w_full()
        .gap(8.0)
        .px(14.0)
        .py(12.0)
        .radius(10.0)
        .border(1.0, p.hairline)
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(crate::agents::mark(backend.label(), p, 16.0, s.agent_icon(backend.label())))
                .child(text(format!("Sign in to {}", backend.label())).font(style::NAME).color(p.text)),
        )
        .child(crate::curator_sign_in::controls(s, backend, p, k, retry))
}

fn quiet_step(item: &Item) -> bool {
    matches!(item, Item::Tool { call, result: Some(r), .. } if !r.failed && !is_edit(&call.name) && call.name != "dismiss_flag")
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
        let turn = i > 0 && matches!(chat.items[i], Item::You(_));
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
        Item::SignIn(backend) => sign_in_panel(s, format!("gardener-signin-{i}"), *backend, p, k, Some(State::gardener_retry)),
        Item::Failed(why) => col()
            .key(format!("gardener-failed-{i}"))
            .w_full()
            .gap(8.0)
            .child(Card::new().accent(p.amber).title("Run stopped", None).description(why.clone()).build(k))
            .child(
                row().child(
                    Button::new("Try Again")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(|s: &mut State| s.gardener_retry())
                        .build(k)
                        .key(format!("gardener-retry-{i}")),
                ),
            ),
        Item::Ended => text("Run ended").font(style::META).color(p.text_3).pt(2.0),
    }
}

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
        "read_page" => call.int_arg("id").map_or_else(|| "a note".into(), |id| format!("note #{id}")),
        other => other.replace('_', " "),
    }
}

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
        Some(id) => s.notebook.page(id).map_or_else(|| format!("note #{id}"), |pg| format!("“{}”", pg.title)),
        None => "a note".to_string(),
    };
    match call.name.as_str() {
        "list_flags" => "Read open flags".into(),
        "read_traces" => "Read recent Feed updates".into(),
        "search_notebook" if arg("query").trim().is_empty() => "Searched Library".into(),
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
        ("search_notebook", None) => "Searching Library…".into(),
        ("list_flags", _) => "Reading open flags…".into(),
        ("read_traces", _) => "Reading recent Feed updates…".into(),
        ("read_page", _) => "Reading note…".into(),
        ("write_page", _) => match call.text_arg("title") {
            Some(t) => format!("Writing “{t}”…"),
            None => "Writing note…".into(),
        },
        ("archive_page", _) => "Archiving note…".into(),
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

fn proposal_id(output: &str) -> Option<i64> {
    number_after(output, &["\"proposal_id\":", "proposal #"])
}

fn edited_page(output: &str) -> Option<i64> {
    number_after(output, &["\"page\":{\"id\":", "\"page_id\":", "page #"])
}

fn edit_card(s: &State, i: usize, call: &ToolCall, result: &ToolResult, p: &Palette, k: &Kit) -> El {
    if let Some(pid) = proposal_id(&result.output) {
        return proposal_card(s, i, pid, call, result, p, k);
    }
    let page = edited_page(&result.output).or_else(|| call.int_arg("id"));
    let (verb, fallback) = match call.name.as_str() {
        "archive_page" => ("Archived", String::new()),
        _ if call.int_arg("id").is_some() => ("Updated", call.text_arg("title").unwrap_or("").to_string()),
        _ => ("New note", call.text_arg("title").unwrap_or("").to_string()),
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
    let changes = &s.notebook.gardener.changes;
    let change = match change_id(&result.output) {
        Some(cid) => changes.iter().find(|c| c.id == cid),
        None => page.and_then(|id| changes.iter().find(|c| c.page_id == id && c.undoes.is_none())),
    };
    let before = change.and_then(|c| c.before.as_ref()).map(|b| b.body.as_str());
    match found.filter(|pg| !pg.is_archived()) {
        Some(pg) if verb != "Updated" || before.is_some() => {
            if let Some(diff) = diff_preview(s, &format!("edit:{i}"), before.unwrap_or(""), &pg.body, p, k) {
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
                .on_press(move |s: &mut State| s.open_notebook_page_from_anywhere(id))
                .build(k)
                .key(format!("gardener-open-{i}")),
        );
    }
    if let Some(cid) = change.map(|c| c.id) {
        let reverted = s.notebook.gardener.changes.iter().find(|c| c.id == cid).is_some_and(Change::is_reverted);
        let reverting = s.notebook.chat.in_flight_changes.contains(&cid);
        actions = actions.child(if reverted {
            Badge::new("Reverted").variant(BadgeVariant::Subtle).build(k)
        } else {
            Button::new(if reverting { "Reverting…" } else { "Revert" })
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .disabled(reverting)
                .on_press(move |s: &mut State| {
                    s.notebook.chat.in_flight_changes.insert(cid);
                    s.gardener_send_op(NotebookOp::Revert(cid));
                })
                .build(k)
                .key(format!("gardener-revert-{cid}"))
        });
    }
    card.child(actions)
}

fn proposal_card(s: &State, i: usize, pid: i64, call: &ToolCall, result: &ToolResult, p: &Palette, k: &Kit) -> El {
    let proposal = s.notebook.gardener.proposals.iter().find(|pr| pr.id == pid);
    let status = proposal.map_or(ProposalStatus::Pending, |pr| pr.status);
    let busy = s.notebook.chat.in_flight_proposals.contains(&pid) || (s.notebook.chat.viewing.is_some() && proposal.is_none());
    let action = proposal.map_or_else(
        || match call.name.as_str() {
            "archive_page" => ChangeAction::Archived,
            _ if call.int_arg("id").is_some() => ChangeAction::Revised,
            _ => ChangeAction::Created,
        },
        |pr| pr.action,
    );
    let page_id = proposal.and_then(|pr| pr.page_id).or_else(|| edited_page(&result.output)).or_else(|| call.int_arg("id"));
    let existing = page_id.and_then(|id| s.notebook.page(id));
    let verb = match (status, action) {
        (ProposalStatus::Applied, ChangeAction::Created) => "Added to Library",
        (ProposalStatus::Applied, ChangeAction::Revised) => "Updated in Library",
        (ProposalStatus::Applied, ChangeAction::Archived) => "Archived",
        (_, ChangeAction::Archived) => "Proposed archive",
        (_, ChangeAction::Revised) => "Proposed update",
        _ => "Proposed note",
    };
    let title = proposal
        .map(|pr| pr.title.as_str())
        .or_else(|| call.text_arg("title"))
        .or_else(|| existing.map(|pg| pg.title.as_str()))
        .unwrap_or_default()
        .to_string();
    let topic = proposal.map(|pr| pr.topic.as_str()).or_else(|| existing.map(|pg| pg.topic.as_str())).or_else(|| call.text_arg("topic"));
    let reason = proposal.map(|pr| pr.reason.as_str()).or_else(|| call.text_arg("reason")).unwrap_or("").trim();
    let mut eyebrow = row().w_full().gap(6.0).center_y().child(text(verb).font(style::META).color(p.text_3));
    if let Some(slug) = topic.filter(|t| !t.trim().is_empty()) {
        eyebrow = eyebrow.child(Badge::new(feeder_core::topic_label(slug)).variant(BadgeVariant::Subtle).build(k));
    }
    let mut card = col()
        .key(format!("gardener-proposal-{pid}-{i}"))
        .role("group")
        .label(format!("{verb}: {title}"))
        .w_full()
        .gap(6.0)
        .px(12.0)
        .py(10.0)
        .radius(8.0)
        .border(1.0, p.hairline)
        .child(eyebrow)
        .child(text(title).font(style::NAME).color(p.text).wrap());
    if !reason.is_empty() {
        card = card.child(text(reason.to_string()).font(style::META).color(p.text_2).wrap());
    }
    let old_body = proposal.and_then(|pr| pr.before_body.as_deref()).or_else(|| existing.map(|pg| pg.body.as_str())).unwrap_or("");
    let new_body = proposal.map(|pr| pr.body.as_str()).or_else(|| call.text_arg("body")).unwrap_or("");
    if action != ChangeAction::Archived {
        if let Some(diff) = diff_preview(s, &format!("proposal:{pid}"), old_body, new_body, p, k) {
            card = card.child(diff);
        }
    } else if let Some(pg) = existing
        && !pg.excerpt(160).is_empty()
    {
        card = card.child(text(pg.excerpt(160)).font(style::META).color(p.text_2).wrap().max_lines(2));
    }
    let mut actions = row().gap(6.0).pt(2.0);
    match status {
        ProposalStatus::Pending => {
            actions = actions
                .child(
                    Button::new(if busy { "Applying…" } else { "Apply" })
                        .variant(ButtonVariant::Contrast)
                        .control_size(ControlSize::Small)
                        .disabled(busy)
                        .on_press(move |s: &mut State| {
                            s.notebook.chat.in_flight_proposals.insert(pid);
                            s.gardener_send_op(NotebookOp::ApplyProposal(pid));
                        })
                        .build(k)
                        .key(format!("gardener-apply-proposal-{pid}")),
                )
                .child(
                    Button::new("Dismiss")
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .disabled(busy)
                        .on_press(move |s: &mut State| {
                            s.notebook.chat.in_flight_proposals.insert(pid);
                            s.gardener_send_op(NotebookOp::DismissProposal(pid));
                        })
                        .build(k)
                        .key(format!("gardener-dismiss-proposal-{pid}")),
                );
            if let Some(id) = page_id.filter(|_| existing.is_some()) {
                actions = actions.child(
                    Button::new("Open")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(move |s: &mut State| s.open_notebook_page_from_anywhere(id))
                        .build(k)
                        .key(format!("gardener-open-proposal-{pid}")),
                );
            }
        }
        ProposalStatus::Dismissed => {
            actions = actions.child(Badge::new("Dismissed").variant(BadgeVariant::Subtle).build(k));
        }
        ProposalStatus::Applied => {
            if let Some(id) = page_id.filter(|_| existing.is_some()) {
                actions = actions.child(
                    Button::new("Open")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(move |s: &mut State| s.open_notebook_page_from_anywhere(id))
                        .build(k)
                        .key(format!("gardener-open-proposal-{pid}")),
                );
            }
            if let Some(cid) = proposal.and_then(|pr| pr.change_id) {
                let reverted = s.notebook.gardener.changes.iter().find(|c| c.id == cid).is_some_and(Change::is_reverted);
                let reverting = s.notebook.chat.in_flight_changes.contains(&cid);
                actions = actions.child(if reverted {
                    Badge::new("Reverted").variant(BadgeVariant::Subtle).build(k)
                } else {
                    Button::new(if reverting { "Reverting…" } else { "Revert" })
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .disabled(reverting)
                        .on_press(move |s: &mut State| {
                            s.notebook.chat.in_flight_changes.insert(cid);
                            s.gardener_send_op(NotebookOp::Revert(cid));
                        })
                        .build(k)
                        .key(format!("gardener-revert-{cid}"))
                });
            }
        }
    }
    card.child(actions)
}

const DIFF_LINES: usize = 6;

fn diff_preview(s: &State, diff_key: &str, old: &str, new: &str, p: &Palette, k: &Kit) -> Option<El> {
    let diff = FileDiff::from_texts("page.md", old, new);
    let changed: Vec<_> =
        diff.hunks.iter().flat_map(|h| h.lines.iter()).filter(|l| l.kind != LineKind::Context && !l.text.trim().is_empty()).collect();
    if changed.is_empty() {
        return None;
    }
    let expanded = s.notebook.chat.expanded_diffs.contains(diff_key);
    let shown = if expanded { changed.len() } else { DIFF_LINES };
    let mut lines = col().w_full().gap(2.0).label(format!("{} lines added, {} removed", diff.additions(), diff.deletions()));
    for line in changed.iter().take(shown) {
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
        let label = if expanded { "Show Fewer Lines".to_string() } else { format!("Show {} More Lines", changed.len() - DIFF_LINES) };
        let key_owned = diff_key.to_string();
        lines = lines.child(
            Button::new(label)
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut State| {
                    if !s.notebook.chat.expanded_diffs.remove(&key_owned) {
                        s.notebook.chat.expanded_diffs.insert(key_owned.clone());
                    }
                })
                .build(k)
                .key(format!("diff-toggle-{diff_key}")),
        );
    }
    Some(lines)
}

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

fn disclosed(body: El) -> El {
    col().w_full().pl(18.0).pb(4.0).child(body)
}

pub fn is_busy(s: &State) -> bool {
    let chat = &s.notebook.chat;
    chat.phase == Phase::Working || s.notebook.gardener.running.as_ref().is_some_and(|run| Some(run.id) != current_run(chat))
}

fn current_run(chat: &Chat) -> Option<i64> {
    chat.viewing.or_else(|| chat.last_run.lock().ok().and_then(|g| *g))
}

fn bucket(started_at: i64, now: i64) -> &'static str {
    match feeder_core::day_key(now) - feeder_core::day_key(started_at) {
        ..=0 => "Today",
        1 => "Yesterday",
        2..=7 => "Previous 7 days",
        _ => "Older",
    }
}

pub fn shell_view(s: &State, p: &Palette, k: &Kit, inset: f32, reduced_motion: bool, layer: &mut Vec<El>) -> El {
    let max_w = crate::notebook::INDEX_MAX_W;
    let following = s.notebook.chat.phase == Phase::Working;
    let scroll_key = format!("curator-scroll-{}", current_run(&s.notebook.chat).map_or_else(|| "new".to_string(), |id| id.to_string()));
    let main_inner = if is_empty_idle(s) {
        empty_view(s, p, k, max_w, layer).transition(Transition::none())
    } else {
        let body = view(s, p, k, max_w, reduced_motion);
        let viewport = col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(col().w_full().items_center().px(crate::notebook::INDEX_PAD).pt(24.0).pb(24.0).child(body))
            .scroll(ScrollSpec { stick_bottom: following })
            .scroll_edge_fade(16.0);
        let scroll_area = scroll_bar(k, &format!("{scroll_key}-area"), &scroll_key, viewport)
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .transition(Transition::none());
        col().w_full().grow(1.0).min_h(0.0).transition(Transition::none()).child(scroll_area).child(composer(s, p, k, max_w, layer))
    };
    let main_area = col().key("curator-main-area").w_full().grow(1.0).min_h(0.0).instant_changes().child(main_inner);
    let toolbar = Toolbar::new().leading(toolbar_breadcrumb(s, p, k)).trailing(chrome::tabs(s, k)).trailing(chrome::curator_button(s, p));

    SplitView::new(main_area)
        .sidebar(&s.curator_pane, |s: &mut State| &mut s.curator_pane, sidebar_view(s, p, k))
        .toolbar(toolbar)
        .titlebar_inset(inset)
        .frame_inset(style::PANE_INSET)
        .sidebar_toggle(SidebarToggle::new().in_header().past_lights(10.0))
        .sidebar_peek(SidebarPeek::new())
        .reduced_motion(reduced_motion)
        .build(&style::open_pane_kit(k, p))
}

fn toolbar_breadcrumb(s: &State, p: &Palette, k: &Kit) -> El {
    let pad = crate::notebook::CRUMB_PAD;
    let mut row_el = row().key("curator-breadcrumb").gap(2.0).center_y().shrink().min_w(0.0).instant_changes();
    if let Some(run_id) = s.notebook.chat.viewing
        && let Some(chat) = s.notebook.gardener.chats.iter().find(|c| c.run_id == run_id)
    {
        let root_btn = Button::new("Curator")
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                if s.notebook.chat.phase != Phase::Working {
                    s.gardener_close();
                    tx.focus_when_shown("gardener-input", false);
                }
            })
            .build(k)
            .key("curator-bc-root");
        row_el = row_el
            .child(root_btn)
            .child(text("/").font(style::META).color(p.text_3).px(2.0))
            .child(text(chat.title.clone()).font(k.ty.ui_compact).color(p.text).truncate().max_w(260.0).shrink().min_w(0.0).pl(pad));
        return row_el;
    }
    row_el.child(text("Curator").key("curator-bc-root").font(k.ty.ui_compact).color(p.text).pl(pad))
}

fn sidebar_view(s: &State, p: &Palette, k: &Kit) -> El {
    let mut search_input = TextInput::new(&s.curator_search)
        .placeholder("Search chats")
        .label("Search Curator chats")
        .leading_icon(k.glyph.search)
        .width(None)
        .on_edit(|s: &mut State| &mut s.curator_search)
        .on_action(|s: &mut State, _| s.curator_search = TextEdit::new(""))
        .on_key(|s: &mut State, e: &KeyEvent, _| {
            if matches!(e.key, Key::Escape) && !s.curator_search.text.is_empty() {
                s.curator_search = TextEdit::new("");
                return true;
            }
            false
        });
    if !s.curator_search.text.is_empty() {
        search_input = search_input.clear_button();
    }
    let search = search_input.build(k).key("curator-search");

    let header = col()
        .key("curator-sidebar-header")
        .w_full()
        .px(10.0)
        .pt(4.0)
        .pb(8.0)
        .gap(8.0)
        .child(search)
        .child(col().w_full().gap(2.0).role("list").label("Curator").child(new_chat_row(k)));

    col()
        .key("curator-sidebar")
        .w_full()
        .h_full()
        .min_h(0.0)
        .pb(crate::notebook::SETTINGS_CORNER_H)
        .instant_changes()
        .child(header)
        .child(col().w_full().grow(1.0).min_h(0.0).child(thread_list(s, p, k)))
}

pub fn thread_list(s: &State, p: &Palette, k: &Kit) -> El {
    let scope = s.notebook.chat.scope;
    let selected = current_run(&s.notebook.chat);
    let query = s.curator_search.text.trim();
    let fold = Fold::default();
    let mut sidebar = Sidebar::new().label("Curator chats");
    let mut section: Option<(&str, Vec<El>)> = None;
    let mut matched = 0usize;
    for c in s
        .notebook
        .gardener
        .chats
        .iter()
        .filter(|c| scope == Scope::Notebook || c.scope == scope)
        .filter(|c| query.is_empty() || fold.contains(&c.title, query))
    {
        matched += 1;
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
    if matched == 0 {
        let msg = if query.is_empty() { "Past chats appear here." } else { "No matching chats." };
        return col().w_full().grow(1.0).px(16.0).py(12.0).child(text(msg).font(style::META).color(p.text_3).wrap());
    }
    if let Some((l, rows)) = section {
        sidebar = sidebar.section(SidebarSection::new(l).key(format!("threads-{l}")).rows(rows));
    }
    sidebar.build(k)
}

pub fn new_chat_row(k: &Kit) -> El {
    SidebarRow::new("New Chat")
        .icon(&crate::notebook::PLUS_ICON)
        .on_select(|s: &mut State, tx: &mut Tx| {
            if s.notebook.chat.phase != Phase::Working {
                s.gardener_close();
                tx.focus_when_shown("gardener-input", false);
            }
        })
        .build(k)
        .key("gardener-new-chat")
}

pub fn preview_saved(chat: &mut Chat, backend: Backend, run_id: i64, question: &str, events: Vec<Event>) {
    chat.backend = Some(backend);
    chat.viewing = Some(run_id);
    let mut stored = vec![StoredEvent::You(question.to_string())];
    stored.extend(events.into_iter().map(StoredEvent::Event));
    show_saved(chat, stored);
}

pub fn replay(chat: &mut Chat, backend: Backend, events: Vec<Event>) {
    chat.backend = Some(backend);
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
    fn saved_chats_reopen_finished_and_ready_to_continue() {
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
        let s = State::new();
        let p = style::palette(false);
        let k = style::kit(&p);
        assert!(diff_preview(&s, "t:1", "same\n", "same\n", &p, &k).is_none(), "no card diff when nothing changed");
        let diff = FileDiff::from_texts("page.md", "a\nold\nc\n", "a\nnew\nc\nadded\n");
        assert_eq!((diff.additions(), diff.deletions()), (2, 1));
        assert!(diff_preview(&s, "t:2", "a\nold\nc\n", "a\nnew\nc\nadded\n", &p, &k).is_some());
    }

    #[test]
    fn dismiss_flag_never_folds_and_retry_preserves_multi_turn_history() {
        let dismiss = Item::Tool {
            call: call("d1", "dismiss_flag"),
            result: Some(ToolResult { output: "Dismissed".into(), failed: false }),
            open: false,
        };
        assert!(!quiet_step(&dismiss));
        let search = Item::Tool {
            call: call("s1", "search_notebook"),
            result: Some(ToolResult { output: "0 hits".into(), failed: false }),
            open: false,
        };
        assert!(quiet_step(&search));

        let mut s = State::new();
        s.notebook.chat.items =
            vec![Item::You("Turn 1".into()), Item::Reply("Answer 1".into()), Item::You("Turn 2".into()), Item::Failed("timeout".into())];
        s.notebook.chat.last_launch = Some(LastLaunch::Message("Turn 2".into()));
        *s.notebook.chat.last_run.lock().unwrap() = Some(7);
        s.gardener_retry();
        assert_eq!(s.notebook.chat.items.len(), 2);
        assert!(matches!(&s.notebook.chat.items[0], Item::You(t) if t == "Turn 1"));
    }

    #[test]
    fn sign_in_and_end_show_as_notices() {
        let (mut items, mut phase) = (vec![], Phase::Working);
        apply(&mut items, &mut phase, Some(Backend::ClaudeCode), Event::NeedsSignIn);
        apply(&mut items, &mut phase, Some(Backend::ClaudeCode), Event::Ended);
        assert_eq!(items, [Item::SignIn(Backend::ClaudeCode)], "the sign-in card already explains the stop");
        assert_eq!(phase, Phase::Ended);
    }

    #[test]
    fn working_status_tracks_active_tool_and_thinking() {
        let mut chat = Chat { phase: Phase::Working, ..Chat::default() };
        assert_eq!(working_status(&chat), "Starting…");
        chat.items.push(Item::Tool { call: call("a", "search_notebook"), result: None, open: false });
        assert_eq!(working_status(&chat), "Searching Library…");
        chat.items.push(Item::Reply("Done".into()));
        assert_eq!(working_status(&chat), "Thinking…");
    }

    #[test]
    fn proposal_ids_come_from_either_cli_output() {
        assert_eq!(proposal_id(r#"{"proposal_id":9,"status":"pending"}"#), Some(9));
        assert_eq!(proposal_id("Proposed created “SQLite” (proposal #4)."), Some(4));
        assert_eq!(proposal_id("No proposal"), None);
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

    #[test]
    fn follow_ups_reuse_only_the_same_ready_provider_configuration() {
        let run = RunConfig { model: Some("provider/first".into()), effort: Some("high".into()) };
        let mut chat = Chat { phase: Phase::Ready, backend: Some(Backend::OpenCode), run_config: run.clone(), ..Chat::default() };
        assert!(chat.can_reuse_session(Backend::OpenCode, &run));
        assert!(!chat.can_reuse_session(Backend::Codex, &run));
        let other_model = RunConfig { model: Some("provider/second".into()), ..run.clone() };
        assert!(!chat.can_reuse_session(Backend::OpenCode, &other_model));
        let other_effort = RunConfig { effort: None, ..run.clone() };
        assert!(!chat.can_reuse_session(Backend::OpenCode, &other_effort));
        chat.phase = Phase::Working;
        assert!(!chat.can_reuse_session(Backend::OpenCode, &run));
        chat.phase = Phase::Ended;
        assert!(!chat.can_reuse_session(Backend::OpenCode, &run));
    }

    #[test]
    fn session_send_with_closed_session_recovers_and_wakes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake: Waker = Arc::new(move || {
            let _ = tx.send(());
        });
        let mut chat = Chat { phase: Phase::Working, ..Chat::default() };
        chat.session_send("follow-up".into(), Some(wake));
        rx.recv_timeout(std::time::Duration::from_secs(2)).expect("session_send must wake the UI on failure");
        assert!(chat.drain());
        assert_eq!(chat.phase, Phase::Ended);
        assert!(matches!(chat.items.last(), Some(Item::Failed(_))));
    }
}
