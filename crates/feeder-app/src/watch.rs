use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use feeder_core::gardener::{Backend, ChatSummary, Run, RunStatus};
use feeder_core::{
    Change, FeedUpdate, Flag, FlagStatus, GARDENER, HUMAN_AUTHOR, OnSimilar, Page, PageDraft, PageFilter, ProjectSummary, Reply, Settings,
    Standing, Store, WorkingTree, now_ms, read_working_tree,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Want {
    pub project: Option<i64>,
    pub limit: usize,
    pub files: bool,
    pub visible: bool,
}

pub const TIMELINE_PAGE: usize = 25;

enum Msg {
    Want(Want),
    Search(String),
    Rename(i64, String),
    SetBrief(i64, Option<String>),
    SetArchived(i64, bool),
    SetPinned(i64, bool),
    MarkSeen(i64),
    MarkAllSeen,
    SetAttentionDismissed(i64, bool),
    EditUpdate(i64, String),
    DeleteUpdate(i64),
    AddReply(i64, String),
    SaveSettings(Settings),
    Notebook(NotebookOp),
    Timeline(usize),
}

#[derive(Clone, Debug)]
pub enum FailedWrite {
    Reply { update_id: i64, body: String },
    EditUpdate { update_id: i64, body: String },
    Brief { project_id: i64, body: Option<String> },
}

pub enum NotebookOp {
    Create(PageDraft),
    Revise(i64, PageDraft),
    ToggleTask(i64, usize, bool),
    SetStanding(i64, Standing),
    Delete(i64),
    Revert(i64),
    FlagUpdate(i64),
    DeleteChat(i64),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GardenerView {
    pub changes: Vec<Change>,
    pub open_flags: Vec<Flag>,
    pub last_run: Option<Run>,
    pub running: Option<Run>,
    pub chats: Vec<ChatSummary>,
    pub backends: Vec<Backend>,
}

const GARDENER_LIST: usize = 30;

fn gardener_view(s: &Store, backends: &[Backend]) -> feeder_core::Result<GardenerView> {
    Ok(GardenerView {
        changes: s.changes(Some(GARDENER), None, GARDENER_LIST)?,
        open_flags: s.flags(Some(FlagStatus::Open), GARDENER_LIST)?,
        last_run: s.gardener_runs(1)?.into_iter().find(|r| r.status != RunStatus::Running),
        running: s.running_gardener()?,
        chats: s.curator_chats(GARDENER_LIST)?,
        backends: backends.to_vec(),
    })
}

pub struct Snapshot {
    pub projects: Vec<ProjectSummary>,
    pub want: Want,
    pub feed: Vec<FeedUpdate>,
    pub timeline: Vec<FeedUpdate>,
    pub timeline_limit: usize,
    pub replies: HashMap<i64, Vec<Reply>>,
    pub search_query: String,
    pub search_hits: Vec<FeedUpdate>,
    pub pages: Option<Vec<Page>>,
    pub gardener: Option<GardenerView>,
    pub agent_apps: Vec<(String, PathBuf)>,
    pub error: Option<String>,
    pub mutation_error: Option<String>,
    pub failed_writes: Vec<FailedWrite>,
}

#[derive(Default)]
struct FilesRefresh {
    project: Option<i64>,
    scanned_at: Option<Instant>,
    published: Option<(Instant, WorkingTree)>,
}

impl FilesRefresh {
    fn select(&mut self, want: Want) {
        let project = want.project.filter(|_| want.files);
        if self.project != project {
            *self = Self { project, ..Self::default() };
        }
    }

    fn poll(&mut self, now: Instant, read: impl FnOnce(i64) -> Option<WorkingTree>) -> Option<(i64, WorkingTree)> {
        let id = self.project?;
        if self.scanned_at.is_some_and(|last| now.duration_since(last) < Duration::from_secs(2)) {
            return None;
        }
        self.scanned_at = Some(now);
        let mut tree = read(id)?;
        if let Some((last, previous)) = &self.published
            && now.duration_since(*last) < Duration::from_secs(30)
        {
            let read_at = tree.read_at;
            tree.read_at = previous.read_at;
            if tree == *previous {
                return None;
            }
            tree.read_at = read_at;
        }
        self.published = Some((now, tree.clone()));
        Some((id, tree))
    }
}

#[derive(Clone)]
pub struct Watcher {
    tx: Sender<Msg>,
    latest: Arc<Mutex<Option<Snapshot>>>,
    files: Arc<Mutex<Option<(i64, WorkingTree)>>>,
}

impl Watcher {
    pub fn start(wake: rune::Waker) -> Watcher {
        let (tx, rx) = mpsc::channel::<Msg>();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        let files = Arc::new(Mutex::new(None));
        let files_slot = files.clone();
        std::thread::Builder::new()
            .name("feeder-watch".into())
            .spawn(move || {
                let mut store = None;
                let mut want = Want::default();
                let mut files_refresh = FilesRefresh::default();
                let mut search_query = String::new();
                let mut timeline_limit = TIMELINE_PAGE;
                let mut apps: HashMap<String, Option<PathBuf>> = Default::default();
                let mut seen: Option<i64> = None;
                let mut last_notebook_stamp: Option<(i64, i64, i64, i64)> = None;
                let mut next_pulse_expiry: Option<i64> = None;
                let mut dirty = true;
                let backends = Backend::installed();
                loop {
                    let mut renames = Vec::new();
                    let mut briefs = Vec::new();
                    let mut archives = Vec::new();
                    let mut pins = Vec::new();
                    let mut seens = Vec::new();
                    let mut mark_all = false;
                    let mut dismissals = Vec::new();
                    let mut edits = Vec::new();
                    let mut deletes = Vec::new();
                    let mut new_replies = Vec::new();
                    let mut notebook_ops = Vec::new();
                    let mut mutation_error: Option<String> = None;
                    let mut failed_writes = Vec::new();
                    let poll_interval = if want.visible { Duration::from_millis(400) } else { Duration::from_secs(2) };
                    match rx.recv_timeout(poll_interval) {
                        Ok(first) => {
                            for msg in std::iter::once(first).chain(rx.try_iter()) {
                                match msg {
                                    Msg::Want(w) => {
                                        want = w;
                                        files_refresh.select(w);
                                    }
                                    Msg::Search(q) => search_query = q,
                                    Msg::Timeline(n) => timeline_limit = timeline_limit.max(n),
                                    Msg::Rename(id, name) => renames.push((id, name)),
                                    Msg::SetBrief(id, brief) => briefs.push((id, brief)),
                                    Msg::SetArchived(id, archived) => archives.push((id, archived)),
                                    Msg::SetPinned(id, pinned) => pins.push((id, pinned)),
                                    Msg::MarkSeen(id) => seens.push(id),
                                    Msg::MarkAllSeen => mark_all = true,
                                    Msg::SetAttentionDismissed(id, dismissed) => dismissals.push((id, dismissed)),
                                    Msg::EditUpdate(id, body) => edits.push((id, body)),
                                    Msg::DeleteUpdate(id) => deletes.push(id),
                                    Msg::AddReply(update_id, body) => new_replies.push((update_id, body)),
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
                                for (project_id, body) in briefs {
                                    failed_writes.push(FailedWrite::Brief { project_id, body });
                                }
                                for (update_id, body) in edits {
                                    failed_writes.push(FailedWrite::EditUpdate { update_id, body });
                                }
                                for (update_id, body) in new_replies {
                                    failed_writes.push(FailedWrite::Reply { update_id, body });
                                }
                                publish(
                                    &slot,
                                    &wake,
                                    Snapshot {
                                        projects: vec![],
                                        want,
                                        feed: vec![],
                                        timeline: vec![],
                                        timeline_limit,
                                        replies: HashMap::new(),
                                        search_query: search_query.clone(),
                                        search_hits: vec![],
                                        pages: None,
                                        gardener: None,
                                        agent_apps: vec![],
                                        error: Some(e.to_string()),
                                        mutation_error,
                                        failed_writes,
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
                            failed_writes.push(FailedWrite::Brief { project_id: id, body: brief });
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
                    for (id, dismissed) in dismissals {
                        let res = if dismissed { s.dismiss_attention(id) } else { s.undismiss_attention(id) };
                        if let Err(e) = res {
                            mutation_error = Some(format!("Dismiss failed: {e}"));
                        }
                    }
                    for (id, body) in edits {
                        if let Err(e) = s.edit_update(id, &body) {
                            mutation_error = Some(format!("Edit failed: {e}"));
                            failed_writes.push(FailedWrite::EditUpdate { update_id: id, body });
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
                            failed_writes.push(FailedWrite::Reply { update_id, body });
                        }
                    }
                    for op in notebook_ops {
                        let res = match op {
                            NotebookOp::Create(draft) => s.create_page(draft, "You", None, OnSimilar::Allow).map(|_| ()),
                            NotebookOp::Revise(id, draft) => s.revise_page(id, draft, "You").map(|_| ()),
                            NotebookOp::ToggleTask(id, task_index, checked) => s.toggle_page_task(id, task_index, checked).map(|_| ()),
                            NotebookOp::SetStanding(id, standing) => s.set_page_standing(id, standing),
                            NotebookOp::Delete(id) => s.delete_page(id),
                            NotebookOp::Revert(id) => s.revert_change(id, HUMAN_AUTHOR).map(|_| ()),
                            NotebookOp::FlagUpdate(update_id) => s.flag_update_for_notebook(update_id, HUMAN_AUTHOR).map(|_| ()),
                            NotebookOp::DeleteChat(run_id) => s.delete_curator_chat(run_id),
                        };
                        if let Err(e) = res {
                            mutation_error = Some(format!("Notebook change failed: {e}"));
                        }
                    }

                    let version = s.data_version().ok();
                    let pulse_expired = next_pulse_expiry.is_some_and(|exp| now_ms() >= exp);
                    if !dirty && !pulse_expired && mutation_error.is_none() && version == seen {
                        publish_files(&files_slot, &wake, &mut files_refresh, s);
                        continue;
                    }
                    seen = version;
                    dirty = false;
                    let nb_stamp = s.notebook_stamp().ok();
                    let need_pages = nb_stamp.is_none() || nb_stamp != last_notebook_stamp;
                    let loaded = s.projects().and_then(|projects| {
                        let feed = match want.project {
                            Some(id) => s.feed(id, want.limit, None)?,
                            None => vec![],
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
                        let pages =
                            if need_pages { Some(s.pages(&PageFilter { include_archived: true, ..PageFilter::default() })?) } else { None };
                        let gardener = gardener_view(s, &backends)?;
                        Ok((projects, feed, timeline, replies, search_hits, pages, gardener))
                    });
                    let snapshot = match loaded {
                        Ok((projects, feed, timeline, replies, search_hits, pages, gardener)) => {
                            if pages.is_some() {
                                last_notebook_stamp = nb_stamp;
                            }
                            next_pulse_expiry = projects.iter().filter_map(|p| p.active_pulse.as_ref().map(|pu| pu.expires_at)).min();
                            let agents = projects
                                .iter()
                                .filter_map(|p| p.last_agent.clone())
                                .chain(projects.iter().filter_map(|p| p.last_via.clone()))
                                .chain(feed.iter().chain(&timeline).chain(&search_hits).map(|u| u.agent.clone()))
                                .chain(feed.iter().chain(&timeline).chain(&search_hits).filter_map(|u| u.via.clone()))
                                .chain(pages.iter().flatten().map(|pg| pg.agent.clone()))
                                .chain(backends.iter().map(|b| b.label().to_string()));
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
                                timeline,
                                timeline_limit,
                                replies,
                                search_query: search_query.clone(),
                                search_hits,
                                pages,
                                gardener: Some(gardener),
                                agent_apps,
                                error: None,
                                mutation_error,
                                failed_writes,
                            }
                        }
                        Err(e) => {
                            next_pulse_expiry = None;
                            Snapshot {
                                projects: vec![],
                                want,
                                feed: vec![],
                                timeline: vec![],
                                timeline_limit,
                                replies: HashMap::new(),
                                search_query: search_query.clone(),
                                search_hits: vec![],
                                pages: None,
                                gardener: None,
                                agent_apps: vec![],
                                error: Some(e.to_string()),
                                mutation_error,
                                failed_writes,
                            }
                        }
                    };
                    publish(&slot, &wake, snapshot);
                    publish_files(&files_slot, &wake, &mut files_refresh, s);
                }
            })
            .expect("spawn watcher");
        Watcher { tx, latest, files }
    }

    pub fn want(&self, want: Want) {
        self.tx.send(Msg::Want(want)).ok();
    }

    pub fn search(&self, query: String) {
        self.tx.send(Msg::Search(query)).ok();
    }

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

    pub fn dismiss_attention(&self, update_id: i64) {
        self.tx.send(Msg::SetAttentionDismissed(update_id, true)).ok();
    }

    pub fn undismiss_attention(&self, update_id: i64) {
        self.tx.send(Msg::SetAttentionDismissed(update_id, false)).ok();
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

    pub fn take_files(&self) -> Option<(i64, WorkingTree)> {
        self.files.lock().ok()?.take()
    }
}

fn publish(slot: &Mutex<Option<Snapshot>>, wake: &rune::Waker, snapshot: Snapshot) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(snapshot);
    }
    wake();
}

fn publish_files(slot: &Mutex<Option<(i64, WorkingTree)>>, wake: &rune::Waker, refresh: &mut FilesRefresh, store: &Store) {
    let tree = refresh.poll(Instant::now(), |id| {
        let project = store.project(id).ok().flatten()?;
        Some(read_working_tree(project.path.as_deref()))
    });
    if let Some(tree) = tree
        && let Ok(mut pending) = slot.lock()
    {
        *pending = Some(tree);
        drop(pending);
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_refresh_is_scoped_throttled_and_suppresses_timestamp_only_changes() {
        let mut files = FilesRefresh::default();
        let start = Instant::now();
        let sample = read_working_tree(None);
        let at = |seconds| start + Duration::from_secs(seconds);
        let read = |id| {
            assert_eq!(id, 1);
            Some(sample.clone())
        };
        files.select(Want { project: Some(1), ..Want::default() });
        assert!(files.poll(start, |_| panic!("closed view read Git")).is_none());
        files.select(Want { project: Some(1), files: true, ..Want::default() });
        assert_eq!(files.poll(start, read), Some((1, sample.clone())));
        assert!(files.poll(at(1), |_| panic!("read too early")).is_none());
        assert!(
            files
                .poll(at(2), |id| {
                    let mut tree = read(id)?;
                    tree.read_at += 2_000;
                    Some(tree)
                })
                .is_none()
        );
        assert!(files.poll(at(30), read).is_some());
        assert!(
            files
                .poll(at(32), |id| {
                    let mut tree = read(id)?;
                    tree.branch = Some("new-branch".into());
                    Some(tree)
                })
                .is_some()
        );
        files.select(Want { project: Some(2), files: true, ..Want::default() });
        assert_eq!(files.poll(at(32), |_| Some(sample.clone())), Some((2, sample)));
    }
}
