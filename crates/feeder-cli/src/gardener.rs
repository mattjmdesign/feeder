use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use clap::Subcommand;
use feeder_core::gardener::{
    Backend, ChatOpening, ChatSummary, CuratorPrefs, Event, OPENING, Run, RunStatus, Scope, Session, SessionEnv, SessionMode, StoredEvent,
    effort_label,
};
use feeder_core::{Change, Flag, FlagOutcome, FlagStatus, HUMAN_AUTHOR, NewFlag, Proposal, Store, ago, now_ms};
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum GardenerCmd {
    #[command(
        about = "Start Curator with an installed agent. It streams what the agent does; type follow-ups after each answer, or press Enter to end."
    )]
    Run {
        #[arg(help = "`claude`, `codex`, `opencode`, or `antigravity`. Defaults to your Curator default or the first installed CLI.")]
        #[arg(long = "with")]
        backend: Option<String>,
        #[arg(help = "Override the model ID or alias for this session.")]
        #[arg(long)]
        model: Option<String>,
        #[arg(help = "Override the thinking/reasoning effort level for this session (for example `low`, `medium`, `high`).")]
        #[arg(long)]
        effort: Option<String>,
        #[arg(help = "Limit reads and edits to this project (name or ID).")]
        #[arg(long)]
        project: Option<String>,
        #[arg(help = "Answer a question from the Notebook without making any changes.")]
        #[arg(long, requires = "message")]
        ask: bool,
        #[arg(help = "Your first message. Defaults to reviewing open flags and recent updates.")]
        message: Vec<String>,
    },
    #[command(about = "Sign in through the provider's own CLI.")]
    Connect {
        #[arg(long = "with")]
        backend: String,
    },
    #[command(about = "Recent runs and which CLIs can run Curator.")]
    Status {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "List saved Curator chats, newest first.")]
    Chats {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Read a saved Curator chat by its run ID.")]
    Chat {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Delete a saved chat while keeping its Notebook edits.")]
    DeleteChat { id: i64 },
    #[command(about = "Recent Notebook changes, with their reasons, newest first.")]
    Changes {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(help = "Only the gardener's changes.")]
        #[arg(long)]
        mine: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Undo one change (see `feeder curator changes`).")]
    Revert { id: i64 },
    #[command(about = "List pending Library proposals from Curator chats.")]
    Proposals {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Apply a pending Curator proposal by its ID.")]
    Apply {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Dismiss a pending Curator proposal by its ID.")]
    Dismiss {
        id: i64,
        #[arg(long)]
        json: bool,
    },
}

pub fn flag(store: &Store, flag: NewFlag, author: &str, json: bool) -> Result<(), String> {
    let flag = store.flag_knowledge(flag, author).map_err(|e| e.to_string())?;
    print_flag(&flag, json);
    Ok(())
}

pub(crate) fn print_flag(flag: &Flag, json: bool) {
    if json {
        println!("{}", flag_json(flag));
    } else {
        println!("Flagged #{} ({}) for Curator to review.", flag.id, flag.kind);
    }
}

pub fn flags(store: &Store, all: bool, dismiss: Option<i64>, reason: Option<String>, json: bool) -> Result<(), String> {
    if let Some(id) = dismiss {
        let note = reason.unwrap_or_default();
        let flag = store.close_flag(id, FlagOutcome::Dismissed { note }, HUMAN_AUTHOR).map_err(|e| e.to_string())?;
        println!("Dismissed flag #{}.", flag.id);
        return Ok(());
    }
    let status = if all { None } else { Some(FlagStatus::Open) };
    let list = store.flags(status, 200).map_err(|e| e.to_string())?;
    if json {
        println!("{}", Value::Array(list.iter().map(flag_json).collect()));
    } else if list.is_empty() {
        println!("{}", if all { "No flags yet." } else { "No open flags." });
    } else {
        let now = now_ms();
        for f in &list {
            let page = f.page_id.map(|id| format!(" page #{id}")).unwrap_or_default();
            let state = if f.status == FlagStatus::Open { String::new() } else { format!(" ({})", f.status.as_str()) };
            println!("#{:<4} {:<8} {}{page} · {} · {}{state}", f.id, f.kind.as_str(), f.note, f.agent, ago(f.created_at, now));
        }
    }
    Ok(())
}

pub fn run(store: &Store, cmd: GardenerCmd) -> Result<(), String> {
    match cmd {
        GardenerCmd::Run { backend, model, effort, project, ask, message } => {
            let prefs = CuratorPrefs::load(store.dir());
            let installed = Backend::installed();
            let backend = match backend {
                Some(name) => name.parse::<Backend>().map_err(|e| e.to_string())?,
                None => prefs
                    .default_backend()
                    .filter(|b| installed.contains(b))
                    .or_else(|| prefs.enabled().into_iter().find(|b| installed.contains(b)))
                    .or_else(|| installed.first().copied())
                    .ok_or("Install Claude Code, Codex, OpenCode, or Antigravity to use Curator.")?,
            };
            let mut env = SessionEnv::detect();
            env.run = prefs.run(backend);
            if let Some(model) = model {
                env.run.model = Some(model);
            }
            if let Some(effort) = effort {
                env.run.effort = Some(effort);
            }
            let text = message.join(" ");
            let opening = if message.is_empty() {
                ChatOpening::Job { title: "Review Open Flags", prompt: OPENING }
            } else {
                ChatOpening::Message(&text)
            };
            let scope = match project {
                Some(name) => Scope::Project(crate::find(store, &name)?.id),
                None => Scope::Notebook,
            };
            let mode = if ask { SessionMode::Ask } else { SessionMode::Review };
            chat(backend, env, &prefs.name(backend), scope, mode, opening)?;
        }
        GardenerCmd::Connect { backend } => {
            backend.parse::<Backend>().and_then(|backend| backend.connect(store.dir())).map_err(|e| e.to_string())?;
        }
        GardenerCmd::Status { json } => {
            let installed = Backend::installed();
            let prefs = CuratorPrefs::load(store.dir());
            let runs = store.gardener_runs(5).map_err(|e| e.to_string())?;
            let open = store.open_flag_count().map_err(|e| e.to_string())?;
            let pending_proposals = store.pending_curator_proposals(100).map_err(|e| e.to_string())?.len();
            if json {
                let runs = runs
                    .iter()
                    .map(|run| {
                        let mut value = run_json(run);
                        value["dismissed_flag_ids"] = json!(store.curator_dismissed_flag_ids(run.id)?);
                        Ok(value)
                    })
                    .collect::<feeder_core::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                let subscriptions: Vec<Value> = installed
                    .iter()
                    .map(|&b| {
                        let p = prefs.get(b);
                        json!({
                            "backend": b.as_str(),
                            "label": b.label(),
                            "name": prefs.name(b),
                            "enabled": p.enabled,
                            "default": prefs.default_backend() == Some(b),
                            "model": p.model,
                            "effort": p.effort,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    json!({
                        "backends": installed.iter().map(|b| b.as_str()).collect::<Vec<_>>(),
                        "default": prefs.default_backend().map(Backend::as_str),
                        "subscriptions": subscriptions,
                        "open_flags": open,
                        "pending_proposals": pending_proposals,
                        "runs": runs,
                    })
                );
            } else {
                let names: Vec<&str> = installed.iter().map(|b| b.label()).collect();
                println!("Installed CLIs: {}", if names.is_empty() { "none".into() } else { names.join(", ") });
                let enabled: Vec<String> = prefs
                    .enabled()
                    .into_iter()
                    .filter(|b| installed.contains(b))
                    .map(|b| {
                        let p = prefs.get(b);
                        let mut parts = vec![prefs.name(b)];
                        if let Some(m) = p.model {
                            parts.push(m);
                        }
                        if let Some(e) = p.effort {
                            parts.push(effort_label(&e));
                        }
                        if prefs.default_backend() == Some(b) { format!("{} (default)", parts.join(" · ")) } else { parts.join(" · ") }
                    })
                    .collect();
                println!(
                    "Enabled for Curator: {}",
                    if enabled.is_empty() { "none (turn one on in Settings → Curator)".into() } else { enabled.join(", ") }
                );
                println!("Open flags: {open}");
                if pending_proposals > 0 {
                    println!("Pending proposals: {pending_proposals}");
                }
                match runs.first() {
                    Some(r) => print_run(r),
                    None => println!("No runs yet. Start one with `feeder curator run`."),
                }
            }
        }
        GardenerCmd::Chats { limit, json } => {
            let chats = store.curator_chats(limit).map_err(|e| e.to_string())?;
            if json {
                println!("{}", Value::Array(chats.iter().map(chat_json).collect()));
            } else if chats.is_empty() {
                println!("No saved Curator chats.");
            } else {
                for chat in chats {
                    println!("#{} {} · {}", chat.run_id, terminal_text(&chat.title), ago(chat.started_at, now_ms()));
                }
            }
        }
        GardenerCmd::Chat { id, json } => {
            let events = store.curator_chat_events(id).map_err(|e| e.to_string())?;
            if json {
                let data = serde_json::to_string(&events).map_err(|e| e.to_string())?;
                println!("{data}");
            } else {
                print_saved_chat(events);
            }
        }
        GardenerCmd::DeleteChat { id } => {
            store.delete_curator_chat(id).map_err(|e| e.to_string())?;
            println!("Deleted Curator chat #{id}.");
        }
        GardenerCmd::Changes { limit, mine, json } => {
            let agent = mine.then_some(feeder_core::GARDENER);
            let changes = store.changes(agent, None, limit).map_err(|e| e.to_string())?;
            if json {
                println!("{}", Value::Array(changes.iter().map(change_json).collect()));
            } else if changes.is_empty() {
                println!("No Notebook changes logged yet.");
            } else {
                let now = now_ms();
                for c in &changes {
                    let reverted = if c.is_reverted() { "  (reverted)" } else { "" };
                    let reason = c.reason.as_deref().map(|r| format!(" — {r}")).unwrap_or_default();
                    println!(
                        "#{:<4} {} #{} \"{}\" · {} · {}{reason}{reverted}",
                        c.id,
                        c.action.label(),
                        c.page_id,
                        c.page_title,
                        c.agent,
                        ago(c.created_at, now)
                    );
                }
            }
        }
        GardenerCmd::Revert { id } => {
            let done = store.revert_change(id, HUMAN_AUTHOR).map_err(|e| e.to_string())?;
            let state = match done.change.action {
                feeder_core::ChangeAction::Archived => "archived",
                feeder_core::ChangeAction::Restored => "back in the Library",
                feeder_core::ChangeAction::Created | feeder_core::ChangeAction::Revised => "back to its earlier text",
            };
            println!("Reverted change #{id}: note #{} \"{}\" is {state}.", done.page.id, done.page.title);
        }
        GardenerCmd::Proposals { json } => {
            let proposals = store.pending_curator_proposals(50).map_err(|e| e.to_string())?;
            if json {
                println!("{}", Value::Array(proposals.iter().map(proposal_json).collect()));
            } else if proposals.is_empty() {
                println!("No pending Curator proposals.");
            } else {
                let now = now_ms();
                for p in &proposals {
                    let target = p.page_id.map(|id| format!("note #{id}")).unwrap_or_else(|| "new note".into());
                    println!(
                        "#{:<4} {} \"{}\" ({target}) · chat #{} · {} — {}",
                        p.id,
                        p.action.label(),
                        terminal_text(&p.title),
                        p.run_id,
                        ago(p.created_at, now),
                        terminal_text(&p.reason)
                    );
                }
            }
        }
        GardenerCmd::Apply { id, json } => {
            let done = store.apply_curator_proposal(id).map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    json!({
                        "proposal_id": id,
                        "change": change_json(&done.change),
                        "page": { "id": done.page.id, "title": done.page.title },
                    })
                );
            } else {
                println!("Applied proposal #{id}: note #{} \"{}\" (change #{}).", done.page.id, done.page.title, done.change.id);
            }
        }
        GardenerCmd::Dismiss { id, json } => {
            let proposal = store.dismiss_curator_proposal(id).map_err(|e| e.to_string())?;
            if json {
                println!("{}", proposal_json(&proposal));
            } else {
                println!("Dismissed proposal #{}: \"{}\".", proposal.id, proposal.title);
            }
        }
    }
    Ok(())
}

fn proposal_json(p: &Proposal) -> Value {
    json!({
        "id": p.id,
        "run_id": p.run_id,
        "action": p.action.as_str(),
        "page_id": p.page_id,
        "project_id": p.project_id,
        "kind": p.kind.as_str(),
        "topic": p.topic,
        "title": p.title,
        "tags": p.tags,
        "reason": p.reason,
        "source_flags": p.source_flags,
        "source_updates": p.source_updates,
        "status": p.status.as_str(),
        "change_id": p.change_id,
        "created_at": p.created_at,
    })
}

fn chat_json(chat: &ChatSummary) -> Value {
    let mode = match chat.mode {
        SessionMode::Ask => "ask",
        SessionMode::Review => "review",
    };
    json!({
        "run_id": chat.run_id,
        "started_at": chat.started_at,
        "mode": mode,
        "project_id": chat.scope.project_id(),
        "title": chat.title,
        "edits": chat.edits,
        "truncated": chat.truncated,
    })
}

fn print_saved_chat(events: Vec<StoredEvent>) {
    for event in events {
        match event {
            StoredEvent::You(text) => println!("You: {}", terminal_text(&text)),
            StoredEvent::Event(Event::Text(text)) => println!("{}", terminal_text(&text)),
            StoredEvent::Event(Event::ToolStarted(call)) => println!("  → {}", terminal_text(&call.name)),
            StoredEvent::Event(Event::ToolFinished { output, failed, .. }) => {
                let first = output.lines().next().unwrap_or("");
                println!("  {} {}", if failed { "✗" } else { "✓" }, terminal_text(first));
            }
            StoredEvent::Event(Event::NeedsSignIn) => println!("Sign-in required."),
            StoredEvent::Event(Event::Failed(text)) => println!("Curator: {}", terminal_text(&text)),
            StoredEvent::Event(Event::Started | Event::Thinking(_) | Event::TurnFinished | Event::Ended) => {}
        }
    }
}

fn terminal_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t').collect()
}

fn print_run(r: &Run) {
    let when = ago(r.started_at, now_ms());
    match (r.status, &r.error) {
        (RunStatus::Failed, Some(e)) => println!("Session #{} with {} failed ({when}): {e}", r.id, r.backend),
        (RunStatus::Running, _) => println!("Session #{} with {} is running (started {when}).", r.id, r.backend),
        _ => {
            println!("Session #{} with {} ({when}): {} change(s).", r.id, r.backend, r.edits);
            if let Some(s) = &r.summary {
                println!("{s}");
            }
        }
    }
}

fn chat(backend: Backend, env: SessionEnv, label: &str, scope: Scope, mode: SessionMode, opening: ChatOpening<'_>) -> Result<(), String> {
    let _signals = StopSignals::install().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let mut header = vec![label.to_string()];
    if let Some(model) = &env.run.model {
        header.push(model.clone());
    }
    if let Some(effort) = &env.run.effort {
        header.push(effort_label(effort));
    }
    let session = Session::start_curator_chat(backend, env, scope, mode, opening, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|e| e.to_string())?;
    let task = match mode {
        SessionMode::Ask => "Ask the Notebook",
        SessionMode::Review => "Review and update notes",
    };
    eprintln!("{task} · {}", header.join(" · "));
    let interactive = std::io::stdin().is_terminal();
    let mut out = std::io::stdout().lock();
    while let Some(event) = receive(&rx)? {
        match event {
            Event::Started => {}
            Event::Thinking(t) => write!(out, "\x1b[2m{}\x1b[0m", terminal_text(&t)).map_err(|e| e.to_string())?,
            Event::Text(t) => write!(out, "{}", terminal_text(&t)).map_err(|e| e.to_string())?,
            Event::ToolStarted(call) => {
                let input = call.input.to_string();
                writeln!(out, "\n  → {} {}", terminal_text(&call.name), terminal_text(&input)).map_err(|e| e.to_string())?;
            }
            Event::ToolFinished { output, failed, .. } => {
                let first = output.lines().next().unwrap_or("");
                writeln!(out, "  {} {}", if failed { "✗" } else { "✓" }, terminal_text(first)).map_err(|e| e.to_string())?;
            }
            Event::TurnFinished => {
                writeln!(out).map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                let next = if interactive { prompt()? } else { String::new() };
                if next.trim().is_empty() {
                    session.stop();
                } else {
                    session.send(next.trim()).map_err(|e| e.to_string())?;
                }
            }
            Event::NeedsSignIn => {
                return Err(format!(
                    "{} isn't signed in. Run `{}` in Terminal to sign in, then try again.",
                    backend.label(),
                    backend.sign_in_command()
                ));
            }
            Event::Failed(why) => return Err(terminal_text(&why)),
            Event::Ended => break,
        }
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prompt() -> Result<String, String> {
    eprint!("\nyou (Enter to finish) › ");
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("feeder-curator-input".into())
        .spawn(move || {
            let mut line = String::new();
            let result = std::io::stdin().lock().read_line(&mut line).map(|_| line).map_err(|e| e.to_string());
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    receive(&rx)?.unwrap_or_else(|| Err("Curator input closed".into()))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

fn receive<T>(rx: &mpsc::Receiver<T>) -> Result<Option<T>, String> {
    loop {
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Err("Curator stopped".into());
        }
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(value) => return Ok(Some(value)),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
        }
    }
}

struct StopSignals(Vec<(libc::c_int, libc::sigaction)>);

impl StopSignals {
    fn install() -> std::io::Result<Self> {
        INTERRUPTED.store(false, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let previous = swap_signal_handler(signal, stop_signal)?;
            guard.0.push((signal, previous));
        }
        Ok(guard)
    }
}

impl Drop for StopSignals {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            restore_signal_action(*signal, previous);
        }
    }
}

fn swap_signal_handler(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) -> std::io::Result<libc::sigaction> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler as libc::sighandler_t;
    let rc = unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, &mut previous)
    };
    if rc == 0 { Ok(previous) } else { Err(std::io::Error::last_os_error()) }
}

fn restore_signal_action(signal: libc::c_int, previous: &libc::sigaction) {
    unsafe {
        libc::sigaction(signal, previous, std::ptr::null_mut());
    }
}

fn flag_json(f: &Flag) -> Value {
    json!({
        "id": f.id,
        "kind": f.kind.as_str(),
        "page_id": f.page_id,
        "project_id": f.project_id,
        "agent": f.agent,
        "note": f.note,
        "suggestion": f.suggestion,
        "status": f.status.as_str(),
        "resolution": f.resolution,
        "change_id": f.change_id,
        "source_update_id": f.source_update_id,
        "created_at": f.created_at,
        "resolved_at": f.resolved_at,
    })
}

fn change_json(c: &Change) -> Value {
    json!({
        "id": c.id,
        "page_id": c.page_id,
        "page_title": c.page_title,
        "action": c.action.as_str(),
        "agent": c.agent,
        "reason": c.reason,
        "source_updates": c.source_updates,
        "source_flags": c.source_flags,
        "gardener_run_id": c.gardener_run_id,
        "revertible": c.is_revertible(),
        "created_at": c.created_at,
        "reverted_at": c.reverted_at,
    })
}

fn run_json(r: &Run) -> Value {
    json!({
        "id": r.id,
        "backend": r.backend,
        "project_id": r.scope.project_id(),
        "status": format!("{:?}", r.status).to_lowercase(),
        "started_at": r.started_at,
        "finished_at": r.finished_at,
        "summary": r.summary,
        "edits": r.edits,
        "error": r.error,
    })
}
