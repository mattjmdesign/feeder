use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, session};
use crate::{Error, Result};

pub(super) fn command(backend: Backend, binary: &Path, data: &Path) -> Result<Command> {
    let profile = profile_dir(backend, data)?;
    let mut cmd = Command::new(binary);
    cmd.env_clear().current_dir(&profile);

    cmd.env("HOME", dirs::home_dir().unwrap_or_else(|| profile.clone()));

    for var in ["PATH", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    Ok(cmd)
}

pub(super) fn gemini_dir(data: &Path) -> Result<PathBuf> {
    let dir = profile_dir(Backend::Antigravity, data)?.join(".gemini");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    Ok(dir)
}

pub(super) fn add_antigravity_gemini_dir(cmd: &mut Command, data: &Path) -> Result<()> {
    cmd.arg("--gemini-dir").arg(gemini_dir(data)?);
    Ok(())
}

pub(super) fn profile_dir(backend: Backend, data: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(data)?.join("curator-profiles").join(backend.as_str());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&path)?;
    Ok(path)
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = session::terminate_child(&mut self.0);
    }
}

pub(super) fn codex_rpc(bin: &Path, data_dir: &Path, method: &str, timeout: Duration, max_line: u64, thread_name: &str) -> Result<Value> {
    let mut cmd = command(Backend::Codex, bin, data_dir)?;
    cmd.args(["app-server", "--listen", "stdio://"]);
    let mut guard = ChildGuard(cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?);
    let mut input = guard.0.stdin.take().ok_or_else(|| Error::Invalid("Missing Codex input".into()))?;
    let output = guard.0.stdout.take().ok_or_else(|| Error::Invalid("Missing Codex output".into()))?;
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::Builder::new().name(thread_name.into()).spawn(move || {
        let mut reader = BufReader::new(output);
        let mut line = Vec::new();
        loop {
            line.clear();
            let Ok(bytes) = (&mut reader).take(max_line).read_until(b'\n', &mut line) else { break };
            if bytes == 0 || (!line.ends_with(b"\n") && bytes as u64 == max_line) {
                break;
            }
            if let Ok(message) = serde_json::from_slice::<Value>(&line)
                && tx.send(message).is_err()
            {
                break;
            }
        }
    })?;

    let deadline = Instant::now() + timeout;
    writeln!(
        input,
        "{}",
        json!({
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "feeder", "title": "Feeder", "version": env!("CARGO_PKG_VERSION") } }
        })
    )?;
    input.flush()?;

    let mut initialized = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::TimedOut, "Codex request timed out")));
        }
        let message = match rx.recv_timeout(remaining.min(Duration::from_millis(80))) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Invalid("Codex closed the connection".into())),
        };
        match message["id"].as_u64() {
            Some(1) if !initialized && message.get("result").is_some() => {
                initialized = true;
                writeln!(input, "{}", json!({ "method": "initialized" }))?;
                writeln!(input, "{}", json!({ "id": 2, "method": method, "params": {} }))?;
                input.flush()?;
            }
            Some(2) if message.get("result").is_some() => return Ok(message["result"].clone()),
            Some(1 | 2) => return Err(Error::Invalid("Codex rejected the request".into())),
            _ => {}
        }
    }
}
