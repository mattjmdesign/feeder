use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use feeder_core::gardener::{AccountStatus, Backend, ConnectionStatus, SessionEnv};
use rune::{El, Waker, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::State;
use crate::style::{self, Palette};

#[derive(Default)]
pub struct SignIn {
    active: Option<(Backend, Receiver<Status>, Arc<AtomicBool>)>,
    status: Option<(Backend, Status)>,
    scanned: HashMap<Backend, AccountStatus>,
    scan_rx: Option<Receiver<Vec<(Backend, AccountStatus)>>>,
    scan_fresh: bool,
}

enum Status {
    Browser,
    Terminal,
    Checking,
    Connected(Option<String>),
    Cancelled,
    Failed(String),
}

#[derive(Clone, Copy)]
enum Action {
    Browser,
    Terminal,
    Check,
}

fn browser_supported(backend: Backend) -> bool {
    matches!(backend, Backend::Codex | Backend::ClaudeCode | Backend::Antigravity)
}

fn primary_label(backend: Backend) -> &'static str {
    match backend {
        Backend::Codex => "Sign In with ChatGPT",
        Backend::Antigravity => "Sign In with Google",
        Backend::ClaudeCode => "Continue in Browser",
        Backend::OpenCode => "Connect in Terminal",
    }
}

impl SignIn {
    pub fn begin_run(&mut self) {
        if self.active.is_none() {
            self.status = None;
        }
    }

    pub fn follow_accounts(&mut self, open: bool, wake: Option<&Waker>) {
        if !open {
            self.scan_fresh = false;
            return;
        }
        if self.scan_fresh || self.scan_rx.is_some() {
            return;
        }
        self.scan_fresh = true;
        let (tx, rx) = mpsc::channel();
        let wake = wake.cloned();
        if std::thread::Builder::new()
            .name("feeder-curator-accounts".into())
            .spawn(move || {
                let data_dir = feeder_core::data_dir();
                let installed = Backend::installed();
                let results = std::thread::scope(|scope| {
                    let jobs: Vec<_> = installed
                        .into_iter()
                        .map(|backend| {
                            let data_dir = &data_dir;
                            scope.spawn(move || (backend, backend.account_status(data_dir)))
                        })
                        .collect();
                    jobs.into_iter().filter_map(|job| job.join().ok()).collect::<Vec<_>>()
                });
                let _ = tx.send(results);
                if let Some(wake) = wake {
                    wake();
                }
            })
            .is_ok()
        {
            self.scan_rx = Some(rx);
        }
    }

    fn start(&mut self, backend: Backend, action: Action, wake: Option<Waker>) {
        if self.active.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        self.status = Some((
            backend,
            match action {
                Action::Browser => Status::Browser,
                Action::Terminal | Action::Check => Status::Checking,
            },
        ));
        self.active = Some((backend, rx, cancel));
        if std::thread::Builder::new()
            .name("feeder-curator-sign-in".into())
            .spawn(move || {
                let data_dir = feeder_core::data_dir();
                let result = match action {
                    Action::Browser => match backend.connect_in_browser(&data_dir, &stop, open_browser) {
                        _ if stop.load(Ordering::Acquire) => Status::Cancelled,
                        Ok(()) => Status::Connected(backend.account_status(&data_dir).label),
                        Err(error) => Status::Failed(error.to_string()),
                    },
                    Action::Terminal => match open_terminal(backend) {
                        Ok(()) => Status::Terminal,
                        Err(error) => Status::Failed(format!("Couldn’t open Terminal: {error}")),
                    },
                    Action::Check => {
                        let account = backend.account_status(&data_dir);
                        match account.status {
                            ConnectionStatus::Connected => Status::Connected(account.label),
                            ConnectionStatus::Unknown => {
                                Status::Failed("This provider didn’t confirm sign-in. You can try Curator again.".into())
                            }
                            ConnectionStatus::NotInstalled => Status::Failed(format!("{} isn’t installed.", backend.label())),
                            ConnectionStatus::SignedOut => Status::Failed("This provider is still signed out.".into()),
                        }
                    }
                };
                let _ = tx.send(result);
                if let Some(wake) = wake {
                    wake();
                }
            })
            .is_err()
        {
            self.active = None;
            self.status = Some((backend, Status::Failed("Couldn’t start sign-in.".into())));
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.scan_rx {
            match rx.try_recv() {
                Ok(results) => {
                    for (backend, account) in results {
                        self.scanned.insert(backend, account);
                    }
                    self.scan_rx = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.scan_rx = None;
                    changed = true;
                }
            }
        }
        if let Some((backend, rx, _)) = &self.active {
            let status = match rx.try_recv() {
                Ok(status) => Some(status),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Status::Failed("Sign-in stopped unexpectedly.".into())),
            };
            if let Some(status) = status {
                let backend = *backend;
                if let Status::Connected(label) = &status {
                    self.scanned.insert(backend, AccountStatus { status: ConnectionStatus::Connected, label: label.clone() });
                }
                self.status = Some((backend, status));
                self.active = None;
                changed = true;
            }
        }
        changed
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        if let Some((_, _, cancel)) = &self.active {
            cancel.store(true, Ordering::Release);
        }
    }
}

fn open_browser(url: &str) -> io::Result<()> {
    let status = Command::new("/usr/bin/open").arg(url).status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other("Couldn’t open the browser.")) }
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn open_terminal(backend: Backend) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let env = SessionEnv::detect();
    if !env.feeder.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "Feeder’s command-line helper is missing."));
    }
    let path = std::env::temp_dir().join(format!("feeder-sign-in-{}-{}.command", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o700).open(&path)?;
    let result = (|| {
        writeln!(
            file,
            "#!/bin/sh\n/bin/rm -f -- \"$0\"\nexport FEEDER_HOME={}\nexec {} curator connect --with {}",
            shell_word(&env.data_dir.to_string_lossy()),
            shell_word(&env.feeder.to_string_lossy()),
            backend.as_str(),
        )?;
        drop(file);
        let status = Command::new("/usr/bin/open").args(["-a", "Terminal"]).arg(&path).status()?;
        if status.success() { Ok(()) } else { Err(io::Error::other("Terminal couldn’t start the sign-in command.")) }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

impl SignIn {
    pub fn demo(&mut self) {
        self.scan_fresh = true;
        self.scanned = HashMap::from([
            (Backend::ClaudeCode, AccountStatus { status: ConnectionStatus::Connected, label: Some("alex@example.com · Team".into()) }),
            (Backend::Codex, AccountStatus { status: ConnectionStatus::Connected, label: Some("alex@example.com · Plus".into()) }),
            (Backend::OpenCode, AccountStatus { status: ConnectionStatus::Connected, label: Some("OpenAI".into()) }),
            (Backend::Antigravity, AccountStatus::from_status(ConnectionStatus::SignedOut)),
        ]);
    }

    pub fn usable(&self, backend: Backend) -> bool {
        if matches!(self.status.as_ref(), Some((b, Status::Connected(_))) if *b == backend) {
            return true;
        }
        matches!(self.scanned.get(&backend).map(|a| a.status), Some(ConnectionStatus::Connected | ConnectionStatus::Unknown))
    }
}

pub fn cells(s: &State, backend: Backend, p: &Palette, k: &Kit) -> (El, El) {
    let sign_in = &s.curator_sign_in;
    let transient = sign_in.status.as_ref().filter(|(b, _)| *b == backend).map(|(_, st)| st);
    let scanned = sign_in.scanned.get(&backend);
    let busy_other = sign_in.active.as_ref().is_some_and(|(b, _, _)| *b != backend);
    let browser = browser_supported(backend);
    let default_action = if browser { Action::Browser } else { Action::Terminal };

    let start = |action| move |s: &mut State| s.curator_sign_in.start(backend, action, s.wake.clone());
    let btn = |label: &str, variant: ButtonVariant, action: Action, suffix: &str| {
        Button::new(label)
            .variant(variant)
            .control_size(ControlSize::Small)
            .disabled(sign_in.active.is_some())
            .on_press(start(action))
            .build(k)
            .key(format!("curator-acct-{}-{suffix}", backend.as_str()))
    };
    let cancel_btn = || {
        Button::new("Cancel")
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press(|s: &mut State| {
                if let Some((_, _, cancel)) = &s.curator_sign_in.active {
                    cancel.store(true, Ordering::Release);
                }
            })
            .build(k)
            .key(format!("curator-acct-{}-cancel", backend.as_str()))
    };
    let line = |t: &str, c| text(t.to_string()).font(style::META).color(c).truncate();
    let signed_in = |account_label: Option<&str>| match account_label {
        Some(detail) => line(&format!("{} · {detail}", backend.label()), p.text_2),
        None => line(&format!("{} · signed in", backend.label()), p.text_2),
    };

    match transient {
        Some(Status::Browser) => (line("Finish signing in in your browser…", p.text_2), cancel_btn()),
        Some(Status::Checking) => (line("Checking…", p.text_3), el()),
        Some(Status::Terminal) => (
            line("Finish in Terminal, then check", p.text_2),
            row().gap(6.0).child(btn("Check Sign-In", ButtonVariant::Outline, Action::Check, "check")).child(btn(
                "Reopen Terminal",
                ButtonVariant::GhostMuted,
                Action::Terminal,
                "term",
            )),
        ),
        Some(Status::Failed(err)) => (
            text(err.clone()).font(style::META).color(k.c.danger.fg).truncate().help(err.clone()),
            btn("Try Again", ButtonVariant::Outline, default_action, "retry"),
        ),
        Some(Status::Connected(label)) => (signed_in(label.as_deref()), el()),
        Some(Status::Cancelled) | None => match scanned.map(|a| a.status) {
            Some(ConnectionStatus::Connected) => (signed_in(scanned.and_then(|a| a.label.as_deref())), el()),
            Some(ConnectionStatus::Unknown) if backend == Backend::OpenCode => {
                (line("OpenCode · no account listed", p.text_3), btn("Sign In…", ButtonVariant::Outline, default_action, "connect"))
            }
            Some(ConnectionStatus::SignedOut | ConnectionStatus::Unknown) => (
                line(&format!("{} · Sign in to use", backend.label()), p.text_3),
                btn("Sign In…", ButtonVariant::Outline, default_action, "connect"),
            ),
            Some(ConnectionStatus::NotInstalled) => (line(&format!("{} · not installed", backend.label()), p.text_3), el()),
            None if sign_in.scan_rx.is_some() || busy_other => (line("Checking…", p.text_3), el()),
            None => (line(backend.label(), p.text_3), btn("Sign In…", ButtonVariant::Outline, default_action, "connect")),
        },
    }
}

pub fn controls(s: &State, backend: Backend, p: &Palette, k: &Kit, retry: Option<fn(&mut State)>) -> El {
    let sign_in = &s.curator_sign_in;
    let status = sign_in.status.as_ref().filter(|(b, _)| *b == backend).map(|(_, status)| status);
    let browser = browser_supported(backend);
    let start = |action| move |s: &mut State| s.curator_sign_in.start(backend, action, s.wake.clone());
    let primary = primary_label(backend);
    let button = |label, action| {
        let key = match action {
            Action::Browser => "curator-signin-browser",
            Action::Terminal => "curator-signin-terminal",
            Action::Check => "curator-signin-check",
        };
        Button::new(label).control_size(ControlSize::Small).disabled(sign_in.active.is_some()).on_press(start(action)).build(k).key(key)
    };
    let mut actions = row().gap(6.0).flex_wrap();
    let other_active = sign_in.active.as_ref().map(|(b, _, _)| *b).filter(|b| *b != backend);
    let cancel_button = || {
        Button::new("Cancel")
            .control_size(ControlSize::Small)
            .on_press(|s: &mut State| {
                if let Some((_, _, cancel)) = &s.curator_sign_in.active {
                    cancel.store(true, Ordering::Release);
                }
            })
            .build(k)
    };
    let message: Option<Cow<'_, str>> = if let Some(active_backend) = other_active {
        actions = actions.child(cancel_button());
        Some(format!("Finish or cancel {} sign-in first.", active_backend.label()).into())
    } else {
        match status {
            Some(Status::Browser) => {
                actions = actions.child(cancel_button());
                Some("Finish signing in with your provider. This page will update when you’re connected.".into())
            }
            Some(Status::Checking) => Some("Checking the provider…".into()),
            Some(Status::Connected(_)) => Some("Connected for Curator.".into()),
            Some(Status::Terminal) => {
                actions = actions.child(button("Check Sign-In", Action::Check)).child(button("Reopen Terminal", Action::Terminal));
                Some("Follow the provider prompts in Terminal, then check sign-in here.".into())
            }
            Some(Status::Failed(message)) => {
                actions = actions.child(button(primary, if browser { Action::Browser } else { Action::Terminal }));
                if browser {
                    actions = actions.child(button("Open in Terminal", Action::Terminal));
                }
                Some(message.as_str().into())
            }
            Some(Status::Cancelled) | None => {
                actions = actions.child(button(primary, if browser { Action::Browser } else { Action::Terminal }));
                match backend {
                    Backend::OpenCode => {
                        Some("Choose a provider in OpenCode’s picker, such as OpenAI for ChatGPT or OpenCode Go for its API key.".into())
                    }
                    _ => None,
                }
            }
        }
    };
    if let Some(retry) = retry
        && other_active.is_none()
        && !matches!(status, Some(Status::Browser | Status::Checking))
    {
        actions = actions
            .child(Button::new("Try Again").variant(ButtonVariant::Outline).control_size(ControlSize::Small).on_press(retry).build(k));
    }
    let mut out = col().gap(8.0);
    if let Some(message) = message {
        out = out.child(text(message.into_owned()).font(style::META).color(p.text_2).wrap().live_region(rune::LiveRegion::Polite));
    }
    out.child(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_word_quotes_spaces_and_single_quotes() {
        assert_eq!(shell_word("/tmp/a b/c'd"), "'/tmp/a b/c'\\''d'");
    }

    #[test]
    fn poll_applies_finished_status_and_begin_run_clears_idle_state() {
        let mut sign_in = SignIn::default();
        let (tx, rx) = mpsc::channel();
        sign_in.active = Some((Backend::Codex, rx, Arc::new(AtomicBool::new(false))));
        assert!(!sign_in.poll());
        tx.send(Status::Connected(Some("alex@example.com · Plus".into()))).unwrap();
        assert!(sign_in.poll());
        assert!(sign_in.active.is_none());
        assert!(matches!(sign_in.status, Some((Backend::Codex, Status::Connected(_)))));
        assert_eq!(
            sign_in.scanned.get(&Backend::Codex),
            Some(&AccountStatus { status: ConnectionStatus::Connected, label: Some("alex@example.com · Plus".into()) })
        );

        sign_in.begin_run();
        assert!(sign_in.status.is_none());
    }

    #[test]
    fn browser_flow_covers_claude_codex_and_antigravity() {
        assert!(browser_supported(Backend::ClaudeCode));
        assert!(browser_supported(Backend::Codex));
        assert!(browser_supported(Backend::Antigravity));
        assert!(!browser_supported(Backend::OpenCode));
    }
}
