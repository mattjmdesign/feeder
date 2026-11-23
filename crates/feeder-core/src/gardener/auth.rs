use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, profile, session};

const CODEX_ACCOUNT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    NotInstalled,
    SignedOut,
    Connected,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountStatus {
    pub status: ConnectionStatus,
    pub label: Option<String>,
}

impl AccountStatus {
    pub fn from_status(status: ConnectionStatus) -> Self {
        Self { status, label: None }
    }
}

impl Backend {
    pub fn account_status(self, data_dir: &Path) -> AccountStatus {
        let Some(bin) = self.binary() else { return AccountStatus::from_status(ConnectionStatus::NotInstalled) };
        let Ok(mut cmd) = profile::command(self, &bin, data_dir) else { return AccountStatus::from_status(ConnectionStatus::Unknown) };
        let args: &[&str] = match self {
            Self::ClaudeCode => &["auth", "status", "--json"],
            Self::Codex => &["login", "status"],
            Self::OpenCode => &["auth", "list", "--format", "json", "--standalone"],
            Self::Antigravity => &["models"],
        };
        let Ok((status, stdout, stderr)) = read_output(cmd.args(args), Duration::from_secs(5)) else {
            return AccountStatus::from_status(ConnectionStatus::Unknown);
        };
        let conn = classify(self, status.code(), &stdout, &stderr);
        let label = if conn == ConnectionStatus::Connected {
            match self {
                Self::ClaudeCode => claude_account_label(&stdout),
                Self::OpenCode => opencode_account_label(&stdout),
                Self::Codex => codex_account_label(&bin, data_dir),
                Self::Antigravity => None,
            }
        } else {
            None
        };
        AccountStatus { status: conn, label }
    }
}

fn clean_piece(raw: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { None } else { Some(super::clip(trimmed, max_chars)) }
}

fn title_word(raw: &str) -> Option<String> {
    let piece = clean_piece(raw, 24)?;
    let mut chars = piece.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

fn join_email_and_plan(email: Option<String>, plan: Option<String>) -> Option<String> {
    match (email, plan) {
        (Some(email), Some(plan)) => Some(format!("{email} · {plan}")),
        (Some(email), None) => Some(email),
        (None, Some(plan)) => Some(plan),
        (None, None) => None,
    }
}

fn claude_account_label(stdout: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(stdout).ok()?;
    let email = value["email"].as_str().and_then(|s| clean_piece(s, 64));
    let plan = value["subscriptionType"].as_str().and_then(title_word);
    join_email_and_plan(email, plan)
}

fn opencode_account_label(stdout: &[u8]) -> Option<String> {
    let accounts: Vec<Value> = serde_json::from_slice(stdout).ok()?;
    let mut names = Vec::new();
    for account in accounts {
        let name = account["name"].as_str().or_else(|| account["id"].as_str()).and_then(|s| clean_piece(s, 32));
        if let Some(name) = name
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    if names.is_empty() {
        None
    } else if names.len() <= 2 {
        Some(names.join(", "))
    } else {
        Some(format!("{}, {} +{}", names[0], names[1], names.len() - 2))
    }
}

fn parse_codex_account(result: &Value) -> Option<String> {
    let account = result.get("account").unwrap_or(result);
    let email = account["email"].as_str().and_then(|s| clean_piece(s, 64));
    let plan = account["planType"].as_str().and_then(title_word);
    join_email_and_plan(email, plan)
}

fn codex_account_label(bin: &Path, data_dir: &Path) -> Option<String> {
    let mut cmd = profile::command(Backend::Codex, bin, data_dir).ok()?;
    cmd.args(["app-server", "--listen", "stdio://"]);
    let mut child = cmd.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut input = child.stdin.take()?;
    let output = child.stdout.take()?;
    let (tx, rx) = mpsc::sync_channel(16);
    if std::thread::Builder::new()
        .name("feeder-codex-account".into())
        .spawn(move || {
            let mut reader = BufReader::new(output);
            let mut line = Vec::new();
            loop {
                line.clear();
                const MAX_LINE: u64 = 128 * 1024;
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
        })
        .is_err()
    {
        drop(input);
        let _ = session::terminate_child(&mut child);
        return None;
    }

    let label = (|| -> Option<String> {
        let deadline = Instant::now() + CODEX_ACCOUNT_TIMEOUT;
        writeln!(
            input,
            "{}",
            json!({
                "id": 1,
                "method": "initialize",
                "params": { "clientInfo": { "name": "feeder", "title": "Feeder", "version": env!("CARGO_PKG_VERSION") } }
            })
        )
        .ok()?;
        input.flush().ok()?;

        let mut initialized = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let message = match rx.recv_timeout(remaining.min(Duration::from_millis(80))) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return None,
            };
            match message["id"].as_u64() {
                Some(1) if !initialized && message.get("result").is_some() => {
                    initialized = true;
                    writeln!(input, "{}", json!({ "method": "initialized" })).ok()?;
                    writeln!(input, "{}", json!({ "id": 2, "method": "account/read", "params": {} })).ok()?;
                    input.flush().ok()?;
                }
                Some(2) => return message.get("result").and_then(parse_codex_account),
                Some(1) => return None,
                _ => {}
            }
        }
    })();

    drop(input);
    let _ = session::terminate_child(&mut child);
    label
}

fn classify(backend: Backend, code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> ConnectionStatus {
    use ConnectionStatus::{Connected, SignedOut, Unknown};

    let has_line =
        |message: &str| [stdout, stderr].into_iter().any(|bytes| String::from_utf8_lossy(bytes).lines().any(|line| line.trim() == message));
    match (backend, code) {
        (Backend::ClaudeCode, Some(0 | 1)) => {
            match (code, serde_json::from_slice::<Value>(stdout).ok().and_then(|value| value["loggedIn"].as_bool())) {
                (Some(0), Some(true)) => Connected,
                (Some(1), Some(false)) => SignedOut,
                _ => Unknown,
            }
        }
        (Backend::Codex, Some(0))
            if [stdout, stderr]
                .into_iter()
                .any(|bytes| String::from_utf8_lossy(bytes).lines().any(|line| line.trim_start().starts_with("Logged in using "))) =>
        {
            Connected
        }
        (Backend::Codex, Some(1)) if has_line("Not logged in") => SignedOut,
        (Backend::OpenCode, Some(0)) => match serde_json::from_slice::<Vec<Value>>(stdout) {
            Ok(accounts) if accounts.iter().any(Value::is_object) => Connected,
            _ => Unknown,
        },
        (Backend::Antigravity, Some(0))
            if String::from_utf8_lossy(stdout).lines().any(|line| {
                let line = line.trim();
                !line.is_empty() && line != "Fetching available models..."
            }) =>
        {
            Connected
        }
        (Backend::Antigravity, Some(1))
            if has_line("Error: Please sign in to view available models. Launch the CLI without arguments to sign in.") =>
        {
            SignedOut
        }
        _ => Unknown,
    }
}

fn read_output(cmd: &mut Command, timeout: Duration) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = cmd.process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    std::thread::scope(|scope| {
        let stdout = scope.spawn(move || read_bounded(stdout));
        let stderr = scope.spawn(move || read_bounded(stderr));
        let start = Instant::now();
        let exited = loop {
            match session::child_exited(child.id()) {
                Ok(true) => break Ok(()),
                Ok(false) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                Ok(false) => break Err(io::Error::new(io::ErrorKind::TimedOut, "provider status check timed out")),
                Err(error) => break Err(error),
            }
        };
        let status = session::terminate_child(&mut child);
        exited?;
        let stdout = stdout.join().map_err(|_| io::Error::other("couldn't read provider status"))??;
        let stderr = stderr.join().map_err(|_| io::Error::other("couldn't read provider status"))??;
        Ok((status?, stdout, stderr))
    })
}

fn read_bounded(pipe: Option<impl Read>) -> io::Result<Vec<u8>> {
    const OUTPUT_LIMIT: usize = 64 * 1024;
    let mut output = Vec::new();
    pipe.ok_or_else(|| io::Error::other("missing provider status output"))?.take((OUTPUT_LIMIT + 1) as u64).read_to_end(&mut output)?;
    if output.len() > OUTPUT_LIMIT {
        return Err(io::Error::other("provider status output exceeded its limit"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_status_does_not_turn_errors_into_sign_outs() {
        use Backend::{Antigravity, ClaudeCode, Codex, OpenCode};
        use ConnectionStatus::{Connected, SignedOut, Unknown};
        assert_eq!(classify(ClaudeCode, Some(0), br#"{"loggedIn":true}"#, b""), Connected);
        assert_eq!(classify(ClaudeCode, Some(1), br#"{"loggedIn":false}"#, b""), SignedOut);
        assert_eq!(classify(ClaudeCode, Some(1), b"Unsupported command", b""), Unknown);
        assert_eq!(classify(Codex, Some(0), b"", b"Logged in using ChatGPT\n"), Connected);
        assert_eq!(classify(Codex, Some(1), b"Not logged in\n", b""), SignedOut);
        assert_eq!(classify(Codex, Some(1), b"", b"couldn't open credentials"), Unknown);
        assert_eq!(classify(OpenCode, Some(0), br#"[{"provider":"openai"}]"#, b""), Connected);
        assert_eq!(classify(OpenCode, Some(0), b"[]", b""), Unknown);
        assert_eq!(classify(Antigravity, Some(0), b"Fetching available models...\nGemini\n", b""), Connected);
        assert_eq!(
            classify(
                Antigravity,
                Some(1),
                b"Fetching available models...",
                b"Error: Please sign in to view available models. Launch the CLI without arguments to sign in."
            ),
            SignedOut
        );
        assert_eq!(classify(Antigravity, Some(1), b"", b"The model service is unavailable"), Unknown);
    }

    #[test]
    fn account_labels_format_cleanly_and_strip_control_chars() {
        assert_eq!(
            claude_account_label(br#"{"loggedIn":true,"email":"alex@example.com\n","subscriptionType":"team"}"#).as_deref(),
            Some("alex@example.com · Team")
        );
        assert_eq!(
            parse_codex_account(&json!({ "account": { "email": "alex@example.com", "planType": "plus" } })).as_deref(),
            Some("alex@example.com · Plus")
        );
        assert_eq!(
            opencode_account_label(br#"[{"id":"openai","name":"OpenAI"},{"id":"opencode","name":"OpenCode Console"}]"#).as_deref(),
            Some("OpenAI, OpenCode Console")
        );
        assert_eq!(opencode_account_label(br#"[{"name":"A"},{"name":"B"},{"name":"C"},{"name":"D"}]"#).as_deref(), Some("A, B +2"));
    }

    #[test]
    fn status_check_times_out_even_if_a_child_holds_the_output_open() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        let error = read_output(&mut cmd, Duration::from_millis(80)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
