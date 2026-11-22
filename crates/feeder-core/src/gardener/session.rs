//! A live gardener session: the person's CLI as a child process, its stream turned into
//! [`Event`]s, follow-up messages, and Stop.
//!
//! Claude Code runs as one long-lived process that reads user messages on stdin. Codex runs one
//! process per turn and resumes the same thread for each follow-up.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::acp::{self, Action as AcpAction};
use super::chat::{ChatOpening, Recorder, StoredEvent};
use super::parse::{StreamState, claude_line, codex_line, failure};
use super::{Backend, Scope, SessionMode, clip};
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
}

impl SessionEnv {
    /// This Mac's Feeder: the CLI beside the running binary (a development build) or the one
    /// [`setup::feeder_binary`] picks (the app's bundled copy), and the active data folder.
    pub fn detect() -> SessionEnv {
        let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("feeder"))).filter(|p| p.is_file());
        SessionEnv { feeder: sibling.unwrap_or_else(setup::feeder_binary), data_dir: data_dir() }
    }
}

/// Codex features that give the model a tool of its own. A session turns all of them off, so
/// its only tools are the gardener's.
const CODEX_TOOL_FEATURES: [&str; 7] =
    ["shell_tool", "unified_exec", "apps", "browser_use", "browser_use_external", "in_app_browser", "view_image"];

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
    ended: AtomicBool,
}

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
            ended: AtomicBool::new(false),
        });
        let prompt = opening.prompt();
        let started = match backend {
            Backend::ClaudeCode => inner.start_claude(&bin).and_then(|()| inner.send_claude(prompt)),
            Backend::Codex => inner.codex_turn(&bin, &format!("{}\n\n{prompt}", mode.instructions())),
            Backend::OpenCode | Backend::Antigravity => inner.start_acp(&bin, prompt),
        };
        if let Err(e) = started {
            let _ = lock(&inner.recording).record(StoredEvent::Event(Event::Failed(e.to_string())));
            inner.kill();
            inner.end(Some(&e.to_string()));
            return Err(e);
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
        if self.inner.ended.load(Ordering::SeqCst) {
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
        self.inner.kill();
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
        let mut cmd = acp::command(self.backend, bin, &self.env.data_dir, self.scratch.path(), self.mode, &server)?;
        cmd.process_group(0);
        let servers = if self.backend == Backend::Antigravity { json!([]) } else { json!([server]) };
        let mut child = spawn(cmd)?;
        *lock(&self.stdin) = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *lock(&self.child) = Some(child);
        let stdout = stdout.ok_or_else(|| Error::Invalid("The provider has no ACP output".into()))?;
        let opening = format!("{}\n\n{opening}", self.mode.instructions());
        let inner = Arc::clone(self);
        let worker = std::thread::Builder::new().name("feeder-curator-acp".into()).spawn(move || {
            let errors = stderr.map(|mut stderr| {
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut stderr, &mut std::io::sink());
                })
            });
            let outcome = inner.read_acp(BufReader::new(stdout), &servers, &opening);
            inner.kill();
            let child = lock(&inner.child).take();
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            if let Some(errors) = errors {
                let _ = errors.join();
            }
            if let Err(e) = outcome {
                let error = e.to_string();
                inner.forward(failure(error.clone()));
                inner.end(Some(&error));
            }
        });
        if let Err(error) = worker {
            self.kill();
            if let Some(mut child) = lock(&self.child).take() {
                let _ = child.wait();
            }
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
        if self.ended.load(Ordering::SeqCst) {
            return Err(Error::Invalid("this Curator session has ended".into()));
        }
        recording.record(StoredEvent::You(text.to_string()))?;
        lock(&self.last_reply).clear();
        Ok(())
    }

    fn start_claude(self: &Arc<Self>, bin: &Path) -> Result<()> {
        let config = json!({ "mcpServers": { "feeder": {
            "command": self.env.feeder,
            "args": ["mcp", "--gardener", "--gardener-run", &self.run_id.to_string()],
            "env": { "FEEDER_HOME": self.env.data_dir },
        } } });
        let config_path = self.scratch.path().join("mcp.json");
        std::fs::write(&config_path, config.to_string())?;
        let mut cmd = Command::new(bin);
        cmd.current_dir(self.scratch.path())
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
            // No built-in tools, only the gardener's server, none of the person's settings or hooks.
            .args(["--tools", "", "--strict-mcp-config", "--allowedTools", "mcp__feeder", "--setting-sources", ""])
            .args(["--no-session-persistence", "--append-system-prompt", self.mode.instructions()])
            .arg("--mcp-config")
            .arg(&config_path);
        let mut child = spawn(cmd)?;
        *lock(&self.stdin) = child.stdin.take();
        self.watch(child, claude_line);
        Ok(())
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
        let thread = lock(&self.state).thread_id.clone();
        let mut cmd = Command::new(bin);
        cmd.current_dir(self.scratch.path()).arg("exec");
        if let Some(thread) = &thread {
            cmd.args(["resume", thread]);
        }
        cmd.args(["--json", "--skip-git-repo-check"]);
        for feature in CODEX_TOOL_FEATURES {
            cmd.args(["--disable", feature]);
        }
        // One inline table replaces the person's own MCP servers for this run.
        let server = format!(
            "mcp_servers={{feeder={{command={},args=[\"mcp\",\"--gardener\",\"--gardener-run\",\"{}\"],env={{FEEDER_HOME={}}},default_tools_approval_mode=\"approve\"}}}}",
            toml_string(&self.env.feeder.to_string_lossy()),
            self.run_id,
            toml_string(&self.env.data_dir.to_string_lossy()),
        );
        cmd.args([
            "-c",
            &server,
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "sandbox_mode=\"read-only\"",
            "-c",
            "model_reasoning_summary=\"auto\"",
        ]);
        cmd.arg("-");
        let mut child = spawn(cmd)?;
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Invalid("Codex has no input".into()))?;
        stdin.write_all(text.as_bytes())?;
        drop(stdin);
        self.watch(child, codex_line);
        Ok(())
    }

    /// Reads the child's stream on a thread and forwards events until it exits.
    fn watch(self: &Arc<Self>, mut child: Child, parse: fn(&str, &mut StreamState) -> Vec<Event>) {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *lock(&self.child) = Some(child);
        let inner = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("feeder-gardener-stream".into()).spawn(move || {
            let errors = stderr.map(|mut e| {
                std::thread::spawn(move || {
                    let mut text = String::new();
                    let _ = e.read_to_string(&mut text);
                    text
                })
            });
            let mut turn_done = false;
            for line in stdout.into_iter().flat_map(|out| BufReader::new(out).lines().map_while(std::io::Result::ok)) {
                let events = parse(&line, &mut lock(&inner.state));
                for event in events {
                    if matches!(event, Event::TurnFinished) {
                        turn_done = true;
                        if inner.backend == Backend::Codex {
                            continue;
                        }
                    }
                    inner.forward(event);
                }
            }
            let status = lock(&inner.child).take().and_then(|mut c| c.wait().ok());
            let stderr = errors.and_then(|t| t.join().ok()).unwrap_or_default();
            if inner.ended.load(Ordering::SeqCst) {
                return;
            }
            let crashed = !status.is_some_and(|s| s.success());
            match inner.backend {
                // Claude Code exiting ends the session.
                Backend::ClaudeCode => {
                    let error = crashed.then(|| last_lines(&stderr));
                    if let Some(e) = &error {
                        inner.forward(failure(e.clone()));
                    }
                    inner.end(error.as_deref());
                }
                // Codex exits after every turn; only a turn that never finished is a failure.
                Backend::Codex if crashed || !turn_done => {
                    let error = if crashed { last_lines(&stderr) } else { "Codex exited before finishing its answer".into() };
                    inner.forward(failure(error.clone()));
                    inner.end(Some(&error));
                }
                Backend::Codex => inner.forward(Event::TurnFinished),
                Backend::OpenCode | Backend::Antigravity => unreachable!("ACP has its own transport"),
            }
        });
        if let Err(e) = spawned {
            self.forward(Event::Failed(format!("couldn't read the CLI's output: {e}")));
        }
    }

    fn forward(&self, event: Event) {
        if self.record_event(&event) {
            (self.sink)(event);
        }
    }

    fn record_event(&self, event: &Event) -> bool {
        let mut recording = lock(&self.recording);
        if self.ended.load(Ordering::SeqCst) {
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

    fn kill(&self) {
        *lock(&self.stdin) = None;
        if let Some(child) = lock(&self.child).as_mut() {
            if matches!(self.backend, Backend::OpenCode | Backend::Antigravity)
                && let Ok(group) = libc::pid_t::try_from(child.id())
                && group > 0
            {
                // SAFETY: ACP children lead their own process groups. The unreaped child
                // is still owned by this mutex, so its group ID can't be reused here.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = child.kill();
        }
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

fn spawn(mut cmd: Command) -> Result<Child> {
    Ok(cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?)
}

/// Lock poisoning means a stream thread panicked; the session can't continue sensibly.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().expect("gardener session lock poisoned")
}

fn last_lines(stderr: &str) -> String {
    let tail: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
    let text = tail.into_iter().rev().collect::<Vec<_>>().join(" ");
    if text.is_empty() { "the CLI stopped unexpectedly".into() } else { clip(&text, STDERR_CHARS) }
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
    }
}
