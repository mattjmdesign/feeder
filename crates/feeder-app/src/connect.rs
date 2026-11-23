//! Settings → Connected Tools: the coding tools installed on this Mac, whether each one starts Feeder's
//! MCP server, and connecting one with a press. Scans and config edits read and write files in
//! the user's home folder, so they run on a worker thread and land through `State::poll`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use feeder_core::setup::{self, Outcome, Status, Tool};
use rune::Waker;

/// Where one installed tool stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connected,
    /// Its config starts another copy of Feeder (an older install, or one config of several).
    Outdated,
    Missing,
    Connecting,
    Disconnecting,
    /// The last connect or disconnect failed, with the tool's own reason.
    Failed(String),
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

enum Msg {
    /// The installed tools, and the apps whose icons stand for those without a logo.
    Scanned(Vec<(Tool, Link)>, Vec<(String, PathBuf)>),
    Linked(Tool, Link),
    Uninstalled,
}

#[derive(Default)]
pub struct Agents {
    /// The installed tools in `Tool::ALL` order; None until the first scan lands.
    pub rows: Option<Vec<(Tool, Link)>>,
    /// Scanned since Settings last opened.
    fresh: bool,
    /// Every tool was disconnected and the skills deleted this session.
    pub uninstalled: bool,
    /// App icons to load, by tool name, for tools whose mark isn't bundled.
    pub icons: Vec<(String, PathBuf)>,
    inbox: Arc<Mutex<Vec<Msg>>>,
}

impl Agents {
    /// Scans once each time Settings opens, so a tool installed meanwhile shows up.
    pub fn follow_settings(&mut self, open: bool, wake: Option<&Waker>) {
        if !open {
            self.fresh = false;
            return;
        }
        if self.fresh {
            return;
        }
        self.fresh = true;
        let (inbox, wake) = (self.inbox.clone(), wake.cloned());
        std::thread::spawn(move || {
            let bin = setup::feeder_binary();
            let found: Vec<Tool> = Tool::ALL.into_iter().filter(|t| setup::detected(*t)).collect();
            let icons = found
                .iter()
                .filter(|t| !crate::agents::has_logo(t.name()))
                .filter_map(|t| Some((t.name().to_string(), setup::app_path(*t)?)))
                .collect();
            let rows = found.into_iter().map(|t| (t, setup::status(t, &bin).into())).collect();
            send(&inbox, wake.as_ref(), Msg::Scanned(rows, icons));
        });
    }

    /// Adds Feeder to `tool`'s configs and installs the skills.
    pub fn connect(&mut self, tool: Tool, wake: Option<&Waker>) {
        self.set(tool, Link::Connecting);
        let (inbox, wake) = (self.inbox.clone(), wake.cloned());
        std::thread::spawn(move || {
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
                (_, Some(why)) => Link::Failed(why),
                (status, None) => status.into(),
            };
            send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
        });
    }

    /// Removes Feeder from `tool`'s configs.
    pub fn disconnect(&mut self, tool: Tool, wake: Option<&Waker>) {
        self.set(tool, Link::Disconnecting);
        let (inbox, wake) = (self.inbox.clone(), wake.cloned());
        std::thread::spawn(move || {
            let link = unlink(tool, &setup::feeder_binary());
            send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
        });
    }

    /// Removes Feeder from every tool that still names it, and deletes its skills: everything
    /// to undo before moving Feeder to the Trash.
    pub fn disconnect_all(&mut self, wake: Option<&Waker>) {
        for (_, link) in self.rows.iter_mut().flatten() {
            if matches!(link, Link::Connected | Link::Outdated) {
                *link = Link::Disconnecting;
            }
        }
        let (inbox, wake) = (self.inbox.clone(), wake.cloned());
        std::thread::spawn(move || {
            let bin = setup::feeder_binary();
            for tool in Tool::ALL {
                if setup::status(tool, &bin) != Status::Missing {
                    let link = unlink(tool, &bin);
                    send(&inbox, wake.as_ref(), Msg::Linked(tool, link));
                }
            }
            setup::remove_skills();
            send(&inbox, wake.as_ref(), Msg::Uninstalled);
        });
    }

    /// Takes what the workers finished; true when a row changed.
    pub fn take(&mut self) -> bool {
        let msgs = match self.inbox.lock() {
            Ok(mut inbox) => std::mem::take(&mut *inbox),
            Err(_) => return false,
        };
        let changed = !msgs.is_empty();
        for msg in msgs {
            match msg {
                Msg::Scanned(mut rows, icons) => {
                    self.icons.extend(icons);
                    // A connect still running outlives the scan that started before it finished.
                    for (tool, link) in &mut rows {
                        if let Some(busy @ (Link::Connecting | Link::Disconnecting)) = self.link(*tool) {
                            *link = busy.clone();
                        }
                    }
                    self.rows = Some(rows);
                }
                Msg::Linked(tool, link) => {
                    if link != Link::Missing {
                        self.uninstalled = false;
                    }
                    self.set(tool, link);
                }
                Msg::Uninstalled => self.uninstalled = true,
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

/// Disconnects `tool`, then reads back where it stands.
fn unlink(tool: Tool, bin: &std::path::Path) -> Link {
    let skipped = setup::disconnect(tool).into_iter().find_map(|(_, outcome)| match outcome {
        Outcome::Skipped(why) => Some(why),
        _ => None,
    });
    match (setup::status(tool, bin), skipped) {
        (Status::Missing, _) => Link::Missing,
        (_, Some(why)) => Link::Failed(why),
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
