use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use feeder_core::setup::{self, Outcome, Status, Tool};
use rune::Waker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolAction {
    Connect,
    Disconnect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connected,
    Outdated,
    Missing,
    Connecting,
    Disconnecting,
    Failed { action: ToolAction, reason: String },
}

impl From<Status> for Link {
    fn from(s: Status) -> Link {
        match s {
            Status::Connected => Link::Connected,
            Status::Outdated => Link::Outdated,
            Status::Missing => Link::Missing,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CliStatus {
    #[default]
    Checking,
    Installed {
        path: PathBuf,
        on_path: bool,
    },
    NotInstalled,
    Installing,
    Failed(String),
}

enum Msg {
    Scanned(Vec<(Tool, Link)>, Vec<(String, PathBuf)>, CliStatus),
    Linked(Tool, Link),
    Uninstalled(bool),
    Cli(CliStatus),
}

enum Job {
    Connect { tool: Tool, wake: Option<Waker> },
    Disconnect { tool: Tool, wake: Option<Waker> },
    DisconnectAll { wake: Option<Waker> },
    InstallCli { wake: Option<Waker> },
}

#[derive(Default)]
pub struct Agents {
    pub rows: Option<Vec<(Tool, Link)>>,
    fresh: bool,
    scanning: bool,
    pub uninstalled: bool,
    pub cli: CliStatus,
    pub icons: Vec<(String, PathBuf)>,
    inbox: Arc<Mutex<Vec<Msg>>>,
    worker: Option<Sender<Job>>,
}

impl Agents {
    fn worker(&mut self) -> &Sender<Job> {
        let inbox = self.inbox.clone();
        self.worker.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let _ = std::thread::Builder::new().name("feeder-connect".to_string()).spawn(move || {
                while let Ok(job) = rx.recv() {
                    match job {
                        Job::Connect { tool, wake } => {
                            let bin = setup::feeder_binary();
                            let skipped: Vec<String> = setup::connect(tool, &bin)
                                .into_iter()
                                .filter_map(|(_, outcome)| match outcome {
                                    Outcome::Skipped(why) => Some(why),
                                    _ => None,
                                })
                                .collect();
                            setup::install_skills();
                            let link = match (setup::status(tool, &bin), skipped.into_iter().next()) {
                                (Status::Connected, _) => Link::Connected,
                                (_, Some(why)) => Link::Failed { action: ToolAction::Connect, reason: why },
                                (status, None) => status.into(),
                            };
                            send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
                        }
                        Job::Disconnect { tool, wake } => {
                            let link = unlink(tool, &setup::feeder_binary());
                            send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
                        }
                        Job::DisconnectAll { wake } => {
                            let bin = setup::feeder_binary();
                            let mut all_disconnected = true;
                            for tool in Tool::ALL {
                                if setup::status(tool, &bin) != Status::Missing {
                                    let link = unlink(tool, &bin);
                                    if link != Link::Missing {
                                        all_disconnected = false;
                                    }
                                    send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
                                }
                            }
                            if all_disconnected {
                                setup::remove_skills();
                                match setup::remove_cli_links() {
                                    Ok(()) => {
                                        let status = setup::cli_on_path(&bin)
                                            .map_or(CliStatus::NotInstalled, |(path, on_path)| CliStatus::Installed { path, on_path });
                                        send(&inbox, wake.as_ref(), Msg::Cli(status));
                                    }
                                    Err(error) => {
                                        all_disconnected = false;
                                        send(&inbox, wake.as_ref(), Msg::Cli(CliStatus::Failed(error.to_string())));
                                    }
                                }
                            }
                            send(&inbox, wake.as_ref(), Msg::Uninstalled(all_disconnected));
                        }
                        Job::InstallCli { wake } => {
                            let bin = setup::feeder_binary();
                            let status = match setup::install_cli_link(&bin) {
                                Ok((path, on_path)) => CliStatus::Installed { path, on_path },
                                Err(err) => CliStatus::Failed(err.to_string()),
                            };
                            send(&inbox, wake.as_ref(), Msg::Cli(status));
                        }
                    }
                }
            });
            tx
        })
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn has_connected(&self) -> bool {
        self.rows.as_ref().is_some_and(|rows| rows.iter().any(|(_, link)| matches!(link, Link::Connected | Link::Outdated)))
    }

    pub fn follow_scan(&mut self, active: bool, wake: Option<&Waker>) {
        if !active {
            self.fresh = false;
            return;
        }
        if self.fresh || self.scanning {
            return;
        }
        self.fresh = true;
        self.spawn_scan(wake);
    }

    pub fn rescan(&mut self, wake: Option<&Waker>) {
        if self.scanning {
            return;
        }
        self.fresh = true;
        self.spawn_scan(wake);
    }

    fn spawn_scan(&mut self, wake: Option<&Waker>) {
        self.scanning = true;
        let (inbox, wake) = (self.inbox.clone(), wake.cloned());
        if std::thread::Builder::new()
            .name("feeder-tool-scan".into())
            .spawn(move || {
                let bin = setup::feeder_binary();
                let found: Vec<Tool> = Tool::ALL.into_iter().filter(|t| setup::detected(*t)).collect();
                let icons = found
                    .iter()
                    .filter(|t| !crate::agents::has_logo(t.name()))
                    .filter_map(|t| Some((t.name().to_string(), setup::app_path(*t)?)))
                    .collect();
                let rows = found.into_iter().map(|t| (t, setup::status(t, &bin).into())).collect();
                let cli = match setup::cli_on_path(&bin) {
                    Some((path, on_path)) => CliStatus::Installed { path, on_path },
                    None => CliStatus::NotInstalled,
                };
                send(&inbox, wake.as_ref(), Msg::Scanned(rows, icons, cli));
            })
            .is_err()
        {
            self.scanning = false;
        }
    }

    pub fn connect(&mut self, tool: Tool, wake: Option<&Waker>) {
        self.set(tool, Link::Connecting);
        let _ = self.worker().send(Job::Connect { tool, wake: wake.cloned() });
    }

    pub fn disconnect(&mut self, tool: Tool, wake: Option<&Waker>) {
        self.set(tool, Link::Disconnecting);
        let _ = self.worker().send(Job::Disconnect { tool, wake: wake.cloned() });
    }

    pub fn install_cli(&mut self, wake: Option<&Waker>) {
        self.cli = CliStatus::Installing;
        let _ = self.worker().send(Job::InstallCli { wake: wake.cloned() });
    }

    pub fn disconnect_all(&mut self, wake: Option<&Waker>) {
        for (_, link) in self.rows.iter_mut().flatten() {
            if matches!(link, Link::Connected | Link::Outdated) {
                *link = Link::Disconnecting;
            }
        }
        let _ = self.worker().send(Job::DisconnectAll { wake: wake.cloned() });
    }

    pub fn take(&mut self) -> bool {
        let msgs = match self.inbox.lock() {
            Ok(mut inbox) => std::mem::take(&mut *inbox),
            Err(_) => return false,
        };
        let changed = !msgs.is_empty();
        for msg in msgs {
            match msg {
                Msg::Scanned(mut rows, icons, cli) => {
                    self.scanning = false;
                    self.icons.extend(icons);
                    for (tool, link) in &mut rows {
                        if let Some(busy @ (Link::Connecting | Link::Disconnecting)) = self.link(*tool) {
                            *link = busy.clone();
                        }
                    }
                    self.rows = Some(rows);
                    if self.cli != CliStatus::Installing {
                        self.cli = cli;
                    }
                }
                Msg::Linked(tool, link) => {
                    if link != Link::Missing {
                        self.uninstalled = false;
                    }
                    self.set(tool, link);
                }
                Msg::Uninstalled(done) => self.uninstalled = done,
                Msg::Cli(status) => self.cli = status,
            }
        }
        changed
    }

    fn link(&self, tool: Tool) -> Option<&Link> {
        self.rows.as_ref()?.iter().find(|(t, _)| *t == tool).map(|(_, l)| l)
    }

    fn set(&mut self, tool: Tool, link: Link) {
        if let Some(row) = self.rows.as_mut().and_then(|rows| rows.iter_mut().find(|(t, _)| *t == tool)) {
            row.1 = link;
        }
    }
}

fn unlink(tool: Tool, bin: &std::path::Path) -> Link {
    let skipped = setup::disconnect(tool).into_iter().find_map(|(_, outcome)| match outcome {
        Outcome::Skipped(why) => Some(why),
        _ => None,
    });
    match (setup::status(tool, bin), skipped) {
        (Status::Missing, _) => Link::Missing,
        (_, Some(why)) => Link::Failed { action: ToolAction::Disconnect, reason: why },
        (status, None) => status.into(),
    }
}

fn send(inbox: &Mutex<Vec<Msg>>, wake: Option<&Waker>, msg: Msg) {
    if let Ok(mut inbox) = inbox.lock() {
        inbox.push(msg);
    }
    if let Some(wake) = wake {
        wake();
    }
}
