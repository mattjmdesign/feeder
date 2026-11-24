use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, profile};
use crate::{Error, Result};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const CHECK_INTERVAL: Duration = Duration::from_millis(100);

impl Backend {
    pub fn connect_in_browser(self, data_dir: &Path, cancel: &AtomicBool, open: impl FnOnce(&str) -> io::Result<()>) -> Result<()> {
        let bin = self.binary().ok_or_else(|| Error::NotFound(format!("{} isn't installed", self.label())))?;
        let mut cmd = profile::command(self, &bin, data_dir)?;
        let deadline = Instant::now() + LOGIN_TIMEOUT;

        match self {
            Self::Codex => {
                cmd.args(["app-server", "--listen", "stdio://"]);
                codex_login(cmd, cancel, deadline, open)
            }
            Self::ClaudeCode => {
                cmd.args(["auth", "login", "--claudeai"]);
                let mut child =
                    LoginChild::new(cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?);
                let _stdin = child.child.stdin.take();
                let stderr = child.child.stderr.take().map(|mut pipe| {
                    std::thread::spawn(move || {
                        let mut buf = Vec::new();
                        let _ = pipe.by_ref().take(8 * 1024).read_to_end(&mut buf);
                        let _ = io::copy(&mut pipe, &mut io::sink());
                        String::from_utf8_lossy(&buf).trim().to_string()
                    })
                });
                loop {
                    check_login(cancel, deadline)?;
                    if crate::process::has_exited(child.child.id())? {
                        let status = child.finish()?;
                        let detail = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
                        return if status.success() {
                            Ok(())
                        } else if !detail.is_empty() {
                            let line = detail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(&detail);
                            Err(Error::Invalid(format!("Claude Code sign-in failed: {}", super::clip(line.trim(), 200))))
                        } else {
                            Err(Error::Invalid("Claude Code couldn’t finish browser sign-in.".into()))
                        };
                    }
                    std::thread::sleep(CHECK_INTERVAL);
                }
            }
            Self::Antigravity => {
                cmd.args(["acp", "--stdio", "--include-default-customizations=false", "--gemini-dir"]).arg(profile::gemini_dir(data_dir)?);
                antigravity_login(cmd, cancel, deadline)
            }
            Self::OpenCode => Err(Error::Invalid("OpenCode uses an interactive sign-in in Terminal.".into())),
        }
    }
}

struct LoginChild {
    child: Child,
    reaped: bool,
}

impl LoginChild {
    fn new(child: Child) -> Self {
        Self { child, reaped: false }
    }

    fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
        self.reaped = true;
        crate::process::terminate_group(&mut self.child)
    }
}

impl Drop for LoginChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
        }
    }
}

fn check_login(cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Sign-in cancelled."));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "Sign-in timed out."));
    }
    Ok(())
}

fn codex_login(mut cmd: Command, cancel: &AtomicBool, deadline: Instant, open: impl FnOnce(&str) -> io::Result<()>) -> Result<()> {
    let mut child = LoginChild::new(cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?);
    let mut input = child.child.stdin.take().ok_or_else(|| io::Error::other("Missing Codex input"))?;
    let output = child.child.stdout.take().ok_or_else(|| io::Error::other("Missing Codex output"))?;
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::Builder::new().name("feeder-codex-login".into()).spawn(move || {
        let mut reader = BufReader::new(output);
        let mut line = Vec::new();
        loop {
            line.clear();
            const MAX_LINE: u64 = 256 * 1024;
            let Ok(bytes) = (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line) else { break };
            if bytes == 0 || (!line.ends_with(b"\n") && bytes as u64 == MAX_LINE) {
                break;
            }
            if let Ok(message) = serde_json::from_slice::<Value>(&line)
                && tx.send(message).is_err()
            {
                break;
            }
        }
    })?;

    let send = |input: &mut std::process::ChildStdin, request: Value| writeln!(input, "{request}");
    send(
        &mut input,
        json!({
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "feeder", "title": "Feeder", "version": env!("CARGO_PKG_VERSION") } }
        }),
    )?;

    let mut open = Some(open);
    let mut login_id = None;
    loop {
        check_login(cancel, deadline)?;
        let message = match rx.recv_timeout(CHECK_INTERVAL) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Invalid("Codex closed before sign-in finished.".into())),
        };
        match message["id"].as_u64() {
            Some(1) if message.get("result").is_some() => {
                send(&mut input, json!({ "method": "initialized" }))?;
                send(&mut input, json!({ "id": 2, "method": "account/login/start", "params": { "type": "chatgpt" } }))?;
            }
            Some(2) if message["result"]["type"] == "chatgpt" => {
                let result = &message["result"];
                let url = result["authUrl"]
                    .as_str()
                    .filter(|url| url.starts_with("https://auth.openai.com/") && !url.chars().any(char::is_control))
                    .ok_or_else(|| Error::Invalid("Codex didn’t return an OpenAI sign-in page.".into()))?;
                login_id = result["loginId"].as_str().map(str::to_owned);
                let Some(launch) = open.take().filter(|_| login_id.is_some()) else {
                    return Err(Error::Invalid("Codex didn’t return a sign-in session.".into()));
                };
                launch(url)?;
            }
            Some(1 | 2) => return Err(Error::Invalid("Codex couldn’t start browser sign-in.".into())),
            _ if message["method"] == "account/login/completed"
                && login_id.as_deref().is_some_and(|id| message["params"]["loginId"] == id) =>
            {
                return if message["params"]["success"] == true {
                    Ok(())
                } else {
                    Err(Error::Invalid("Codex couldn’t finish browser sign-in.".into()))
                };
            }
            _ => {}
        }
    }
}

fn antigravity_login(mut cmd: Command, cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    let mut child = LoginChild::new(cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?);
    let mut input = child.child.stdin.take().ok_or_else(|| io::Error::other("Missing Antigravity input"))?;
    let output = child.child.stdout.take().ok_or_else(|| io::Error::other("Missing Antigravity output"))?;
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::Builder::new().name("feeder-agy-login".into()).spawn(move || {
        let mut reader = BufReader::new(output);
        let mut line = Vec::new();
        loop {
            line.clear();
            const MAX_LINE: u64 = 256 * 1024;
            let Ok(bytes) = (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line) else { break };
            if bytes == 0 || (!line.ends_with(b"\n") && bytes as u64 == MAX_LINE) {
                break;
            }
            if let Ok(message) = serde_json::from_slice::<Value>(&line)
                && tx.send(message).is_err()
            {
                break;
            }
        }
    })?;

    let send = |input: &mut std::process::ChildStdin, request: Value| writeln!(input, "{request}");
    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": { "name": "feeder", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
    )?;

    loop {
        check_login(cancel, deadline)?;
        let message = match rx.recv_timeout(CHECK_INTERVAL) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Invalid("Antigravity closed before sign-in finished.".into())),
        };
        match message["id"].as_u64() {
            Some(1) if message.get("result").is_some() => {
                let has_google =
                    message["result"]["authMethods"].as_array().is_some_and(|methods| methods.iter().any(|m| m["id"] == "oauth-personal"));
                if !has_google {
                    return Err(Error::Invalid("Antigravity didn’t offer Google browser sign-in.".into()));
                }
                send(
                    &mut input,
                    json!({ "jsonrpc": "2.0", "id": 2, "method": "authenticate", "params": { "methodId": "oauth-personal" } }),
                )?;
            }
            Some(2) if message.get("error").is_none() && message.get("result").is_some() => return Ok(()),
            Some(1 | 2) => {
                let detail = message["error"]["message"].as_str().map(str::trim).filter(|s| !s.is_empty());
                return Err(Error::Invalid(match detail {
                    Some(msg) => format!("Antigravity sign-in failed: {}", super::clip(msg, 200)),
                    None => "Antigravity couldn’t finish Google sign-in.".into(),
                }));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_official_browser_url_and_waits_for_matching_completion() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            r#"
            read -r init
            echo '{"id":1,"result":{}}'
            read -r initialized
            read -r login
            echo '{"id":2,"result":{"type":"chatgpt","authUrl":"https://auth.openai.com/test","loginId":"this"}}'
            echo '{"method":"account/login/completed","params":{"success":false,"loginId":"other"}}'
            echo '{"method":"account/login/completed","params":{"success":true,"loginId":"this"}}'
        "#,
        ]);
        let mut opened = false;
        codex_login(cmd, &AtomicBool::new(false), Instant::now() + Duration::from_secs(3), |url| {
            assert_eq!(url, "https://auth.openai.com/test");
            opened = true;
            Ok(())
        })
        .unwrap();
        assert!(opened);
    }

    #[test]
    fn antigravity_login_calls_acp_authenticate_with_oauth_personal() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            r#"
            read -r init
            echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"oauth-personal","name":"Log in with Google"}]}}'
            read -r auth
            case "$auth" in
                *'"method":"authenticate"'*'"methodId":"oauth-personal"'*)
                    echo '{"jsonrpc":"2.0","id":2,"result":{}}'
                    ;;
                *)
                    echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"unexpected request"}}'
                    ;;
            esac
        "#,
        ]);
        antigravity_login(cmd, &AtomicBool::new(false), Instant::now() + Duration::from_secs(3)).unwrap();
    }
}
