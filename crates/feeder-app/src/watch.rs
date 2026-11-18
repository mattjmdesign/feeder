//! Keeps the window in step with the store without touching disk on the UI thread. A worker owns
//! its own connection, checks SQLite's `data_version` a few times a second (it moves whenever the
//! CLI or an MCP server commits) and hands the view a fresh snapshot when anything changed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use feeder_core::{FeedUpdate, OnSimilar, Page, PageDraft, PageFilter, ProjectSummary, Reply, Settings, Standing, Store, TodoItem, now_ms};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Want {
    pub project: Option<i64>,
    pub limit: usize,
}

/// How many timeline posts to load at first, and how many more each time the end comes near.
pub const TIMELINE_PAGE: usize = 80;

enum Msg {
    Want(Want),
    Search(String),
    Rename(i64, String),
    SetBrief(i64, Option<String>),
    SetArchived(i64, bool),
    SetPinned(i64, bool),
    MarkSeen(i64),
    MarkAllSeen,
    EditUpdate(i64, String),
    DeleteUpdate(i64),
    AddReply(i64, String),
    AddTodo(i64, String),
    SetTodoDone(i64, bool),
    RenameTodo(i64, String),
    DeleteTodo(i64),
    ClearCompletedTodos(i64),
    ClearProjectTodos(i64),
    SaveSettings(Settings),
    Notebook(NotebookOp),
    /// Load the timeline's first `n` posts.
    Timeline(usize),
}

/// A change to the notebook made in the app; the user is the author.
pub enum NotebookOp {
    Create(PageDraft),
    Revise(i64, PageDraft),
    SetStanding(i64, Standing),
    Helped(i64),
    Delete(i64),
}

pub struct Snapshot {
    pub projects: Vec<ProjectSummary>,
    pub want: Want,
    pub feed: Vec<FeedUpdate>,
    pub todos: Vec<TodoItem>,
    /// Every project's updates, newest first, `timeline_limit` at most.
    pub timeline: Vec<FeedUpdate>,
    pub timeline_limit: usize,
    /// Replies keyed by `update_id`.
    pub replies: HashMap<i64, Vec<Reply>>,
    pub search_query: String,
    pub search_hits: Vec<FeedUpdate>,
    /// Every notebook page, archived ones included, the most recently revised first.
    pub pages: Vec<Page>,
    /// Agents without a logo whose app is installed, with the app to take the icon from.
    pub agent_apps: Vec<(String, PathBuf)>,
    pub error: Option<String>,
    pub mutation_error: Option<String>,
}

enum TodoOp {
    Add(i64, String),
    SetDone(i64, bool),
    Rename(i64, String),
    Delete(i64),
    ClearCompleted(i64),
    ClearAll(i64),
}

#[derive(Clone)]
pub struct Watcher {
    tx: Sender<Msg>,
    latest: Arc<Mutex<Option<Snapshot>>>,
}

impl Watcher {
    pub fn start(wake: rune::Waker) -> Watcher {
        let (tx, rx) = mpsc::channel::<Msg>();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        std::thread::Builder::new()
            .name("feeder-watch".into())
            .spawn(move || {
                let mut store = None;
                let mut want = Want { project: None, limit: 0 };
                let mut search_query = String::new();
                let mut timeline_limit = TIMELINE_PAGE;
                let mut apps: HashMap<String, Option<PathBuf>> = Default::default();
                let mut seen: Option<i64> = None;
                let mut next_pulse_expiry: Option<i64> = None;
                let mut dirty = true;
                loop {
                    let mut renames = Vec::new();
                    let mut briefs = Vec::new();
                    let mut archives = Vec::new();
                    let mut pins = Vec::new();
                    let mut seens = Vec::new();
                    let mut mark_all = false;
                    let mut edits = Vec::new();
                    let mut deletes = Vec::new();
                    let mut new_replies = Vec::new();
                    let mut todo_ops = Vec::new();
                    let mut notebook_ops = Vec::new();
                    let mut mutation_error: Option<String> = None;
                    match rx.recv_timeout(Duration::from_millis(400)) {
                        Ok(first) => {
                            for msg in std::iter::once(first).chain(rx.try_iter()) {
                                match msg {
                                    Msg::Want(w) => want = w,
                                    Msg::Search(q) => search_query = q,
                                    Msg::Timeline(n) => timeline_limit = timeline_limit.max(n),
                                    Msg::Rename(id, name) => renames.push((id, name)),
                                    Msg::SetBrief(id, brief) => briefs.push((id, brief)),
                                    Msg::SetArchived(id, archived) => archives.push((id, archived)),
                                    Msg::SetPinned(id, pinned) => pins.push((id, pinned)),
                                    Msg::MarkSeen(id) => seens.push(id),
                                    Msg::MarkAllSeen => mark_all = true,
                                    Msg::EditUpdate(id, body) => edits.push((id, body)),
                                    Msg::DeleteUpdate(id) => deletes.push(id),
                                    Msg::AddReply(update_id, body) => new_replies.push((update_id, body)),
                                    Msg::AddTodo(pid, title) => todo_ops.push(TodoOp::Add(pid, title)),
                                    Msg::SetTodoDone(id, done) => todo_ops.push(TodoOp::SetDone(id, done)),
                                    Msg::RenameTodo(id, title) => todo_ops.push(TodoOp::Rename(id, title)),
                                    Msg::DeleteTodo(id) => todo_ops.push(TodoOp::Delete(id)),
                                    Msg::ClearCompletedTodos(pid) => todo_ops.push(TodoOp::ClearCompleted(pid)),
                                    Msg::ClearProjectTodos(pid) => todo_ops.push(TodoOp::ClearAll(pid)),
                                    Msg::Notebook(op) => notebook_ops.push(op),
                                    Msg::SaveSettings(settings) => {
                                        if let Err(e) = settings.save(&feeder_core::data_dir()) {
                                            mutation_error = Some(format!("Couldn't save settings: {e}"));
                                        }
                                    }
                                }
                            }
                            dirty = true;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    if store.is_none() {
                        match Store::open_default() {
                            Ok(s) => store = Some(s),
                            Err(e) => {
                                publish(
                                    &slot,
                                    &wake,
                                    Snapshot {
                                        projects: vec![],
                                        want,
                                        feed: vec![],
                                        todos: vec![],
                                        timeline: vec![],
                                        timeline_limit,
                                        replies: HashMap::new(),
                                        search_query: search_query.clone(),
                                        search_hits: vec![],
                                        pages: vec![],
                                        agent_apps: vec![],
                                        error: Some(e.to_string()),
                                        mutation_error,
                                    },
                                );
                                std::thread::sleep(Duration::from_secs(2));
                                continue;
                            }
                        }
                    }
                    let Some(s) = store.as_ref() else { continue };
                    for (id, name) in renames {
                        if let Err(e) = s.rename_project(id, &name) {
                            mutation_error = Some(format!("Rename failed: {e}"));
                        }
                    }
                    for (id, brief) in briefs {
                        if let Err(e) = s.set_project_brief(id, brief.as_deref()) {
                            mutation_error = Some(format!("Brief update failed: {e}"));
                        }
                    }
                    for (id, archived) in archives {
                        if let Err(e) = s.set_archived(id, archived) {
                            mutation_error = Some(format!("Archive failed: {e}"));
                        }
                    }
                    for (id, pinned) in pins {
                        if let Err(e) = s.set_pinned(id, pinned) {
                            mutation_error = Some(format!("Pin failed: {e}"));
                        }
                    }
                    if mark_all {
                        if let Err(e) = s.mark_all_seen() {
                            mutation_error = Some(format!("Mark read failed: {e}"));
                        }
                    } else {
                        for id in seens {
                            if let Err(e) = s.mark_seen(id) {
                                mutation_error = Some(format!("Mark read failed: {e}"));
                            }
                        }
                    }
                    for (id, body) in edits {
                        if let Err(e) = s.edit_update(id, &body) {
                            mutation_error = Some(format!("Edit failed: {e}"));
                        }
                    }
                    for id in deletes {
                        if let Err(e) = s.delete_update(id) {
                            mutation_error = Some(format!("Delete failed: {e}"));
                        }
                    }
                    for (update_id, body) in new_replies {
                        if let Err(e) = s.add_reply(update_id, "You", true, &body) {
                            mutation_error = Some(format!("Reply failed: {e}"));
                        }
                    }
                    for op in todo_ops {
                        let res = match op {
                            TodoOp::Add(pid, title) => s.add_todo(pid, &title).map(|_| ()),
                            TodoOp::SetDone(id, done) => s.set_todo_done(id, done).map(|_| ()),
                            TodoOp::Rename(id, title) => s.rename_todo(id, &title).map(|_| ()),
                            TodoOp::Delete(id) => s.delete_todo(id),
                            TodoOp::ClearCompleted(pid) => s.clear_completed_todos(pid).map(|_| ()),
                            TodoOp::ClearAll(pid) => s.clear_project_todos(pid).map(|_| ()),
                        };
                        if let Err(e) = res {
                            mutation_error = Some(format!("To-do update failed: {e}"));
                        }
                    }
                    for op in notebook_ops {
                        let res = match op {
                            // The user curates by hand, so a page they file deliberately is kept even if it
                            // resembles another.
                            NotebookOp::Create(draft) => s.create_page(draft, "You", None, OnSimilar::Allow).map(|_| ()),
                            NotebookOp::Revise(id, draft) => s.revise_page(id, draft, "You").map(|_| ()),
                            NotebookOp::SetStanding(id, standing) => s.set_page_standing(id, standing),
                            NotebookOp::Helped(id) => s.mark_page_helped(id).map(|_| ()),
                            NotebookOp::Delete(id) => s.delete_page(id),
                        };
                        if let Err(e) = res {
                            mutation_error = Some(format!("Notebook change failed: {e}"));
                        }
                    }

                    let version = s.data_version().ok();
                    let pulse_expired = next_pulse_expiry.is_some_and(|exp| now_ms() >= exp);
                    if !dirty && !pulse_expired && mutation_error.is_none() && version == seen {
                        continue;
                    }
                    seen = version;
                    dirty = false;
                    let loaded = s.projects().and_then(|projects| {
                        let (feed, todos) = match want.project {
                            Some(id) => (s.feed(id, want.limit, None)?, s.todos(id)?),
                            None => (vec![], vec![]),
                        };
                        let timeline = s.timeline(timeline_limit)?;
                        let search_hits =
                            if search_query.trim().is_empty() { vec![] } else { s.search_updates(&search_query, None, true, 50)? };
                        let mut ids: Vec<i64> = feed.iter().chain(&timeline).chain(&search_hits).map(|u| u.id).collect();
                        ids.sort_unstable();
                        ids.dedup();
                        let flat_replies = s.replies_for_updates(&ids)?;
                        let mut replies: HashMap<i64, Vec<Reply>> = HashMap::new();
                        for r in flat_replies {
                            replies.entry(r.update_id).or_default().push(r);
                        }
                        let pages = s.pages(&PageFilter { include_archived: true, ..PageFilter::default() })?;
                        Ok((projects, feed, todos, timeline, replies, search_hits, pages))
                    });
                    let snapshot = match loaded {
                        Ok((projects, feed, todos, timeline, replies, search_hits, pages)) => {
                            next_pulse_expiry = projects.iter().filter_map(|p| p.active_pulse.as_ref().map(|pu| pu.expires_at)).min();
                            let agents = projects
                                .iter()
                                .filter_map(|p| p.last_agent.clone())
                                .chain(feed.iter().chain(&timeline).chain(&search_hits).map(|u| u.agent.clone()))
                                .chain(pages.iter().map(|pg| pg.agent.clone()));
                            for agent in agents {
                                apps.entry(agent.clone()).or_insert_with(|| {
                                    if crate::agents::has_logo(&agent) {
                                        return None;
                                    }
                                    crate::agents::app_candidates(&agent).into_iter().find(|p| p.exists())
                                });
                            }
                            let agent_apps = apps.iter().filter_map(|(a, p)| Some((a.clone(), p.clone()?))).collect();
                            Snapshot {
                                projects,
                                want,
                                feed,
                                todos,
                                timeline,
                                timeline_limit,
                                replies,
                                search_query: search_query.clone(),
                                search_hits,
                                pages,
                                agent_apps,
                                error: None,
                                mutation_error,
                            }
                        }
                        Err(e) => {
                            next_pulse_expiry = None;
                            Snapshot {
                                projects: vec![],
                                want,
                                feed: vec![],
                                todos: vec![],
                                timeline: vec![],
                                timeline_limit,
                                replies: HashMap::new(),
                                search_query: search_query.clone(),
                                search_hits: vec![],
                                pages: vec![],
                                agent_apps: vec![],
                                error: Some(e.to_string()),
                                mutation_error,
                            }
                        }
                    };
                    publish(&slot, &wake, snapshot);
                }
            })
            .expect("spawn watcher");
        Watcher { tx, latest }
    }

    pub fn want(&self, want: Want) {
        self.tx.send(Msg::Want(want)).ok();
    }

    pub fn search(&self, query: String) {
        self.tx.send(Msg::Search(query)).ok();
    }

    /// Renames off the UI thread; the next snapshot carries the new name.
    pub fn rename(&self, id: i64, name: String) {
        self.tx.send(Msg::Rename(id, name)).ok();
    }

    pub fn set_brief(&self, id: i64, brief: Option<String>) {
        self.tx.send(Msg::SetBrief(id, brief)).ok();
    }

    pub fn set_archived(&self, id: i64, archived: bool) {
        self.tx.send(Msg::SetArchived(id, archived)).ok();
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) {
        self.tx.send(Msg::SetPinned(id, pinned)).ok();
    }

    pub fn mark_seen(&self, id: i64) {
        self.tx.send(Msg::MarkSeen(id)).ok();
    }

    pub fn mark_all_seen(&self) {
        self.tx.send(Msg::MarkAllSeen).ok();
    }

    pub fn edit_update(&self, id: i64, body: String) {
        self.tx.send(Msg::EditUpdate(id, body)).ok();
    }

    pub fn delete_update(&self, id: i64) {
        self.tx.send(Msg::DeleteUpdate(id)).ok();
    }

    pub fn add_reply(&self, update_id: i64, body: String) {
        self.tx.send(Msg::AddReply(update_id, body)).ok();
    }

    pub fn add_todo(&self, project_id: i64, title: String) {
        self.tx.send(Msg::AddTodo(project_id, title)).ok();
    }

    pub fn set_todo_done(&self, id: i64, done: bool) {
        self.tx.send(Msg::SetTodoDone(id, done)).ok();
    }

    pub fn rename_todo(&self, id: i64, title: String) {
        self.tx.send(Msg::RenameTodo(id, title)).ok();
    }

    pub fn delete_todo(&self, id: i64) {
        self.tx.send(Msg::DeleteTodo(id)).ok();
    }

    pub fn clear_completed_todos(&self, project_id: i64) {
        self.tx.send(Msg::ClearCompletedTodos(project_id)).ok();
    }

    pub fn clear_project_todos(&self, project_id: i64) {
        self.tx.send(Msg::ClearProjectTodos(project_id)).ok();
    }

    /// Asks for the timeline's first `n` posts; callable from scroll handlers, which can't
    /// change state themselves.
    pub fn timeline(&self, n: usize) {
        self.tx.send(Msg::Timeline(n)).ok();
    }

    pub fn notebook(&self, op: NotebookOp) {
        self.tx.send(Msg::Notebook(op)).ok();
    }

    pub fn save_settings(&self, settings: Settings) {
        self.tx.send(Msg::SaveSettings(settings)).ok();
    }

    pub fn take(&self) -> Option<Snapshot> {
        self.latest.lock().ok()?.take()
    }
}

fn publish(slot: &Mutex<Option<Snapshot>>, wake: &rune::Waker, snapshot: Snapshot) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(snapshot);
    }
    wake();
}
