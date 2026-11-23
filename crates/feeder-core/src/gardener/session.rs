//! A live gardener session: the person's CLI as a child process, its stream turned into
//! [`Event`]s, follow-up messages, and Stop.
//!
//! Claude Code runs as one long-lived process that reads user messages on stdin. Codex runs one
//! process per turn and resumes the same thread for each follow-up.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::acp::{self, Action as AcpAction};
use super::chat::{ChatOpening, Recorder, StoredEvent};
use super::parse::{StreamState, claude_line, codex_line, failure};
use super::prefs::{self, RunConfig};
use super::{Backend, Scope, SessionMode, clip, profile};
use crate::{Error, Result, Store, data_dir, setup};

/// Something the session did, in the order it happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    /// The CLI is up and connected to the gardener's tools.
    Started,
    /// A piece of the agent's reasoning, as the CLI shares it.
    Thinking(String),
    /// A piece of the agent's reply. Consecutive pieces belong to the same paragraph.
    Text(String),
    ToolStarted(ToolCall),
    ToolFinished {
        id: String,
        output: String,
        failed: bool,
    },
    /// The agent finished answering; the person can write again.
    TurnFinished,
    /// The CLI isn't signed in. The person signs in with [`Backend::sign_in_command`].
    NeedsSignIn,
    Failed(String),
    /// The session is over, by Stop or because the CLI exited.
    Ended,
}

/// One call to a gardener tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// The tool's own name, such as `write_page`.
    pub name: String,
    pub input: Value,
}

impl ToolCall {
    /// A text argument, if the call has it.
    pub fn text_arg(&self, key: &str) -> Option<&str> {
        self.input.get(key).and_then(Value::as_str)
    }

    /// A whole-number argument, if the call has it.
    pub fn int_arg(&self, key: &str) -> Option<i64> {
        self.input.get(key).and_then(Value::as_i64)
    }
}

/// A made-up session for screenshots and the app's snapshot scene, on the demo data from
/// `scripts/site/demo-data.sh`. It is never shown as real activity.
pub fn demo_events() -> Vec<Event> {
    let call = |id: &str, name: &str, input: Value| Event::ToolStarted(ToolCall { id: id.into(), name: name.into(), input });
    let done = |id: &str, output: &str| Event::ToolFinished { id: id.into(), output: output.into(), failed: false };
    vec![
        Event::Started,
        Event::Thinking("Two open flags. One has a suggestion I can use; the other has nothing to go on yet.".into()),
        call("t1", "list_flags", json!({})),
        call("t2", "read_traces", json!({ "limit": 20 })),
        done("t1", "- flag #1 missing: Nothing on checking the changelog before a release"),
        done("t2", "## update #5 · Orbit · Claude Code"),
        call("t3", "search_notebook", json!({ "query": "changelog" })),
        done("t3", "Nothing matches “changelog”."),
        Event::Text("Nothing on changelogs yet, so I'm adding a page from flag 1's suggestion.".into()),
        call("t4", "write_page", json!({ "title": "Check the changelog before a release", "flags": [1] })),
        done("t4", r#"{"change_id":1,"action":"created"}"#),
        call("t5", "search_notebook", json!({ "query": "appcast" })),
        done("t5", "Nothing matches “appcast”."),
        Event::Text("I added **Check the changelog before a release**, based on flag 1's suggestion: run `scripts/changelog.sh --check` after editing `release-notes/`.\n\nThe appcast flag stays open. Nothing in the notebook or the Feed says how signing works, so I left it for someone who knows.".into()),
        Event::TurnFinished,
    ]
}

/// Where a session finds Feeder: the CLI that serves the gardener's tools, and the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEnv {
    pub feeder: PathBuf,
    pub data_dir: PathBuf,
    /// The model and thinking level the CLI runs with.
    pub run: RunConfig,
}

impl SessionEnv {
    /// This Mac's Feeder: the CLI beside the running binary (a development build) or the one
    /// [`setup::feeder_binary`] picks (the app's bundled copy), and the active data folder.
    pub fn detect() -> SessionEnv {
        let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("feeder"))).filter(|p| p.is_file());
        SessionEnv { feeder: sibling.unwrap_or_else(setup::feeder_binary), data_dir: data_dir(), run: RunConfig::default() }
    }
}

/// Codex features that give the model a tool of its own. A session turns all of them off, so
/// its only tools are the gardener's.
const CODEX_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "apps",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "view_image",
    "hooks",
    "plugins",
    "remote_plugin",
    "multi_agent",
    "goals",
    "memories",
    "skill_mcp_dependency_install",
];

/// How much stderr to keep for an error message.
const STDERR_CHARS: usize = 400;
const ACP_FRAME_BYTES: u64 = 2 * 1024 * 1024;

type Sink = Box<dyn Fn(Event) + Send + Sync>;

/// A running session. Dropping it stops the session.
pub struct Session {
    backend: Backend,
    inner: Arc<Inner>,
}

struct Inner {
    backend: Backend,
    mode: SessionMode,
    env: SessionEnv,
    sink: Sink,
    run_id: i64,
    scratch: Scratch,
    child: Mutex<Option<Child>>,
    /// Claude Code's stdin, kept open for follow-up messages.
    stdin: Mutex<Option<ChildStdin>>,
    state: Mutex<StreamState>,
    acp: Mutex<acp::State>,
    recording: Mutex<Recorder>,
    send_lock: Mutex<()>,
    /// The agent's latest reply, for the run log's summary.
    last_reply: Mutex<String>,
    stopping: AtomicBool,
    ended: AtomicBool,
}

struct ChildPipes {
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

type ProcessMonitor = JoinHandle<std::io::Result<Option<ExitStatus>>>;

impl Session {
    /// Starts a session with `opening` as the person's first message. Events arrive on `sink`
    /// from a background thread.
    ///
    /// # Errors
    /// [`Error::NotFound`] when the backend isn't installed, [`Error::Invalid`] when a session is
    /// already running, and I/O errors when the CLI can't start.
    pub fn start(backend: Backend, env: SessionEnv, opening: &str, sink: impl Fn(Event) + Send + Sync + 'static) -> Result<Session> {
        Self::start_scoped(backend, env, Scope::Notebook, opening, sink)
    }

    /// Starts a session whose tools can only access the given scope.
    ///
    /// # Errors
    /// Returns an error for an unavailable provider or project, an active session, or a failed launch.
    pub fn start_scoped(
        backend: Backend,
        env: SessionEnv,
        scope: Scope,
        opening: &str,
        sink: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Session> {
        Self::start_curator(backend, env, scope, SessionMode::Review, opening, sink)
    }

    /// Starts Curator with a fixed scope and set of Notebook permissions.
    ///
    /// # Errors
    /// Returns an error for an unavailable provider or project, an active session, or a failed launch.
    pub fn start_curator(
        backend: Backend,
        env: SessionEnv,
        scope: Scope,
        mode: SessionMode,
        opening: &str,
        sink: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Session> {
        Self::start_curator_chat(backend, env, scope, mode, ChatOpening::Message(opening), sink)
    }

    /// Starts a saved Curator chat with a typed message or a named job.
    ///
    /// # Errors
    /// Returns an error for an unavailable provider or project, an active session, or failure
    /// to save or start the chat.
    pub fn start_curator_chat(
        backend: Backend,
        env: SessionEnv,
        scope: Scope,
        mode: SessionMode,
        opening: ChatOpening<'_>,
        sink: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Session> {
        env.run.validate()?;
        let bin = backend.binary().ok_or_else(|| Error::NotFound(format!("{} isn't installed", backend.label())))?;
        let scratch = Scratch::new()?;
        let store = Store::open(&env.data_dir)?;
        let run_id = store.claim_curator_run(backend, scope, mode, Some(opening))?;
        let inner = Arc::new(Inner {
            backend,
            mode,
            env,
            sink: Box::new(sink),
            run_id,
            scratch,
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            state: Mutex::new(StreamState::default()),
            acp: Mutex::new(acp::State::default()),
            recording: Mutex::new(Recorder::new(store, run_id)),
            send_lock: Mutex::new(()),
            last_reply: Mutex::new(String::new()),
            stopping: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        });
        let prompt = opening.prompt().to_string();
        let worker = Arc::clone(&inner);
        if let Err(e) = std::thread::Builder::new().name("feeder-curator-start".into()).spawn(move || {
            let started = match backend {
                Backend::ClaudeCode => worker.start_claude(&bin).and_then(|()| worker.send_claude(&prompt)),
                Backend::Codex => worker.codex_turn(&bin, &format!("{}\n\n{prompt}", mode.instructions())),
                Backend::OpenCode | Backend::Antigravity => worker.start_acp(&bin, &prompt),
            };
            if let Err(e) = started {
                worker.terminate();
                if !worker.stopping.load(Ordering::Acquire) && !worker.ended.load(Ordering::Acquire) {
                    worker.forward(failure(e.to_string()));
                    worker.end(Some(&e.to_string()));
                }
            }
        }) {
            inner.end(Some(&e.to_string()));
            return Err(e.into());
        }
        Ok(Session { backend, inner })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Saves and sends the person's follow-up. Call it after [`Event::TurnFinished`].
    ///
    /// # Errors
    /// Returns an error when the session has ended, the message cannot be saved, or submission fails.
    pub fn send(&self, text: &str) -> Result<()> {
        let _send = lock(&self.inner.send_lock);
        if self.inner.stopping.load(Ordering::Acquire) || self.inner.ended.load(Ordering::SeqCst) {
            return Err(Error::Invalid("this gardener session has ended; start a new one".into()));
        }
        let sent = match self.backend {
            Backend::ClaudeCode => {
                self.inner.record_user(text)?;
                self.inner.send_claude(text)
            }
            Backend::Codex => {
                if lock(&self.inner.child).is_some() {
                    return Err(Error::Invalid("the gardener is still answering".into()));
                }
                let bin = self.backend.binary().ok_or_else(|| Error::NotFound(format!("{} isn't installed", self.backend.label())))?;
                self.inner.record_user(text)?;
                self.inner.codex_turn(&bin, text)
            }
            Backend::OpenCode | Backend::Antigravity => {
                if !lock(&self.inner.acp).ready() {
                    return Err(Error::Invalid("Curator is still connecting or answering".into()));
                }
                self.inner.record_user(text)?;
                let prompt = lock(&self.inner.acp).prompt(text)?;
                self.inner.send_acp(&prompt)
            }
        };
        if let Err(e) = &sent {
            self.inner.record_event(&Event::Failed(e.to_string()));
        }
        sent
    }

    /// The stored session ID for looking up its committed changes.
    pub fn run_id(&self) -> i64 {
        self.inner.run_id
    }

    /// Ends the session now, stopping the CLI mid-turn if need be.
    pub fn stop(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.terminate();
        self.inner.end(None);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Inner {
    fn start_acp(self: &Arc<Self>, bin: &Path, opening: &str) -> Result<()> {
        let helper = std::fs::canonicalize(&self.env.feeder)?;
        let data_dir = std::fs::canonicalize(&self.env.data_dir)?;
        let server = json!({
            "name": "feeder", "command": helper, "args": ["mcp", "--gardener", "--gardener-run", self.run_id.to_string()],
            "env": [{ "name": "FEEDER_HOME", "value": data_dir }],
        });
        let mut probe = profile::command(self.backend, bin, &self.env.data_dir)?;
        probe.arg("--version");
        let (pipes, monitor) = self.spawn_provider(probe, false, Some(Duration::from_secs(5)))?;
        drop(pipes.stdin);
        let probe_err = pipes.stderr.map(|mut stderr| {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            })
        });
        let mut version = String::new();
        pipes.stdout.ok_or_else(|| Error::Invalid("The provider has no version output".into()))?.take(512).read_to_string(&mut version)?;
        let status = process_status(monitor)?;
        if let Some(t) = probe_err {
            let _ = t.join();
        }
        acp::check_version(self.backend, status, &version)?;
        let cmd = acp::command(self.backend, bin, &self.env.data_dir, self.scratch.path(), self.mode, &server, &self.env.run)?;
        match self.backend {
            Backend::Antigravity => {
                let mut acp = lock(&self.acp);
                acp.model = prefs::antigravity_model(&self.env.run);
                if acp.model.is_none() {
                    acp.effort = self.env.run.effort.clone();
                }
            }
            Backend::OpenCode => {
                let mut acp = lock(&self.acp);
                acp.model = self.env.run.model.clone();
                acp.effort = self.env.run.effort.clone();
            }
            _ => {}
        }
        let servers = if self.backend == Backend::Antigravity { json!([]) } else { json!([server]) };
        let (pipes, monitor) = self.spawn_provider(cmd, true, None)?;
        let stdout = pipes.stdout.ok_or_else(|| Error::Invalid("The provider has no ACP output".into()))?;
        let opening = format!("{}\n\n{opening}", self.mode.instructions());
        let inner = Arc::clone(self);
        let worker = std::thread::Builder::new().name("feeder-curator-acp".into()).spawn(move || {
            let errors = pipes.stderr.map(|stderr| std::thread::spawn(move || stderr_tail(stderr)));
            let outcome = inner.read_acp(BufReader::new(stdout), &servers, &opening);
            inner.terminate();
            let _ = process_status(monitor);
            let stderr = errors.and_then(|t| t.join().ok()).unwrap_or_default();
            if let Err(e) = outcome
                && !inner.stopping.load(Ordering::Acquire)
            {
                let error = acp_error_message(&e, &stderr);
                inner.forward(failure(error.clone()));
                inner.end(Some(&error));
            }
        });
        if let Err(error) = worker {
            self.terminate();
            return Err(error.into());
        }
        self.send_acp(&acp::State::initialize())
    }

    fn send_acp(&self, message: &Value) -> Result<()> {
        let mut stdin = lock(&self.stdin);
        let pipe = stdin.as_mut().ok_or_else(|| Error::Invalid("The provider has exited".into()))?;
        writeln!(pipe, "{message}")?;
        pipe.flush()?;
        Ok(())
    }

    fn read_acp(&self, mut stdout: impl BufRead, servers: &Value, opening: &str) -> Result<()> {
        let mut line = String::new();
        while !self.ended.load(Ordering::Acquire) {
            line.clear();
            let len = (&mut stdout).take(ACP_FRAME_BYTES + 1).read_line(&mut line)?;
            if len == 0 {
                return Err(Error::Invalid(format!("{} closed the Curator session", self.backend.label())));
            }
            if u64::try_from(len).unwrap_or(u64::MAX) > ACP_FRAME_BYTES {
                return Err(Error::Invalid("An ACP message exceeded the Curator size limit".into()));
            }
            if line.trim().is_empty() {
                continue;
            }
            let message = serde_json::from_str(&line).map_err(|_| Error::Invalid("The provider returned an invalid ACP message".into()))?;
            let actions = lock(&self.acp).receive(message, self.mode, self.scratch.path(), servers, opening)?;
            for action in actions {
                match action {
                    AcpAction::Send(message) => self.send_acp(&message)?,
                    AcpAction::Emit(event) => self.forward(event),
                }
            }
        }
        Ok(())
    }

    fn record_user(&self, text: &str) -> Result<()> {
        let mut recording = lock(&self.recording);
        if self.stopping.load(Ordering::Acquire) || self.ended.load(Ordering::SeqCst) {
            return Err(Error::Invalid("this Curator session has ended".into()));
        }
        recording.record(StoredEvent::You(text.to_string()))?;
        lock(&self.last_reply).clear();
        Ok(())
    }

    fn start_claude(self: &Arc<Self>, bin: &Path) -> Result<()> {
        let helper = resolve_path(&self.env.feeder)?;
        let data_dir = resolve_path(&self.env.data_dir)?;
        let config = json!({ "mcpServers": { "feeder": {
            "command": helper,
            "args": ["mcp", "--gardener", "--gardener-run", &self.run_id.to_string()],
            "env": { "FEEDER_HOME": data_dir },
        } } });
        let config_path = self.scratch.path().join("mcp.json");
        std::fs::write(&config_path, config.to_string())?;
        let mut cmd = profile::command(self.backend, bin, &self.env.data_dir)?;
        cmd.current_dir(self.scratch.path())
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
            // No built-in tools, only the gardener's server, none of the person's settings or hooks.
            .args(["--restricted", "--tools", "", "--strict-mcp-config", "--allowedTools", "mcp__feeder", "--setting-sources", ""])
            .args(["--settings", r#"{"disableAllHooks":true}"#])
            .args(["--no-session-persistence", "--append-system-prompt", self.mode.instructions()])
            .arg("--mcp-config")
            .arg(&config_path);
        if let Some(model) = &self.env.run.model {
            cmd.args(["--model", model]);
        }
        if let Some(effort) = &self.env.run.effort {
            cmd.args(["--effort", effort]);
        }
        let (pipes, monitor) = self.spawn_provider(cmd, true, None)?;
        self.watch(pipes, monitor, claude_line)
    }

    fn send_claude(&self, text: &str) -> Result<()> {
        let line = json!({ "type": "user", "message": { "role": "user", "content": text } });
        let mut stdin = lock(&self.stdin);
        let pipe = stdin.as_mut().ok_or_else(|| Error::Invalid("Claude Code has exited".into()))?;
        writeln!(pipe, "{line}")?;
        pipe.flush()?;
        Ok(())
    }

    fn codex_turn(self: &Arc<Self>, bin: &Path, text: &str) -> Result<()> {
        let helper = resolve_path(&self.env.feeder)?;
        let data_dir = resolve_path(&self.env.data_dir)?;
        let thread = lock(&self.state).thread_id.clone();
        let mut cmd = profile::command(self.backend, bin, &self.env.data_dir)?;
        cmd.current_dir(self.scratch.path()).arg("exec");
        if let Some(thread) = &thread {
            cmd.args(["resume", thread]);
        }
        cmd.args(["--json", "--skip-git-repo-check", "--ignore-user-config"]);
        for &feature in CODEX_DISABLED_FEATURES {
            cmd.args(["--disable", feature]);
        }
        // One inline table replaces the person's own MCP servers for this run.
        let server = format!(
            "mcp_servers={{feeder={{command={},args=[\"mcp\",\"--gardener\",\"--gardener-run\",\"{}\"],env={{FEEDER_HOME={}}},default_tools_approval_mode=\"approve\"}}}}",
            toml_string(&helper.to_string_lossy()),
            self.run_id,
            toml_string(&data_dir.to_string_lossy()),
        );
        cmd.args([
            "-c",
            &server,
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "sandbox_mode=\"read-only\"",
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "model_reasoning_summary=\"auto\"",
        ]);
        if let Some(model) = &self.env.run.model {
            cmd.args(["-m", model]);
        }
        if let Some(effort) = &self.env.run.effort {
            cmd.args(["-c", &format!("model_reasoning_effort={}", toml_string(effort))]);
        }
        cmd.arg("-");
        let (mut pipes, monitor) = self.spawn_provider(cmd, false, None)?;
        let mut stdin = pipes.stdin.take().ok_or_else(|| Error::Invalid("Codex has no input".into()))?;
        self.watch(pipes, monitor, codex_line)?;
        stdin.write_all(text.as_bytes())?;
        Ok(())
    }

    /// Reads the child's stream on a thread and forwards events until it exits.
    fn watch(self: &Arc<Self>, pipes: ChildPipes, monitor: ProcessMonitor, parse: fn(&str, &mut StreamState) -> Vec<Event>) -> Result<()> {
        let inner = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("feeder-gardener-stream".into()).spawn(move || {
            let errors = pipes.stderr.map(|e| std::thread::spawn(move || stderr_tail(e)));
            let mut turn_done = false;
            let mut stream_error: Option<String> = None;
            let mut needs_sign_in = false;
            for line in pipes.stdout.into_iter().flat_map(|out| BufReader::new(out).lines().map_while(std::io::Result::ok)) {
                let events = parse(&line, &mut lock(&inner.state));
                for event in events {
                    match &event {
                        Event::TurnFinished => {
                            turn_done = true;
                            if inner.backend == Backend::Codex {
                                continue;
                            }
                        }
                        Event::Failed(why) => stream_error = Some(why.clone()),
                        Event::NeedsSignIn => needs_sign_in = true,
                        _ => {}
                    }
                    inner.forward(event);
                }
            }
            let status = process_status(monitor);
            let stderr = errors.and_then(|t| t.join().ok()).unwrap_or_default();
            if inner.stopping.load(Ordering::Acquire) || inner.ended.load(Ordering::SeqCst) {
                return;
            }
            let crashed = !status.is_ok_and(|status| status.is_some_and(|s| s.success()));
            match inner.backend {
                // Claude Code exiting ends the session.
                Backend::ClaudeCode => {
                    let already_reported = needs_sign_in || stream_error.is_some();
                    let error = if needs_sign_in {
                        Some(format!("{} isn't signed in", inner.backend.label()))
                    } else if stream_error.is_some() {
                        stream_error
                    } else {
                        crashed.then(|| last_lines(&stderr))
                    };
                    if !already_reported && let Some(e) = &error {
                        inner.forward(failure(e.clone()));
                    }
                    inner.end(error.as_deref());
                }
                // Codex exits after every turn; only a turn that never finished is a failure.
                Backend::Codex if crashed || !turn_done => {
                    let already_reported = needs_sign_in || stream_error.is_some();
                    let error = if needs_sign_in {
                        format!("{} isn't signed in", inner.backend.label())
                    } else if let Some(why) = stream_error {
                        why
                    } else if crashed {
                        last_lines(&stderr)
                    } else {
                        "Codex exited before finishing its answer".into()
                    };
                    if !already_reported {
                        inner.forward(failure(error.clone()));
                    }
                    inner.end(Some(&error));
                }
                Backend::Codex => inner.forward(Event::TurnFinished),
                Backend::OpenCode | Backend::Antigravity => unreachable!("ACP has its own transport"),
            }
        });
        if let Err(e) = spawned {
            self.terminate();
            return Err(e.into());
        }
        Ok(())
    }

    fn forward(&self, event: Event) {
        if self.record_event(&event) {
            (self.sink)(event);
        }
    }

    fn record_event(&self, event: &Event) -> bool {
        let mut recording = lock(&self.recording);
        if self.stopping.load(Ordering::Acquire) || self.ended.load(Ordering::SeqCst) {
            return false;
        }
        match event {
            Event::Text(piece) => lock(&self.last_reply).push_str(piece),
            Event::ToolStarted(_) => lock(&self.last_reply).clear(),
            _ => {}
        }
        if recording.record(StoredEvent::Event(event.clone())).is_err() && !recording.warned {
            recording.warned = true;
            drop(recording);
            (self.sink)(Event::Failed("couldn't save the full Curator chat".into()));
        }
        true
    }

    fn spawn_provider(
        self: &Arc<Self>,
        mut cmd: Command,
        keep_stdin: bool,
        timeout: Option<Duration>,
    ) -> Result<(ChildPipes, ProcessMonitor)> {
        let mut slot = lock(&self.child);
        if self.stopping.load(Ordering::Acquire) || self.ended.load(Ordering::Acquire) || slot.is_some() {
            return Err(Error::Invalid("Curator cannot start another provider process".into()));
        }
        let mut child = cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut pipes = ChildPipes { stdin: child.stdin.take(), stdout: child.stdout.take(), stderr: child.stderr.take() };
        if keep_stdin {
            *lock(&self.stdin) = pipes.stdin.take();
        }
        let id = child.id();
        *slot = Some(child);
        let inner = Arc::clone(self);
        let monitor = std::thread::Builder::new().name("feeder-curator-process".into()).spawn(move || inner.monitor(id, timeout));
        match monitor {
            Ok(monitor) => Ok((pipes, monitor)),
            Err(error) => {
                drop(slot);
                self.terminate();
                Err(error.into())
            }
        }
    }

    fn monitor(&self, id: u32, timeout: Option<Duration>) -> std::io::Result<Option<ExitStatus>> {
        let started = Instant::now();
        loop {
            {
                let mut slot = lock(&self.child);
                let Some(child) = slot.as_mut().filter(|child| child.id() == id) else { return Ok(None) };
                if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                    let _ = terminate_child(child);
                    *slot = None;
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider startup timed out"));
                }
                if child_exited(id)? {
                    let status = terminate_child(child);
                    *slot = None;
                    return status.map(Some);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn terminate(&self) {
        {
            let mut slot = lock(&self.child);
            if let Some(child) = slot.as_mut() {
                let _ = terminate_child(child);
                *slot = None;
            }
        }
        // A writer may be blocked in the pipe. Kill the reader before trying to close it.
        *lock(&self.stdin) = None;
    }

    /// Logs the end and tells the view, once.
    fn end(&self, error: Option<&str>) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        // The process may exit before the stream thread lets go of `Inner`, so don't wait for Drop.
        let _ = std::fs::remove_dir_all(self.scratch.path());
        let failed = {
            let mut recording = lock(&self.recording);
            let summary = lock(&self.last_reply);
            recording.finish((self.mode == SessionMode::Review).then_some(summary.as_str()), error).is_err()
        };
        if failed {
            (self.sink)(Event::Failed("couldn't finish saving the Curator chat".into()));
        }
        (self.sink)(Event::Ended);
    }
}

fn process_status(monitor: ProcessMonitor) -> std::io::Result<Option<ExitStatus>> {
    monitor.join().map_err(|_| std::io::Error::other("provider monitor stopped unexpectedly"))?
}

pub(super) fn child_exited(id: u32) -> std::io::Result<bool> {
    // SAFETY: zero is valid for siginfo_t and provides a zero pid when no child is ready.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is writable, id belongs to an owned child, and WNOWAIT leaves the leader
    // unreaped so its process group can be signalled before its PID becomes reusable.
    let result = unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    if result == 0 {
        // SAFETY: waitid populates this field on exit and leaves the zero pid for no result.
        return Ok(unsafe { info.si_pid() } != 0);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::Interrupted { Ok(false) } else { Err(error) }
}

pub(super) fn terminate_child(child: &mut Child) -> std::io::Result<ExitStatus> {
    if let Ok(group) = libc::pid_t::try_from(child.id())
        && group > 0
    {
        // SAFETY: every launched provider leads its own group; the caller exclusively owns the
        // Child and has not yet reaped it, so the group ID cannot be reused before kill.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    child.wait()
}

fn stderr_tail(mut reader: impl Read) -> String {
    let mut tail = Vec::new();
    let mut chunk = [0; 1024];
    while let Ok(count) = reader.read(&mut chunk) {
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..count]);
        tail.drain(..tail.len().saturating_sub(STDERR_CHARS * 4));
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Lock poisoning means a stream thread panicked; the session can't continue sensibly.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().expect("gardener session lock poisoned")
}

fn resolve_path(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).or_else(|_| std::path::absolute(path))
}

fn acp_error_message(err: &Error, stderr: &str) -> String {
    let message = err.to_string();
    let generic = message == "The provider returned an invalid ACP message" || message.ends_with(" closed the Curator session");
    if generic && let Some(detail) = non_empty_last_lines(stderr) {
        return detail;
    }
    message
}

fn non_empty_last_lines(stderr: &str) -> Option<String> {
    let tail: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
    let text = tail.into_iter().rev().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| clip(&text, STDERR_CHARS))
}

fn last_lines(stderr: &str) -> String {
    non_empty_last_lines(stderr).unwrap_or_else(|| "the CLI stopped unexpectedly".into())
}

/// A TOML basic string, for paths in Codex's `-c` overrides.
fn toml_string(s: &str) -> String {
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

/// An empty folder the CLI runs in, so it can't pick up a project's instructions. Removed on drop.
struct Scratch(PathBuf);

/// The prefix of every scratch folder, followed by the owning process id.
const SCRATCH_PREFIX: &str = "feeder-gardener-";

impl Scratch {
    fn new() -> Result<Scratch> {
        sweep_orphans();
        let dir = std::env::temp_dir().join(format!("{SCRATCH_PREFIX}{}-{}", std::process::id(), fastrand::u32(..)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Scratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

/// Removes scratch folders whose process is gone (an app force-quit mid-session).
fn sweep_orphans() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let owner = name.to_str().and_then(|n| n.strip_prefix(SCRATCH_PREFIX)).and_then(|rest| rest.split('-').next()?.parse::<i64>().ok());
        if owner.is_some_and(|pid| !super::process_alive(pid)) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Session>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_strings_escape_quotes_and_backslashes() {
        assert_eq!(toml_string(r#"/Users/a "b"\c"#), r#""/Users/a \"b\"\\c""#);
    }

    #[test]
    fn stderr_tails_are_short_and_never_empty() {
        assert_eq!(last_lines(""), "the CLI stopped unexpectedly");
        assert_eq!(last_lines("one\n\ntwo\nthree\nfour\n"), "two three four");
        let output = format!("{}\nlast diagnostic", "x".repeat(16_384));
        let tail = stderr_tail(output.as_bytes());
        assert!(tail.len() <= STDERR_CHARS * 4);
        assert!(tail.ends_with("last diagnostic"));
    }

    #[test]
    fn acp_error_surfaces_stderr_for_generic_transport_failures() {
        let invalid = Error::Invalid("The provider returned an invalid ACP message".into());
        assert_eq!(acp_error_message(&invalid, "ERROR\nUnrecognized flag: --standalone\n"), "ERROR Unrecognized flag: --standalone");
        assert_eq!(acp_error_message(&invalid, "   \n"), "The provider returned an invalid ACP message");
        let closed = Error::Invalid("OpenCode closed the Curator session".into());
        assert_eq!(acp_error_message(&closed, "401 Unauthorized\n"), "401 Unauthorized");
        let specific = Error::Invalid("Unknown model ID: bad".into());
        assert_eq!(acp_error_message(&specific, "noisy debug log\n"), "Unknown model ID: bad");
    }

    #[test]
    fn reaping_the_provider_closes_pipes_held_by_its_children() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 5 & printf 'provider finished\\n'"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !child_exited(child.id()).unwrap() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(terminate_child(&mut child).unwrap().success());
        let mut output = String::new();
        child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
        assert_eq!(output, "provider finished\n");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
