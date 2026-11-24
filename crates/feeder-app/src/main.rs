mod agents;
mod chrome;
mod connect;
mod curator_settings;
mod curator_sign_in;
mod feed;
mod files;
mod gardener;
mod home;
mod icon;
mod keys;
mod logos;
mod media;
mod notebook;
#[cfg(target_os = "macos")]
mod notify;
mod orb;
mod prose;
mod style;
mod timeline;
#[cfg(feeder_sparkle)]
mod updater;
mod watch;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use feeder_core::{FeedReviewStatus, FeedUpdate, ProjectSummary, Reply, Settings, Theme, now_ms, parse_attention};
use rune::{
    AppSpec, ClipboardContent, ClipboardImage, Cx, El, Image, ImageCache, Player, PointerPress, Taskbar, TextEdit, Tx, Waker, WindowHandle,
    col, el,
};
use rune_kit::search::Fold;
use rune_kit::shell::TOOLBAR_HEIGHT;
use rune_kit::{ImageZoom, PaneState, ToastEvent, ToastStack};

pub use media::{MediaSlide, media_slides};
use watch::{Want, Watcher};

const PAGE: usize = 25;
pub const STARTER_PROMPT: &str = "Post a short Feeder update for this project.";

static WINDOW_HANDLE: OnceLock<Mutex<Option<WindowHandle>>> = OnceLock::new();

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

fn window_slot() -> &'static Mutex<Option<WindowHandle>> {
    WINDOW_HANDLE.get_or_init(|| Mutex::new(None))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Home,
    Feed,
    Notebook,
    Curator,
}

impl Tab {
    pub const PRIMARY: [Tab; 3] = [Tab::Home, Tab::Feed, Tab::Notebook];
    pub const ALL: [Tab; 4] = [Tab::Home, Tab::Feed, Tab::Notebook, Tab::Curator];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Feed => "Feed",
            Tab::Notebook => "Library",
            Tab::Curator => "Curator",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HomeFilter {
    #[default]
    All,
    NeedsYou,
    Working,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingStep {
    Connect,
    FirstUpdate,
    Curator,
}

impl OnboardingStep {
    pub const ALL: [OnboardingStep; 3] = [OnboardingStep::Connect, OnboardingStep::FirstUpdate, OnboardingStep::Curator];

    pub fn index(self) -> usize {
        match self {
            OnboardingStep::Connect => 0,
            OnboardingStep::FirstUpdate => 1,
            OnboardingStep::Curator => 2,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            OnboardingStep::Connect => "connect",
            OnboardingStep::FirstUpdate => "first-update",
            OnboardingStep::Curator => "curator",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OnboardingStep::Connect => "Connect tools",
            OnboardingStep::FirstUpdate => "First update",
            OnboardingStep::Curator => "Curator",
        }
    }

    pub fn next(self) -> Option<OnboardingStep> {
        match self {
            OnboardingStep::Connect => Some(OnboardingStep::FirstUpdate),
            OnboardingStep::FirstUpdate => Some(OnboardingStep::Curator),
            OnboardingStep::Curator => None,
        }
    }

    pub fn prev(self) -> Option<OnboardingStep> {
        match self {
            OnboardingStep::Connect => None,
            OnboardingStep::FirstUpdate => Some(OnboardingStep::Connect),
            OnboardingStep::Curator => Some(OnboardingStep::FirstUpdate),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToastKind {
    Info,
    UndoArchive { project_id: i64, was_archived: bool },
    UndoNoteArchive { page_id: i64, was_archived: bool },
    UndoDismiss { update_id: i64 },
    Flagged { project_id: Option<i64> },
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToastItem {
    pub title: String,
    pub description: Option<String>,
    pub kind: ToastKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyTarget {
    McpConfig,
    StarterPrompt,
    ProjectContext,
    LightboxImage,
    Update(i64),
    Reply(i64),
    NotebookPage(i64),
    FilePreview(u64),
}

pub struct State {
    pub tab: Tab,
    pub notebook: notebook::Notebook,
    pub curator_pane: PaneState,
    pub curator_search: TextEdit,
    pub files: files::FilesState,
    curator_sign_in: curator_sign_in::SignIn,
    pub curator: curator_settings::Curator,
    window_visible: bool,
    pub projects: Vec<ProjectSummary>,
    pub timeline: Vec<FeedUpdate>,
    pub timeline_limit: usize,
    pub replies: HashMap<i64, Vec<Reply>>,
    pub review_statuses: HashMap<i64, FeedReviewStatus>,
    timeline_asked: Arc<AtomicUsize>,
    pub loaded: bool,
    pub open: Option<i64>,
    pub feed: Vec<FeedUpdate>,
    pub feed_for: Option<i64>,
    pub limit: usize,
    pub hovered: Option<i64>,
    pub home_filter: HomeFilter,
    pub onboarding_step: Option<OnboardingStep>,
    pub home_selected: Option<i64>,
    pub feed_selected: Option<i64>,
    pub timeline_selected: Option<i64>,
    pub branch_filter: Option<String>,
    pub project_search_open: bool,
    pub project_search: TextEdit,
    pub now: i64,
    pub clock: f64,
    pub window_size: [f32; 2],
    pub error: Option<String>,
    pub editing: Option<TextEdit>,
    pub scratch_edit: TextEdit,
    pub brief_editing: Option<i64>,
    pub brief_draft: TextEdit,
    pub editing_update: Option<i64>,
    pub edit_update_body: TextEdit,
    pub replying_to: Option<i64>,
    pub reply_draft: TextEdit,
    pub icon: bool,
    pub settings: Settings,
    pub settings_open: bool,
    pub settings_page: chrome::SettingsPage,
    pub agents: connect::Agents,
    #[cfg(feeder_sparkle)]
    pub updates: updater::Updates,
    pub menu_open: bool,
    pub show_archived: bool,
    pub orb_menu: Option<(i64, PointerPress)>,
    pub lightbox: Option<i64>,
    pub lightbox_zoom: ImageZoom,
    pub lightbox_compare: Option<i64>,
    pub lightbox_split: f32,
    pub card_slide: HashMap<i64, usize>,
    pub palette_open: bool,
    pub palette_query: TextEdit,
    pub palette_index: usize,
    pub feed_search: TextEdit,
    pub search_query: String,
    pub search_snapshot_query: String,
    pub search_hits: Vec<FeedUpdate>,
    pub delete_confirm: Option<i64>,
    pub post_menu: Option<i64>,
    pub copy_feedback: Option<(CopyTarget, i64)>,
    pub toasts: ToastStack<ToastItem>,
    pub pending_url: Option<String>,
    pending_scroll: Option<i64>,
    last_badge: Option<usize>,
    watcher: Option<Watcher>,
    pub(crate) wake: Option<Waker>,
    pub(crate) images: Option<ImageCache>,
    pub shots: HashMap<i64, Image>,
    pub media_dirty: bool,
    pub feed_visible_rows: Option<std::ops::Range<usize>>,
    pub project_visible_rows: Option<std::ops::Range<usize>>,
    pub videos: HashMap<i64, Player>,
    pub agent_icons: HashMap<String, Image>,
    shots_ready: usize,
    seen_max_update_id: i64,
    last_pulse_tick: f64,
    last_video_tick: f64,
    pub(crate) clipboard_image_rx: Option<Receiver<Result<ClipboardImage, String>>>,
}

impl State {
    pub(crate) fn new() -> State {
        State {
            tab: Tab::Home,
            notebook: notebook::Notebook::default(),
            curator_pane: PaneState::new(252.0).range(200.0, 360.0),
            curator_search: TextEdit::new(""),
            files: files::FilesState::default(),
            curator_sign_in: curator_sign_in::SignIn::default(),
            curator: curator_settings::Curator::load(),
            window_visible: true,
            projects: vec![],
            timeline: vec![],
            timeline_limit: watch::TIMELINE_PAGE,
            replies: HashMap::new(),
            review_statuses: HashMap::new(),
            timeline_asked: Arc::new(AtomicUsize::new(watch::TIMELINE_PAGE)),
            loaded: false,
            open: None,
            feed: vec![],
            feed_for: None,
            limit: PAGE,
            hovered: None,
            home_filter: HomeFilter::All,
            onboarding_step: None,
            home_selected: None,
            feed_selected: None,
            timeline_selected: None,
            branch_filter: None,
            project_search_open: false,
            project_search: TextEdit::new(""),
            now: now_ms(),
            clock: 0.0,
            window_size: [1180.0, 760.0],
            error: None,
            editing: None,
            scratch_edit: TextEdit::new(""),
            brief_editing: None,
            brief_draft: TextEdit::new(""),
            editing_update: None,
            edit_update_body: TextEdit::new(""),
            replying_to: None,
            reply_draft: TextEdit::new(""),
            icon: false,
            settings: Settings::load(&feeder_core::data_dir()),
            settings_open: false,
            settings_page: chrome::SettingsPage::default(),
            agents: connect::Agents::default(),
            #[cfg(feeder_sparkle)]
            updates: updater::Updates::default(),
            menu_open: false,
            show_archived: false,
            orb_menu: None,
            lightbox: None,
            lightbox_zoom: ImageZoom::new(),
            lightbox_compare: None,
            lightbox_split: 0.5,
            card_slide: HashMap::new(),
            palette_open: false,
            palette_query: TextEdit::new(""),
            palette_index: 0,
            feed_search: TextEdit::new(""),
            search_query: String::new(),
            search_snapshot_query: String::new(),
            search_hits: vec![],
            delete_confirm: None,
            post_menu: None,
            copy_feedback: None,
            toasts: ToastStack::default(),
            pending_url: None,
            pending_scroll: None,
            last_badge: None,
            watcher: None,
            wake: None,
            images: None,
            shots: HashMap::new(),
            media_dirty: false,
            feed_visible_rows: None,
            project_visible_rows: None,
            videos: HashMap::new(),
            agent_icons: HashMap::new(),
            shots_ready: 0,
            seen_max_update_id: 0,
            last_pulse_tick: 0.0,
            last_video_tick: 0.0,
            clipboard_image_rx: None,
        }
    }

    pub fn is_typing(&self) -> bool {
        self.editing.is_some()
            || self.brief_editing.is_some()
            || self.editing_update.is_some()
            || self.replying_to.is_some()
            || self.palette_open
            || self.feed_search.focused
            || self.project_search.focused
            || self.notebook.is_typing()
            || self.curator.is_typing()
            || self.curator_search.focused
    }

    pub fn close_settings(&mut self) {
        self.settings_open = false;
        self.curator.close_select();
    }

    pub fn rescan_clis(&mut self) {
        feeder_core::setup::refresh_search_paths();
        let wake = self.wake.clone();
        self.agents.rescan(wake.as_ref());
        self.curator_sign_in.rescan(wake.as_ref());
        for b in feeder_core::gardener::Backend::ALL {
            self.curator.clear_model_failure(b);
        }
    }

    pub fn tools_connected(&self) -> bool {
        !self.projects.is_empty() || self.agents.has_connected()
    }

    pub fn dismiss_menus(&mut self) {
        self.menu_open = false;
        self.orb_menu = None;
        self.post_menu = None;
        self.notebook.action_menu_open = false;
        self.curator.close_picker();
    }

    pub fn has_active_pulse(&self) -> bool {
        self.projects.iter().any(|p| !p.project.archived && p.active_pulse.is_some())
    }

    pub fn open_project(&mut self, id: i64) {
        self.open_project_at(id, None);
    }

    pub fn open_project_at(&mut self, id: i64, update_id: Option<i64>) {
        self.open = Some(id);
        self.hovered = None;
        self.dismiss_menus();
        self.close_settings();
        self.close_lightbox();
        self.palette_open = false;
        self.delete_confirm = None;
        self.editing = None;
        self.branch_filter = None;
        self.project_search_open = false;
        self.project_search = TextEdit::new("");
        self.brief_editing = None;
        self.editing_update = None;
        self.replying_to = None;
        self.feed_selected = update_id;
        self.pending_scroll = update_id;
        self.files.view = files::ProjectView::Updates;
        self.files.close_preview();
        self.project_visible_rows = None;
        self.media_dirty = true;
        self.limit = PAGE;
        let seeded: Vec<FeedUpdate> = self.timeline.iter().filter(|u| u.project_id == id).cloned().collect();
        if !seeded.is_empty() || self.summary(id).is_some_and(|sum| sum.updates == 0) {
            self.feed = seeded;
            self.feed_for = Some(id);
        } else if self.feed_for != Some(id) {
            self.feed.clear();
            self.feed_for = None;
        }
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == id) {
            let seen = now_ms().max(p.last_at.unwrap_or(0));
            p.project.seen_at = Some(seen);
        }
        if let Some(w) = &self.watcher {
            w.mark_seen(id);
        }
        self.ask();
    }

    pub fn close_project(&mut self) {
        self.open = None;
        self.dismiss_menus();
        self.editing = None;
        self.brief_editing = None;
        self.editing_update = None;
        self.replying_to = None;
        self.branch_filter = None;
        self.project_search_open = false;
        self.project_search = TextEdit::new("");
        self.feed_selected = None;
        self.pending_scroll = None;
        self.project_visible_rows = None;
        self.files.view = files::ProjectView::Updates;
        self.files.close_preview();
        self.media_dirty = true;
        self.close_lightbox();
        self.delete_confirm = None;
        self.ask();
    }

    pub fn card_slide_index(&self, update_id: i64, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        self.card_slide.get(&update_id).copied().unwrap_or(0).min(len - 1)
    }

    pub fn step_card_slide(&mut self, update_id: i64, delta: isize, len: usize) {
        if len <= 1 {
            return;
        }
        let cur = self.card_slide_index(update_id, len);
        let next = (cur as isize + delta).rem_euclid(len as isize) as usize;
        self.set_card_slide(update_id, next);
    }

    pub fn sync_media(&mut self) {
        media::sync(self);
        self.media_dirty = false;
    }

    pub fn update_is_flagged_or_reviewed(&self, update_id: i64) -> bool {
        self.open_flag_for_update(update_id)
            || matches!(self.review_statuses.get(&update_id), Some(FeedReviewStatus::Queued | FeedReviewStatus::Reviewed { .. }))
    }

    fn pause_active_video(&self, except: Option<i64>) {
        if let Some(prev) = self.lightbox
            && except != Some(prev)
            && let Some(player) = self.videos.get(&prev)
        {
            player.pause();
        }
    }

    pub fn lightbox_compare_pair(&self, current: i64, compare: i64) -> Option<(i64, i64)> {
        if current == compare {
            return None;
        }
        let Some((ua, pos_a, _)) = self.find_media(current) else {
            return Some((compare, current));
        };
        let Some((ub, pos_b, _)) = self.find_media(compare) else {
            return Some((compare, current));
        };
        let ord = if ua.id != ub.id { ua.created_at.cmp(&ub.created_at).then(ua.id.cmp(&ub.id)) } else { pos_a.cmp(&pos_b) };
        if ord == std::cmp::Ordering::Less { Some((current, compare)) } else { Some((compare, current)) }
    }

    pub fn set_card_slide(&mut self, update_id: i64, idx: usize) {
        self.card_slide.insert(update_id, idx);
        self.sync_media();
    }

    pub fn open_lightbox(&mut self, media_id: i64, tx: &mut Tx) {
        self.pause_active_video(Some(media_id));
        self.lightbox = Some(media_id);
        self.lightbox_zoom.reset();
        self.lightbox_compare = None;
        self.lightbox_split = 0.5;
        self.clear_copied(CopyTarget::LightboxImage);
        self.sync_media();
        tx.focus_when_shown(rune_kit::image_zoom::KEY, false);
    }

    pub fn select_lightbox(&mut self, media_id: i64) {
        self.pause_active_video(Some(media_id));
        self.lightbox = Some(media_id);
        self.lightbox_zoom.reset();
        self.lightbox_compare = None;
        self.clear_copied(CopyTarget::LightboxImage);
        if let Some((u, slide_idx, _)) = self.find_media(media_id) {
            let uid = u.id;
            self.card_slide.insert(uid, slide_idx);
        }
        self.sync_media();
    }

    pub fn close_lightbox(&mut self) {
        self.pause_active_video(None);
        self.lightbox = None;
        self.lightbox_zoom.reset();
        self.lightbox_compare = None;
        self.clear_copied(CopyTarget::LightboxImage);
        self.sync_media();
    }

    pub fn toggle_lightbox_compare(&mut self) {
        if self.lightbox_compare.is_some() {
            self.lightbox_compare = None;
            self.sync_media();
            return;
        }
        let Some(id) = self.lightbox else { return };
        let same_update_other =
            self.find_media(id).and_then(|(_, _, slides)| slides.into_iter().find(|s| s.id != id && s.is_image()).map(|s| s.id));
        let other = same_update_other.or_else(|| {
            self.lightbox_ids().into_iter().find(|&x| {
                x != id && self.find_media(x).and_then(|(_, i, slides)| slides.get(i).map(MediaSlide::is_image)).unwrap_or(false)
            })
        });
        if let Some(other) = other {
            self.lightbox_compare = Some(other);
            self.lightbox_split = 0.5;
            self.sync_media();
        }
    }

    pub fn toggle_video(&mut self, media_id: i64) {
        if let Some(player) = self.videos.get(&media_id) {
            if player.is_playing() {
                player.pause();
            } else {
                player.play();
            }
        }
    }

    pub fn start_brief_edit(&mut self, id: i64, tx: &mut Tx) {
        let current = self.summary(id).and_then(|s| s.project.brief.clone()).unwrap_or_default();
        let mut edit = TextEdit::new(&current);
        edit.focused = true;
        self.brief_draft = edit;
        self.brief_editing = Some(id);
        tx.focus_when_shown("brief-input", false);
    }

    pub fn save_brief(&mut self) {
        let Some(id) = self.brief_editing.take() else { return };
        let trimmed = self.brief_draft.text.trim().to_string();
        let brief = (!trimmed.is_empty()).then_some(trimmed);
        if let Some(sum) = self.projects.iter_mut().find(|p| p.project.id == id) {
            sum.project.brief = brief.clone();
        }
        if let Some(w) = &self.watcher {
            w.set_brief(id, brief);
        }
    }

    pub fn start_edit_update(&mut self, uid: i64, tx: &mut Tx) {
        let Some(u) = self.find_update(uid) else { return };
        let mut edit = TextEdit::new(&u.body);
        edit.focused = true;
        self.edit_update_body = edit;
        self.editing_update = Some(uid);
        self.replying_to = None;
        tx.focus_when_shown(format!("edit-body-{uid}"), false);
    }

    pub fn save_update_edit(&mut self) {
        let Some(uid) = self.editing_update else { return };
        let body = self.edit_update_body.text.trim().to_string();
        if body.is_empty() {
            return;
        }
        self.editing_update = None;
        if let Some(u) = self.feed.iter_mut().find(|u| u.id == uid) {
            u.body = body.clone();
        }
        if let Some(u) = self.timeline.iter_mut().find(|u| u.id == uid) {
            u.body = body.clone();
        }
        if let Some(w) = &self.watcher {
            w.edit_update(uid, body);
        }
    }

    pub fn start_reply(&mut self, uid: i64, tx: &mut Tx) {
        let mut edit = TextEdit::new("");
        edit.focused = true;
        self.reply_draft = edit;
        self.replying_to = Some(uid);
        self.editing_update = None;
        tx.focus_when_shown(format!("reply-input-{uid}"), false);
    }

    pub fn submit_reply(&mut self, uid: i64) {
        let text = self.reply_draft.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.replying_to = None;
        self.reply_draft = TextEdit::new("");
        let project_id = self.find_update(uid).map(|u| u.project_id).unwrap_or(0);
        let entry = self.replies.entry(uid).or_default();
        let temp_id = entry.iter().map(|r| r.id).min().unwrap_or(0).min(0) - 1;
        entry.push(Reply {
            id: temp_id,
            update_id: uid,
            project_id,
            author: "You".into(),
            from_human: true,
            body: text.clone(),
            created_at: now_ms(),
            read_at: None,
        });
        if let Some(sum) = self.projects.iter_mut().find(|p| p.project.id == project_id && p.last_update_id == Some(uid)) {
            sum.needs_you = false;
        }
        if let Some(w) = &self.watcher {
            w.add_reply(uid, text);
        }
        self.reconcile_home_filter();
    }

    pub fn dismiss_attention(&mut self, uid: i64) {
        let now = now_ms();
        for u in self.feed.iter_mut().chain(&mut self.timeline).chain(&mut self.search_hits) {
            if u.id == uid {
                u.attention_dismissed_at = Some(now);
            }
        }
        for sum in &mut self.projects {
            if sum.last_update_id == Some(uid) {
                sum.needs_you = false;
            }
        }
        if let Some(w) = &self.watcher {
            w.dismiss_attention(uid);
        }
        self.reconcile_home_filter();
        self.push_toast(
            format!("dismiss-{uid}"),
            ToastItem { title: "Dismissed Needs you".into(), description: None, kind: ToastKind::UndoDismiss { update_id: uid } },
            Some(5.0),
        );
    }

    pub fn undismiss_attention(&mut self, uid: i64) {
        let mut has_attention = false;
        for u in self.feed.iter_mut().chain(&mut self.timeline).chain(&mut self.search_hits) {
            if u.id == uid {
                u.attention_dismissed_at = None;
                if parse_attention(&u.body).is_some() {
                    has_attention = true;
                }
            }
        }
        let answered = self.has_human_reply(uid);
        if !answered {
            for sum in &mut self.projects {
                if sum.last_update_id == Some(uid) && (has_attention || sum.last_body.as_deref().and_then(parse_attention).is_some()) {
                    sum.needs_you = true;
                }
            }
        }
        if let Some(w) = &self.watcher {
            w.undismiss_attention(uid);
        }
    }

    pub fn has_human_reply(&self, uid: i64) -> bool {
        self.replies.get(&uid).is_some_and(|rs| rs.iter().any(|r| r.from_human))
    }

    pub fn is_open_attention(&self, u: &FeedUpdate) -> bool {
        u.attention_dismissed_at.is_none()
            && parse_attention(&u.body).is_some()
            && !self.has_human_reply(u.id)
            && self.summary(u.project_id).is_some_and(|sum| sum.needs_you && sum.last_update_id == Some(u.id))
    }

    pub fn toggle_branch_filter(&mut self, branch: &str) {
        if self.branch_filter.as_deref() == Some(branch) {
            self.branch_filter = None;
        } else {
            self.branch_filter = Some(branch.to_string());
        }
        self.media_dirty = true;
    }

    pub fn set_archived(&mut self, id: i64, archived: bool) {
        self.set_archived_inner(id, archived, true);
    }

    pub fn set_archived_inner(&mut self, id: i64, archived: bool, notify: bool) {
        self.orb_menu = None;
        let name = self.summary(id).map(|s| s.project.name.clone()).unwrap_or_else(|| "Project".into());
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == id) {
            p.project.archived = archived;
        }
        if !archived && !self.projects.iter().any(|p| p.project.archived) {
            self.show_archived = false;
        }
        if let Some(w) = &self.watcher {
            w.set_archived(id, archived);
        }
        if archived && self.open == Some(id) {
            self.close_project();
        }
        self.reconcile_home_filter();
        if notify {
            let title = if archived { format!("Archived {name}") } else { format!("Restored {name}") };
            self.push_toast(
                format!("archive-{id}"),
                ToastItem { title, description: None, kind: ToastKind::UndoArchive { project_id: id, was_archived: !archived } },
                Some(5.0),
            );
        }
    }

    pub fn set_pinned(&mut self, id: i64, pinned: bool) {
        self.orb_menu = None;
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == id) {
            p.project.pinned = pinned;
        }
        self.projects.sort_by(|a, b| {
            b.project
                .pinned
                .cmp(&a.project.pinned)
                .then_with(|| b.last_at.unwrap_or(b.project.created_at).cmp(&a.last_at.unwrap_or(a.project.created_at)))
        });
        if let Some(w) = &self.watcher {
            w.set_pinned(id, pinned);
        }
    }

    pub fn mark_seen(&mut self, id: i64) {
        self.orb_menu = None;
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == id) {
            let seen = now_ms().max(p.last_at.unwrap_or(0));
            p.project.seen_at = Some(seen);
        }
        if let Some(w) = &self.watcher {
            w.mark_seen(id);
        }
    }

    pub fn mark_all_seen(&mut self) {
        let now = now_ms();
        for p in &mut self.projects {
            if !p.project.archived {
                p.project.seen_at = Some(now.max(p.last_at.unwrap_or(0)));
            }
        }
        if let Some(w) = &self.watcher {
            w.mark_all_seen();
        }
    }

    pub fn delete_update(&mut self, id: i64) {
        self.delete_confirm = None;
        let media_ids: Vec<i64> = self.find_update(id).map(|u| media_slides(u).into_iter().map(|m| m.id).collect()).unwrap_or_default();
        if self.lightbox.is_some_and(|lid| lid == id || media_ids.contains(&lid)) {
            self.close_lightbox();
        }
        self.feed.retain(|u| u.id != id);
        self.timeline.retain(|u| u.id != id);
        self.search_hits.retain(|u| u.id != id);
        self.card_slide.remove(&id);
        for mid in media_ids {
            self.shots.remove(&mid);
            self.videos.remove(&mid);
        }
        if let Some(w) = &self.watcher {
            w.delete_update(id);
        }
    }

    pub fn open_palette(&mut self, tx: &mut Tx) {
        self.dismiss_menus();
        self.close_lightbox();
        self.close_settings();
        self.delete_confirm = None;
        self.palette_open = true;
        self.palette_index = 0;
        let mut edit = TextEdit::new("");
        edit.focused = true;
        self.palette_query = edit;
        tx.focus_when_shown("palette-input", false);
        self.sync_search();
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_query.focused = false;
        self.sync_search();
    }

    pub fn show_more(&mut self) {
        self.limit += PAGE;
        self.ask();
    }

    pub fn start_rename(&mut self, tx: &mut Tx) {
        let Some(p) = self.open.and_then(|id| self.summary(id)) else { return };
        let mut edit = TextEdit::new(&p.project.name);
        edit.focused = true;
        edit.set_selection(0, edit.text.len());
        self.editing = Some(edit);
        tx.focus_when_shown("rename", false);
    }

    pub fn commit_rename(&mut self) {
        let (Some(id), Some(edit)) = (self.open, self.editing.take()) else { return };
        let typed = edit.text.trim().to_string();
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == id) {
            let name = if typed.is_empty() { p.project.folder().unwrap_or_else(|| p.project.slug.clone()) } else { typed.clone() };
            if name == p.project.name {
                return;
            }
            p.project.name = name;
        }
        if let Some(w) = &self.watcher {
            w.rename(id, typed);
        }
    }

    pub fn cancel_rename(&mut self) {
        self.editing = None;
    }

    pub fn save_settings(&self) {
        if let Some(w) = &self.watcher {
            w.save_settings(self.settings);
        }
    }

    pub fn start_onboarding(&mut self) {
        self.settings.onboarding_skipped = false;
        self.onboarding_step = Some(OnboardingStep::Connect);
        self.save_settings();
    }

    pub fn finish_onboarding(&mut self) {
        self.settings.onboarding_skipped = true;
        self.onboarding_step = None;
        self.save_settings();
    }

    pub fn push_toast(&mut self, key: impl Into<String>, item: ToastItem, secs: Option<f64>) {
        self.toasts.step(ToastEvent::Shown { key: key.into(), options: item, secs, now: self.clock });
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.step(ToastEvent::Dismissed { id: Some(id), now: self.clock });
    }

    pub fn mark_copied(&mut self, target: CopyTarget) {
        self.copy_feedback = Some((target, now_ms()));
    }

    pub fn is_copied(&self, target: CopyTarget) -> bool {
        self.copy_feedback.is_some_and(|(t, at)| t == target && now_ms() - at < 2_000)
    }

    pub fn clear_copied(&mut self, target: CopyTarget) {
        if self.copy_feedback.is_some_and(|(t, _)| t == target) {
            self.copy_feedback = None;
        }
    }

    pub fn unseen_count(&self) -> usize {
        self.projects.iter().filter(|p| p.unseen()).count()
    }

    pub fn select_tab(&mut self, tab: Tab, tx: &mut Tx) {
        if self.select_tab_state(tab) {
            tx.instant();
        }
    }

    fn select_tab_state(&mut self, tab: Tab) -> bool {
        let leaving_notebook_subview = tab == Tab::Notebook
            && (self.notebook.open.is_some() || self.notebook.editor.is_some() || self.notebook.shelf != notebook::Shelf::All);
        let instant = self.tab != tab || self.open.is_some() || leaving_notebook_subview;
        if self.open.is_some() {
            self.close_project();
        } else {
            self.dismiss_menus();
            self.editing_update = None;
            self.replying_to = None;
            self.delete_confirm = None;
            self.close_lightbox();
        }
        self.tab = tab;
        self.media_dirty = true;
        match tab {
            Tab::Home => {
                self.home_filter = HomeFilter::All;
                self.home_selected = None;
            }
            Tab::Feed => {
                self.branch_filter = None;
                self.timeline_selected = None;
            }
            Tab::Notebook => {
                self.notebook_shelf(notebook::Shelf::All);
            }
            Tab::Curator => {
                self.curator.close_picker();
            }
        }
        self.sync_search();
        instant
    }

    pub fn sync_search(&mut self) {
        let active = if self.palette_open {
            self.palette_query.text.trim()
        } else if self.tab == Tab::Feed && self.open.is_none() {
            self.feed_search.text.trim()
        } else {
            ""
        };
        if active != self.search_query {
            self.search_query = active.to_string();
            if self.search_query.is_empty() {
                self.search_snapshot_query.clear();
                self.search_hits.clear();
                self.media_dirty = true;
            }
            if let Some(w) = &self.watcher {
                w.search(self.search_query.clone());
            }
        }
    }

    pub fn open_feeder_url(&mut self, url: &str) -> bool {
        let Some(rest) = url.strip_prefix("feeder://") else {
            return false;
        };
        let rest = rest.split(['?', '#']).next().unwrap_or("").trim_matches('/');
        if rest == "home" {
            self.close_project();
            self.close_settings();
            self.tab = Tab::Home;
            return true;
        }
        if rest == "feed" {
            self.close_project();
            self.close_settings();
            self.tab = Tab::Feed;
            return true;
        }
        if rest == "settings" {
            self.dismiss_menus();
            self.palette_open = false;
            self.close_lightbox();
            self.settings_open = true;
            return true;
        }
        if rest == "curator" || rest == "gardener" {
            self.close_project();
            self.close_settings();
            self.tab = Tab::Curator;
            return true;
        }
        if let Some(page) = rest.strip_prefix("notebook").or_else(|| rest.strip_prefix("library")) {
            self.close_project();
            self.close_settings();
            self.tab = Tab::Notebook;
            if let Ok(id) = page.trim_start_matches('/').parse() {
                self.notebook_open_page(id);
            } else {
                self.notebook_shelf(notebook::Shelf::All);
            }
            return true;
        }
        if let Some(after) = rest.strip_prefix("project/") {
            let mut segs = after.split('/').filter(|s| !s.is_empty());
            let slug = segs.next().unwrap_or("");
            let update_id = match (segs.next(), segs.next()) {
                (Some("update"), Some(id)) => id.parse::<i64>().ok(),
                _ => None,
            };
            if !self.loaded {
                self.pending_url = Some(url.to_string());
                return true;
            }
            if let Some(sum) =
                self.projects.iter().find(|p| p.project.slug.eq_ignore_ascii_case(slug) || p.project.name.eq_ignore_ascii_case(slug))
            {
                let id = sum.project.id;
                self.open_project_at(id, update_id);
            } else {
                self.push_toast(
                    "missing-project-url",
                    ToastItem { title: format!("Project \"{slug}\" not found"), description: None, kind: ToastKind::Error },
                    Some(4.0),
                );
            }
            return true;
        }
        true
    }

    fn timeline_source(&self) -> (bool, &[FeedUpdate]) {
        let query = self.feed_search.text.trim();
        let using_db = !query.is_empty() && self.search_snapshot_query == query;
        (using_db, if using_db { &self.search_hits } else { &self.timeline })
    }

    pub fn filtered_timeline(&self) -> Vec<(&FeedUpdate, &ProjectSummary)> {
        let fold = Fold::default();
        let query = self.feed_search.text.trim();
        let (using_db, source) = self.timeline_source();
        source
            .iter()
            .filter_map(|u| {
                let sum = self.summary(u.project_id).filter(|sum| !sum.project.archived)?;
                if let Some(branch) = &self.branch_filter
                    && u.branch.as_deref() != Some(branch.as_str())
                {
                    return None;
                }
                if !using_db
                    && !query.is_empty()
                    && !fold.contains(&u.body, query)
                    && !fold.contains(&sum.project.name, query)
                    && !fold.contains(&u.agent, query)
                    && !u.branch.as_deref().is_some_and(|b| fold.contains(b, query))
                {
                    return None;
                }
                Some((u, sum))
            })
            .collect()
    }

    pub fn find_update(&self, id: i64) -> Option<&FeedUpdate> {
        self.feed.iter().chain(&self.timeline).chain(&self.search_hits).find(|u| u.id == id)
    }

    pub fn find_media(&self, media_id: i64) -> Option<(&FeedUpdate, usize, Vec<MediaSlide>)> {
        for u in self.feed.iter().chain(&self.timeline).chain(&self.search_hits) {
            let slides = media_slides(u);
            if let Some(pos) = slides.iter().position(|m| m.id == media_id) {
                return Some((u, pos, slides));
            }
        }
        None
    }

    pub fn lightbox_ids(&self) -> Vec<i64> {
        let collect_update = |u: &FeedUpdate| media_slides(u).into_iter().map(|m| m.id).collect::<Vec<_>>();
        if let Some(open) = self.open {
            if self.feed_for == Some(open) { feed::visible_feed(self).into_iter().flat_map(collect_update).collect() } else { vec![] }
        } else {
            self.filtered_timeline().into_iter().flat_map(|(u, _)| collect_update(u)).collect()
        }
    }

    pub fn step_lightbox(&mut self, delta: isize) {
        let Some(current) = self.lightbox else { return };
        let ids = self.lightbox_ids();
        let Some(pos) = ids.iter().position(|&id| id == current) else { return };
        if ids.len() <= 1 {
            return;
        }
        let next = (pos as isize + delta).rem_euclid(ids.len() as isize) as usize;
        self.select_lightbox(ids[next]);
    }

    pub fn dark(&self, system_dark: bool) -> bool {
        match self.settings.theme {
            Theme::System => system_dark,
            Theme::Light => false,
            Theme::Dark => true,
        }
    }

    pub fn agent_icon(&self, agent: &str) -> Option<&Image> {
        self.agent_icons.get(agent)
    }

    pub fn summary(&self, id: i64) -> Option<&ProjectSummary> {
        self.projects.iter().find(|p| p.project.id == id)
    }

    fn reconcile_home_filter(&mut self) {
        if self.loaded && self.home_filter != HomeFilter::All && home::ordered_active(self).is_empty() {
            self.home_filter = HomeFilter::All;
        }
    }

    fn ask(&self) {
        if let Some(w) = &self.watcher {
            let files = self.window_visible && self.open.is_some() && self.files.view == files::ProjectView::Files;
            w.want(Want {
                project: self.open,
                limit: self.limit,
                through_update: self.pending_scroll,
                files,
                visible: self.window_visible,
            });
        }
    }

    fn pulse_halo_visible(&self) -> bool {
        if !self.window_visible {
            return false;
        }
        match self.open {
            Some(id) if self.files.view == files::ProjectView::Updates => {
                self.summary(id).is_some_and(|sum| !sum.project.archived && sum.active_pulse.is_some())
            }
            Some(_) => false,
            None => self.tab == Tab::Home && self.has_active_pulse(),
        }
    }
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    if s.icon {
        return icon::view();
    }
    let inset = cx.titlebar_inset();
    let reduced_motion = cx.is_reduced_motion();
    REDUCED_MOTION.store(reduced_motion, Ordering::Relaxed);
    let p = style::palette(s.dark(cx.system_dark()));
    cx.foreground(p.text);
    let k = style::kit(&p);
    let mut layer = Vec::new();
    let shell = if s.open.is_none() && s.tab == Tab::Notebook {
        notebook::view(s, &p, &k, inset, reduced_motion, &mut layer)
    } else if s.open.is_none() && s.tab == Tab::Curator {
        gardener::shell_view(s, &p, &k, inset, reduced_motion, &mut layer)
    } else {
        let (leading, trailing) = if let Some(id) = s.open { feed::bar_chrome(s, id, &p, &k) } else { (el(), Some(chrome::tabs(s, &k))) };
        let header = chrome::bar(s, &p, &k, inset, leading, trailing);
        let page = match s.open {
            Some(id) if s.files.view == files::ProjectView::Files => {
                let [w, h] = cx.layout_size("shell").unwrap_or([1180.0, 760.0]);
                files::view(s, id, &p, &k, [w, h - TOOLBAR_HEIGHT])
            }
            Some(id) => feed::view(s, id, &p, &k, reduced_motion),
            None if s.tab == Tab::Feed => timeline::view(s, &p, &k, &mut layer),
            None => home::view(s, &p, &k, reduced_motion, &mut layer),
        };
        col().key("shell").w_full().h_full().child(header).child(page)
    };
    #[cfg(feeder_sparkle)]
    updater::view(s, &p, &k, &mut layer);
    layer.extend(chrome::palette(s, &p, &k));
    layer.extend(chrome::lightbox(s, inset, &p, &k, reduced_motion));
    layer.extend(chrome::delete_confirm(s, &p, &k));
    chrome::settings(s, &p, &k, &mut layer);
    layer.extend(chrome::toasts(s, &p, &k));
    let base = el()
        .key("root")
        .w_full()
        .h_full()
        .fill(p.bg)
        .on_layout_tx(|s: &mut State, r, tx| {
            if r[2] > 0.0 && r[3] > 0.0 && ((r[2] - s.window_size[0]).abs() > 0.5 || (r[3] - s.window_size[1]).abs() > 0.5) {
                s.window_size = [r[2], r[3]];
            } else {
                tx.live_only();
            }
        })
        .child(shell)
        .child(chrome::settings_corner(s, &p, &k, &mut layer));
    rune_kit::root(base, layer)
}

fn setup(s: &mut State, wake: Waker) {
    #[cfg(feeder_sparkle)]
    updater::bind(wake.clone());
    if rune::tooling::headless_run().is_none() {
        let _ = std::thread::Builder::new().name("feeder-skills-sync".into()).spawn(|| {
            feeder_core::setup::sync_installed_skills();
        });
    }
    s.wake = Some(wake.clone());
    s.images = Some(ImageCache::new(wake.clone()));
    s.watcher = Some(Watcher::start(wake));
    s.ask();
}

fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    s.clock = clock;
    changed |= s.files.poll(tx, s.images.as_ref());
    #[cfg(feeder_sparkle)]
    {
        changed |= updater::poll(s);
    }
    s.sync_search();
    changed |= s.notebook.chat.drain();
    let onboarding_curator = s.loaded && s.open.is_none() && s.tab == Tab::Home && s.onboarding_step == Some(OnboardingStep::Curator);
    s.curator_sign_in.follow_accounts(
        ((s.settings_open && s.settings_page == chrome::SettingsPage::Curator) || onboarding_curator)
            && s.notebook.preview_backends.is_none(),
        s.wake.as_ref(),
    );
    changed |= s.curator_sign_in.poll();
    if let Some(installed) = s.curator_sign_in.take_installed() {
        if s.notebook.preview_backends.is_none() {
            s.notebook.gardener.backends = installed.clone();
        }
        if let Some(w) = &s.watcher {
            w.set_backends(installed);
        }
        changed = true;
    }
    if let Some(backend) = s.curator_sign_in.take_connected() {
        s.curator.clear_model_failure(backend);
        changed = true;
    }
    changed |= s.curator.poll();
    if let Some(rx) = &s.clipboard_image_rx {
        match rx.try_recv() {
            Ok(Ok(img)) => {
                tx.copy_rich(ClipboardContent::new("").with_image(img));
                s.mark_copied(CopyTarget::LightboxImage);
                s.push_toast(
                    "copy-image",
                    ToastItem { title: "Copied screenshot to clipboard".into(), description: None, kind: ToastKind::Info },
                    Some(2.5),
                );
                s.clipboard_image_rx = None;
                changed = true;
            }
            Ok(Err(err)) => {
                s.push_toast(
                    "copy-image-err",
                    ToastItem { title: "Couldn't copy image".into(), description: Some(err), kind: ToastKind::Error },
                    Some(4.0),
                );
                s.clipboard_image_rx = None;
                changed = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                s.clipboard_image_rx = None;
            }
        }
    }
    if s.settings_open && s.settings_page == chrome::SettingsPage::Curator {
        let wake = s.wake.clone();
        for b in s.curator.prefs.enabled() {
            if s.notebook.gardener.backends.contains(&b) {
                s.curator.want_models(b, wake.as_ref());
            }
        }
    } else if s.open.is_none()
        && s.tab == Tab::Curator
        && let Some(b) = curator_settings::selected(s)
        && s.notebook.gardener.backends.contains(&b)
    {
        let wake = s.wake.clone();
        s.curator.want_models(b, wake.as_ref());
    }
    let need_tools = s.settings_open || (s.loaded && s.projects.is_empty());
    s.agents.follow_scan(need_tools, s.wake.as_ref());
    changed |= s.agents.take();
    if let Some(cache) = &s.images {
        for (name, app) in s.agents.icons.drain(..) {
            s.agent_icons.entry(name).or_insert_with(|| {
                let img = cache.app_icon(&app, 20.0);
                if rune::tooling::headless_run().is_some() {
                    let _ = img.wait();
                }
                img
            });
        }
    }

    if s.lightbox.is_some() && s.lightbox_zoom.gliding() && s.lightbox_zoom.poll(clock, tx) {
        changed = true;
    }

    if rune::tooling::headless_run().is_none()
        && let Some(id) = s.lightbox
        && s.videos.get(&id).is_some_and(Player::is_playing)
    {
        if clock - s.last_video_tick >= 0.1 {
            s.last_video_tick = clock;
            changed = true;
        }
        tx.wake_after(Duration::from_millis(100));
    }

    if let Some(due) = s.toasts.next_due() {
        if clock + 1e-3 >= due {
            s.toasts.step(ToastEvent::Tick { now: clock });
            changed = true;
        }
        if let Some(next_due) = s.toasts.next_due() {
            tx.wake_after(Duration::from_secs_f64((next_due - clock).max(0.01)));
        }
    }

    if let Some(snap) = s.watcher.as_ref().and_then(Watcher::take) {
        let was_loaded = s.loaded;
        if !s.loaded {
            tx.instant();
        }
        let max_id = snap.timeline.iter().map(|u| u.id).max().unwrap_or(0);
        if was_loaded && max_id > s.seen_max_update_id {
            if s.settings.notify_updates
                && rune::tooling::headless_run().is_none()
                && let Some(newest) = snap.timeline.iter().filter(|u| u.id > s.seen_max_update_id).max_by_key(|u| u.id)
            {
                let viewing_project = s.window_visible && s.open == Some(newest.project_id) && s.files.view == files::ProjectView::Updates;
                let viewing_feed = s.window_visible && s.open.is_none() && s.tab == Tab::Feed;
                #[cfg(target_os = "macos")]
                let active_here = notify::app_is_active() && (viewing_project || viewing_feed);
                #[cfg(not(target_os = "macos"))]
                let active_here = false;
                if !active_here {
                    let project = snap.projects.iter().find(|p| p.project.id == newest.project_id).map(|p| &p.project);
                    let name = project.map_or("Feeder", |p| p.name.as_str());
                    let link = project
                        .map_or_else(|| "feeder://feed".to_string(), |p| format!("feeder://project/{}/update/{}", p.slug, newest.id));
                    #[cfg(target_os = "macos")]
                    notify::post(&format!("{name} · {}", newest.agent), &newest.body, &link);
                }
            }
            s.seen_max_update_id = max_id;
        } else if !was_loaded {
            s.seen_max_update_id = max_id;
        }
        if snap.error.is_none() && snap.projects.is_empty() && !s.settings.onboarding_skipped && s.onboarding_step.is_none() {
            s.onboarding_step = Some(OnboardingStep::Connect);
        }
        s.projects = snap.projects;
        s.timeline = snap.timeline;
        s.timeline_limit = snap.timeline_limit;
        if let Some(pages) = snap.pages {
            s.notebook.set_pages(pages);
            if s.notebook.open.is_some_and(|id| s.notebook.page(id).is_none()) {
                s.notebook.open = None;
            }
        }
        if let Some(id) = snap.created_page
            && s.notebook.pending_open_created
        {
            s.notebook.pending_open_created = false;
            if s.tab == Tab::Notebook && s.notebook.editor.is_none() {
                s.notebook_open_page(id);
            }
        }
        let mutation_err = snap.mutation_error;
        if let Some(mut g) = snap.gardener {
            if let Some(chats) = &s.notebook.preview_chats {
                g.chats.clone_from(chats);
            }
            if let Some(changes) = &s.notebook.preview_changes {
                g.changes.clone_from(changes);
            }
            if let Some(backends) = &s.notebook.preview_backends {
                g.backends.clone_from(backends);
            }
            s.notebook
                .chat
                .in_flight_proposals
                .retain(|pid| g.proposals.iter().find(|p| p.id == *pid).is_some_and(|p| p.status == feeder_core::ProposalStatus::Pending));
            s.notebook.chat.in_flight_changes.retain(|cid| g.changes.iter().find(|c| c.id == *cid).is_some_and(|c| !c.is_reverted()));
            s.notebook.gardener = g;
        }
        s.replies = snap.replies;
        s.review_statuses = snap.review_statuses;
        s.search_snapshot_query = snap.search_query;
        s.search_hits = snap.search_hits;
        s.error = snap.error;
        s.loaded = true;
        s.reconcile_home_filter();
        if let Some(err) = &mutation_err {
            s.notebook.chat.in_flight_proposals.clear();
            s.notebook.chat.in_flight_changes.clear();
            s.push_toast(
                "watcher-error",
                ToastItem { title: "Action failed".into(), description: Some(err.clone()), kind: ToastKind::Error },
                Some(5.0),
            );
        }
        for failed in snap.failed_writes {
            match failed {
                watch::FailedWrite::Reply { update_id, body } => {
                    if s.replying_to.is_none() {
                        let mut edit = TextEdit::new(&body);
                        edit.focused = true;
                        s.reply_draft = edit;
                        s.replying_to = Some(update_id);
                        tx.focus_when_shown(format!("reply-input-{update_id}"), false);
                    }
                }
                watch::FailedWrite::EditUpdate { update_id, body } => {
                    if s.editing_update.is_none() {
                        let mut edit = TextEdit::new(&body);
                        edit.focused = true;
                        s.edit_update_body = edit;
                        s.editing_update = Some(update_id);
                        tx.focus_when_shown(format!("edit-body-{update_id}"), false);
                    }
                }
                watch::FailedWrite::Brief { project_id, body } => {
                    if s.brief_editing.is_none() {
                        let mut edit = TextEdit::new(body.as_deref().unwrap_or(""));
                        edit.focused = true;
                        s.brief_draft = edit;
                        s.brief_editing = Some(project_id);
                        tx.focus_when_shown("brief-input", false);
                    }
                }
                watch::FailedWrite::CreatePage(draft) => {
                    s.notebook.pending_open_created = false;
                    if s.notebook.editor.is_none() {
                        s.notebook.editor = Some(notebook::Editor::from_failed_draft(None, draft, mutation_err.clone()));
                        s.notebook.open = None;
                        s.tab = Tab::Notebook;
                        tx.focus_when_shown("nb-editor-title", false);
                    }
                }
                watch::FailedWrite::RevisePage { id, draft } => {
                    if s.notebook.editor.is_none() {
                        s.notebook.editor = Some(notebook::Editor::from_failed_draft(Some(id), draft, mutation_err.clone()));
                        s.notebook.open = None;
                        s.tab = Tab::Notebook;
                        tx.focus_when_shown("nb-editor-title", false);
                    }
                }
            }
        }
        if let Some(url) = s.pending_url.take() {
            s.open_feeder_url(&url);
        }
        if snap.want.project == s.open && snap.want.project.is_some() {
            s.feed = snap.feed;
            s.feed_for = snap.want.project;
            if s.error.is_none()
                && let Some(target) = s.pending_scroll
                && snap.want.through_update == Some(target)
            {
                s.pending_scroll = None;
                if s.feed.iter().any(|update| update.id == target) {
                    s.limit = s.limit.max(s.feed.len());
                    tx.scroll_into_view(format!("u-{target}"));
                } else {
                    if s.feed_selected == Some(target) {
                        s.feed_selected = None;
                    }
                    if s.replying_to == Some(target) {
                        s.replying_to = None;
                    }
                    s.push_toast(
                        "missing-update-url",
                        ToastItem { title: "Update not found".into(), description: None, kind: ToastKind::Error },
                        Some(4.0),
                    );
                }
                s.ask();
            }
        }
        if let Some(id) = s.open
            && s.loaded
            && s.error.is_none()
            && s.summary(id).is_none()
        {
            s.open = None;
        }
        s.media_dirty = true;
        if let Some(id) = s.lightbox
            && s.find_media(id).is_none()
        {
            s.close_lightbox();
        }
        if let Some(cache) = &s.images {
            for (agent, app) in snap.agent_apps {
                s.agent_icons.entry(agent).or_insert_with(|| {
                    let img = cache.app_icon(&app, 20.0);
                    if rune::tooling::headless_run().is_some() {
                        let _ = img.wait();
                    }
                    img
                });
            }
        }
        changed = true;
    }

    let files_visible = s.open.is_some() && s.files.view == files::ProjectView::Files;
    if let Some((id, wt)) = s.watcher.as_ref().and_then(Watcher::take_files)
        && files_visible
        && s.open == Some(id)
    {
        s.files.apply(id, wt);
        changed = true;
    }

    let unseen = s.unseen_count();
    if s.loaded
        && s.last_badge != Some(unseen)
        && rune::tooling::headless_run().is_none()
        && let Ok(slot) = window_slot().lock()
        && let Some(handle) = slot.as_ref()
    {
        let desc = if unseen == 1 { "1 unseen project".to_string() } else { format!("{unseen} unseen projects") };
        handle.set_taskbar(Taskbar::new().badge(u32::try_from(unseen).unwrap_or(u32::MAX), desc));
        s.last_badge = Some(unseen);
    }

    if s.media_dirty {
        s.sync_media();
    }

    if rune::tooling::headless_run().is_none() && !REDUCED_MOTION.load(Ordering::Relaxed) && s.pulse_halo_visible() {
        if clock - s.last_pulse_tick >= 0.065 {
            s.last_pulse_tick = clock;
            changed = true;
        }
        tx.wake_after(Duration::from_millis(70));
    }

    let ready = s.shots.values().chain(s.agent_icons.values()).filter(|i| i.is_ready()).count();
    if ready != s.shots_ready {
        s.shots_ready = ready;
        changed = true;
    }
    let now = now_ms();
    if let Some((_, copied_at)) = s.copy_feedback {
        let elapsed = now.saturating_sub(copied_at);
        if elapsed >= 2_000 {
            s.copy_feedback = None;
            changed = true;
        } else {
            let remaining = u64::try_from((2_000 - elapsed).max(50)).unwrap_or(2_000);
            tx.wake_after(Duration::from_millis(remaining + 50));
        }
    }
    if now - s.now >= 30_000 {
        s.now = now;
        changed = true;
    }
    tx.wake_after(Duration::from_secs(30));
    changed
}

fn is_app_icon_scene() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--scene=app-icon" {
            return true;
        }
        if arg == "--scene" && args.next().as_deref() == Some("app-icon") {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn system_font_faces() -> &'static [&'static [u8]] {
    let Ok(regular) = std::fs::read("/System/Library/Fonts/SFNS.ttf") else { return &[] };
    let Ok(italic) = std::fs::read("/System/Library/Fonts/SFNSItalic.ttf") else { return &[] };
    let faces: [&'static [u8]; 2] = [regular.leak(), italic.leak()];
    Box::leak(Box::new(faces))
}

fn main() {
    let mut app = rune::App::new(AppSpec::<State> {
        min_size: Some((640.0, 480.0)),
        scenes: &[
            ("app-icon", |s| s.icon = true),
            ("onboarding-connect", |s| {
                s.settings.onboarding_skipped = false;
                s.onboarding_step = Some(OnboardingStep::Connect);
            }),
            ("onboarding-first-update", |s| {
                s.settings.onboarding_skipped = false;
                s.onboarding_step = Some(OnboardingStep::FirstUpdate);
            }),
            ("onboarding-curator", |s| {
                s.settings.onboarding_skipped = false;
                s.onboarding_step = Some(OnboardingStep::Curator);
            }),
            ("onboarding-skipped", |s| s.settings.onboarding_skipped = true),
            ("toast-update", |s| {
                s.settings.onboarding_skipped = true;
                s.push_toast(
                    "app-update",
                    ToastItem {
                        title: "Feeder is up to date".into(),
                        description: Some("Feeder 0.5.12 is the newest version available.".into()),
                        kind: ToastKind::Info,
                    },
                    None,
                );
            }),
            #[cfg(feeder_sparkle)]
            ("update-popover", |s| {
                s.settings.onboarding_skipped = true;
                s.updates.demo_available();
            }),
            ("feed", |s| s.tab = Tab::Feed),
            ("notebook", |s| s.tab = Tab::Notebook),
            ("notebook-page", |s| {
                s.tab = Tab::Notebook;
                s.notebook.auto_open_first = true;
            }),
            ("curator", |s| s.tab = Tab::Curator),
            ("notebook-gardener", |s| s.tab = Tab::Curator),
            ("notebook-gardener-session", |s| {
                s.tab = Tab::Curator;
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::ClaudeCode, feeder_core::gardener::demo_events());
            }),
            ("notebook-gardener-working", |s| {
                s.tab = Tab::Curator;
                let mut events = feeder_core::gardener::demo_events();
                events.truncate(11);
                events.push(feeder_core::gardener::Event::ToolStarted(feeder_core::gardener::ToolCall {
                    id: "t5".into(),
                    name: "search_notebook".into(),
                    input: Default::default(),
                }));
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::ClaudeCode, events);
            }),
            ("notebook-curator-history", |s| {
                use feeder_core::gardener::{ChatSummary, Scope, SessionMode};
                s.tab = Tab::Curator;
                s.notebook.preview_chats = Some(vec![
                    ChatSummary {
                        run_id: 2,
                        started_at: s.now - 40 * 60_000,
                        mode: SessionMode::Ask,
                        scope: Scope::Notebook,
                        title: "How do we ship the Mac app?".into(),
                        edits: 0,
                        truncated: false,
                    },
                    ChatSummary {
                        run_id: 1,
                        started_at: s.now - 26 * 3_600_000,
                        mode: SessionMode::Review,
                        scope: Scope::Notebook,
                        title: "Review Open Flags".into(),
                        edits: 2,
                        truncated: false,
                    },
                ]);
            }),
            ("notebook-curator-revision", |s| {
                use feeder_core::gardener::{Event, ToolCall};
                s.tab = Tab::Curator;
                let events = vec![
                    Event::Started,
                    Event::Text("Flag 2 says the busy timeout and writer rule are missing, so I'm adding them to the SQLite page.".into()),
                    Event::ToolStarted(ToolCall {
                        id: "w".into(),
                        name: "write_page".into(),
                        input: r#"{"id":1,"title":"SQLite returns SQLITE_BUSY under parallel writes"}"#.parse().unwrap_or_default(),
                    }),
                    Event::ToolFinished { id: "w".into(), output: "Revised page #1".into(), failed: false },
                    Event::Text("Updated the SQLite page with the timeout value and the one-writer rule from flag 2.".into()),
                    Event::TurnFinished,
                ];
                s.notebook.preview_changes = Some(vec![feeder_core::Change {
                    id: 1,
                    page_id: 1,
                    page_title: "SQLite returns SQLITE_BUSY under parallel writes".into(),
                    action: feeder_core::ChangeAction::Revised,
                    agent: feeder_core::GARDENER.into(),
                    reason: Some("Flag 2: timeout value and writer rule missing".into()),
                    before: Some(feeder_core::PageSnapshot {
                        kind: "pitfall".into(),
                        topic: "rust".into(),
                        title: "SQLite returns SQLITE_BUSY under parallel writes".into(),
                        tags: vec!["rust".into(), "sqlite".into()],
                        body: "## What happened\n\nTwo worker threads writing the same database failed with 'database is locked' under load.\n\n## Why\n\nEach connection opened in the default rollback journal mode, which locks the whole file for every write.\n\n## Takeaway\n\nOpen every connection in WAL mode and set a busy timeout before the first write.".into(),
                    }),
                    created_at: s.now,
                    reverted_at: None,
                    undoes: None,
                    source_updates: vec![],
                    gardener_run_id: None,
                    source_flags: vec![2],
                }]);
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::ClaudeCode, events);
            }),
            ("curator-proposal", |s| {
                use feeder_core::gardener::{Event, ToolCall};
                s.tab = Tab::Curator;
                let events = vec![
                    Event::Started,
                    Event::ToolStarted(ToolCall {
                        id: "w".into(),
                        name: "write_page".into(),
                        input: r###"{"id":1,"title":"SQLite returns SQLITE_BUSY under parallel writes","reason":"Add the 5,000 ms busy_timeout and single-writer channel rule from flag #2.","body":"## What happened\n\nTwo worker threads writing the same database failed with 'database is locked' under load.\n\n## Why\n\nEach connection opened in the default rollback journal mode, which locks the whole file for every write.\n\n## Takeaway\n\nOpen every connection in WAL mode, set `PRAGMA busy_timeout = 5000`, and route writes through a single background worker channel."}"###.parse().unwrap_or_default(),
                    }),
                    Event::ToolFinished {
                        id: "w".into(),
                        output: r#"{"proposal_id":1,"status":"pending","action":"revised","page_id":1}"#.into(),
                        failed: false,
                    },
                    Event::TurnFinished,
                ];
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::OpenCode, events);
            }),
            ("notebook-curator-diagram", |s| {
                use feeder_core::gardener::Event;
                s.tab = Tab::Curator;
                let answer = "Releases go through one script:\n\n```mermaid\nflowchart LR\n  A[Bump version] --> B[Write release notes]\n  B --> C[scripts/changelog.sh]\n  C --> D[scripts/release.sh]\n  D --> E[Sign + notarize]\n  E --> F[Publish appcast]\n```\n\nThe signing key for the appcast isn't in the Library yet.";
                let events = vec![Event::Started, Event::Text(answer.into()), Event::TurnFinished];
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::ClaudeCode, events);
                s.notebook.chat.items.insert(0, gardener::Item::You("How do we ship the Mac app?".into()));
            }),
            ("notebook-curator-streaming", |s| {
                use feeder_core::gardener::Event;
                s.tab = Tab::Curator;
                let partial = "Open every connection in **WAL mode** and set a busy timeout. See [SQLite returns SQLITE_BUSY](feeder://noteb";
                let events = vec![Event::Started, Event::Text(partial.into())];
                gardener::replay(&mut s.notebook.chat, feeder_core::gardener::Backend::ClaudeCode, events);
                s.notebook.chat.items.insert(0, gardener::Item::You("Why do our parallel writes fail?".into()));
            }),
            ("notebook-curator-signin", |s| {
                use feeder_core::gardener::{Backend, Event};
                s.tab = Tab::Curator;
                gardener::replay(&mut s.notebook.chat, Backend::Codex, vec![Event::Started, Event::NeedsSignIn, Event::Ended]);
                s.notebook.chat.items.insert(0, gardener::Item::You("How do we sign releases?".into()));
            }),
            ("notebook-curator-saved", |s| {
                s.tab = Tab::Curator;
                let mut events = feeder_core::gardener::demo_events();
                events.retain(|e| !matches!(e, feeder_core::gardener::Event::Ended));
                {
                    use feeder_core::gardener::{ChatSummary, Scope, SessionMode};
                    s.notebook.preview_chats = Some(vec![
                        ChatSummary {
                            run_id: 2,
                            started_at: s.now - 40 * 60_000,
                            mode: SessionMode::Ask,
                            scope: Scope::Notebook,
                            title: "How do we ship the Mac app?".into(),
                            edits: 0,
                            truncated: false,
                        },
                        ChatSummary {
                            run_id: 1,
                            started_at: s.now - 26 * 3_600_000,
                            mode: SessionMode::Review,
                            scope: Scope::Notebook,
                            title: "What changed in the release notes?".into(),
                            edits: 1,
                            truncated: false,
                        },
                        ChatSummary {
                            run_id: 3,
                            started_at: s.now - 4 * 86_400_000,
                            mode: SessionMode::Ask,
                            scope: Scope::Notebook,
                            title: "Why does SQLite return BUSY under load?".into(),
                            edits: 0,
                            truncated: false,
                        },
                    ]);
                }
                gardener::preview_saved(
                    &mut s.notebook.chat,
                    feeder_core::gardener::Backend::ClaudeCode,
                    1,
                    "What changed in the release notes?",
                    events,
                );
            }),
            ("notebook-no-match", |s| {
                s.tab = Tab::Notebook;
                s.notebook.query = TextEdit::new("appcast signing key");
            }),
            ("notebook-page-linked", |s| {
                s.tab = Tab::Notebook;
                s.notebook.open = Some(1);
            }),
            ("notebook-new", |s| {
                s.tab = Tab::Notebook;
                s.notebook_new_page();
            }),
            ("settings", |s| s.settings_open = true),
            ("settings-tools", |s| {
                s.settings_page = chrome::SettingsPage::Agents;
                s.settings_open = true;
            }),
            ("settings-curator", |s| {
                let all = feeder_core::gardener::Backend::ALL.to_vec();
                s.notebook.preview_backends = Some(all.clone());
                s.notebook.gardener.backends = all;
                s.curator_sign_in.demo();
                s.curator.demo();
                s.settings_page = chrome::SettingsPage::Curator;
                s.settings_open = true;
            }),
            ("settings-curator-partial", |s| {
                use feeder_core::gardener::Backend;
                let partial = vec![Backend::Codex, Backend::Antigravity];
                s.notebook.preview_backends = Some(partial.clone());
                s.notebook.gardener.backends = partial;
                s.curator_sign_in.demo();
                s.curator.prefs = feeder_core::gardener::CuratorPrefs::default();
                s.settings_page = chrome::SettingsPage::Curator;
                s.settings_open = true;
            }),
            ("curator-none", |s| {
                s.notebook.preview_backends = Some(vec![]);
                s.notebook.gardener.backends = vec![];
                s.curator.prefs = feeder_core::gardener::CuratorPrefs::default();
                s.tab = Tab::Curator;
            }),
            ("curator-off", |s| {
                let all = feeder_core::gardener::Backend::ALL.to_vec();
                s.notebook.preview_backends = Some(all.clone());
                s.notebook.gardener.backends = all;
                s.curator.prefs = feeder_core::gardener::CuratorPrefs::default();
                s.tab = Tab::Curator;
            }),
            ("curator-picker", |s| {
                let all = feeder_core::gardener::Backend::ALL.to_vec();
                s.notebook.preview_backends = Some(all.clone());
                s.notebook.gardener.backends = all;
                s.curator_sign_in.demo();
                s.curator.demo();
                s.tab = Tab::Curator;
                s.curator.picker_open = true;
            }),
            ("curator-picker-opencode", |s| {
                use feeder_core::gardener::Backend;
                let all = Backend::ALL.to_vec();
                s.notebook.preview_backends = Some(all.clone());
                s.notebook.gardener.backends = all;
                s.curator_sign_in.demo();
                s.curator.demo_opencode();
                s.notebook.curator_backend = Some(Backend::OpenCode);
                s.tab = Tab::Curator;
                s.curator.picker_open = true;
            }),
        ],
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(keys::on_key),
        families: style::FONT_FAMILIES,
        #[cfg(target_os = "macos")]
        fonts: system_font_faces(),
        ..AppSpec::new("Feeder", (1180.0, 760.0), State::new, view)
    })
    .titlebar(rune::Titlebar::new(TOOLBAR_HEIGHT).traffic_lights(18.0, (TOOLBAR_HEIGHT - 14.0) / 2.0))
    .on_visibility(|s, visible, _| {
        s.window_visible = visible;
        s.ask();
    })
    .keep_running()
    .standard_menus()
    .on_window(|handle| {
        #[cfg(target_os = "macos")]
        notify::start();
        #[cfg(feeder_sparkle)]
        updater::start();
        if let Ok(mut slot) = window_slot().lock() {
            *slot = Some(handle.clone());
        }
    })
    .on_reopen(|_s, _visible, _tx| false)
    .on_open_url(|s, url, _tx| {
        if s.open_feeder_url(url)
            && let Ok(slot) = window_slot().lock()
            && let Some(handle) = slot.as_ref()
        {
            handle.open();
            handle.focus();
        }
    });
    if is_app_icon_scene() {
        app = app.window_options(rune::WindowOptions { transparent: true, ..Default::default() });
    }
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use feeder_core::{Orb, Project};

    fn sample_summary(id: i64, slug: &str, name: &str) -> ProjectSummary {
        ProjectSummary {
            project: Project {
                id,
                slug: slug.into(),
                name: name.into(),
                path: None,
                orb: Orb::generate(&[]),
                created_at: 1_000,
                archived: false,
                seen_at: None,
                pinned: false,
                brief: None,
            },
            updates: 1,
            last_update_id: Some(99),
            last_at: Some(2_000),
            last_agent: Some("Codex".into()),
            last_via: None,
            last_body: Some("Changed: hello".into()),
            needs_you: false,
            unread_replies: 0,
            open_todos: 0,
            active_pulse: None,
        }
    }

    fn sample_update(id: i64, project_id: i64, body: &str) -> FeedUpdate {
        FeedUpdate {
            id,
            project_id,
            agent: "Claude Code".into(),
            body: body.into(),
            screenshot: None,
            media: Vec::new(),
            media_w: None,
            media_h: None,
            branch: Some("feat/search".into()),
            diff_stat: Some("1 file changed, +4 -1".into()),
            commit_sha: Some("abc1234".into()),
            created_at: 1_500,
            attention_dismissed_at: None,
            via: None,
        }
    }

    #[test]
    fn feeder_urls_route_cleanly() {
        let mut s = State::new();
        s.loaded = true;
        s.projects.push(sample_summary(7, "alpha", "Alpha"));

        assert!(!s.open_feeder_url("https://example.com"));
        assert!(s.open_feeder_url("feeder://feed"));
        assert_eq!(s.tab, Tab::Feed);
        assert_eq!(s.open, None);

        assert!(s.open_feeder_url("feeder://settings"));
        assert!(s.settings_open);

        assert!(s.open_feeder_url("feeder://curator"));
        assert_eq!(s.tab, Tab::Curator);
        assert_eq!(s.open, None);
        assert_eq!(Tab::PRIMARY.iter().position(|t| *t == s.tab), None);

        assert!(s.open_feeder_url("feeder://notebook/3"));
        assert_eq!(s.tab, Tab::Notebook);
        assert_eq!(s.notebook.open, Some(3));

        assert!(s.open_feeder_url("feeder://library/4"));
        assert_eq!(s.tab, Tab::Notebook);
        assert_eq!(s.notebook.open, Some(4));

        assert!(s.open_feeder_url("feeder://library"));
        assert_eq!(s.tab, Tab::Notebook);
        assert_eq!(s.notebook.open, None);
        assert_eq!(s.notebook.shelf, notebook::Shelf::All);

        s.files.view = files::ProjectView::Files;
        assert!(s.open_feeder_url("feeder://project/alpha/update/42"));
        assert_eq!(s.open, Some(7));
        assert_eq!(s.files.view, files::ProjectView::Updates);
        assert_eq!(s.feed_selected, Some(42));
        assert_eq!(s.pending_scroll, Some(42));

        assert!(s.open_feeder_url("feeder://home"));
        assert_eq!(s.tab, Tab::Home);
        assert_eq!(s.open, None);

        assert!(s.open_feeder_url("feeder://project/missing"));
        assert_eq!(s.toasts.entries().len(), 1);
    }

    #[test]
    fn top_level_tabs_always_reset_to_home_and_keep_curator_independent() {
        let mut s = State::new();
        s.projects.push(sample_summary(1, "alpha", "Alpha"));

        s.select_tab_state(Tab::Curator);
        assert_eq!(s.tab, Tab::Curator);
        assert_eq!(Tab::PRIMARY.iter().position(|t| *t == s.tab), None);

        s.select_tab_state(Tab::Notebook);
        assert_eq!(s.tab, Tab::Notebook);
        assert_eq!(s.notebook.shelf, notebook::Shelf::All);
        assert_eq!(s.notebook.open, None);
        assert!(s.notebook.editor.is_none());

        s.notebook.shelf = notebook::Shelf::Topic("rust".into());
        s.notebook.open = Some(42);
        assert!(s.select_tab_state(Tab::Notebook));
        assert_eq!(s.notebook.shelf, notebook::Shelf::All);
        assert_eq!(s.notebook.open, None);

        s.open_project(1);
        s.home_filter = HomeFilter::Working;
        assert!(s.select_tab_state(Tab::Home));
        assert_eq!(s.tab, Tab::Home);
        assert_eq!(s.open, None);
        assert_eq!(s.home_filter, HomeFilter::All);

        s.branch_filter = Some("feat/x".into());
        s.select_tab_state(Tab::Feed);
        assert_eq!(s.tab, Tab::Feed);
        assert_eq!(s.branch_filter, None);
    }

    #[test]
    fn open_project_preseeds_feed_from_timeline() {
        let mut s = State::new();
        s.projects.push(sample_summary(1, "alpha", "Alpha"));
        s.projects.push(sample_summary(2, "beta", "Beta"));
        s.timeline.push(sample_update(10, 1, "Done: alpha update."));
        s.timeline.push(sample_update(11, 2, "Done: beta update."));
        s.timeline.push(sample_update(12, 1, "Done: second alpha."));

        s.open_project(1);
        assert_eq!(s.open, Some(1));
        assert_eq!(s.feed_for, Some(1));
        assert_eq!(s.feed.iter().map(|u| u.id).collect::<Vec<_>>(), vec![10, 12]);
    }

    #[test]
    fn archiving_pushes_undo_toast() {
        let mut s = State::new();
        s.projects.push(sample_summary(3, "beta", "Beta"));
        s.set_archived(3, true);
        assert!(s.projects[0].project.archived);
        let front = s.toasts.front().unwrap();
        assert_eq!(front.options.kind, ToastKind::UndoArchive { project_id: 3, was_archived: false });
    }

    #[test]
    fn filtered_timeline_uses_db_search_hits_and_branch_and_needs_you() {
        let mut s = State::new();
        let mut sum = sample_summary(1, "alpha", "Alpha");
        sum.needs_you = true;
        sum.last_update_id = Some(99);
        s.projects.push(sum);
        s.feed_search = TextEdit::new("fts5");
        s.search_snapshot_query = "fts5".into();
        s.search_hits.push(sample_update(99, 1, "Done: fixed search.\n\nDecision needed: keep legacy?"));
        s.search_hits.push(sample_update(50, 1, "Done: earlier.\n\nDecision needed: old question?"));
        let visible = s.filtered_timeline();
        assert_eq!(visible.len(), 2);
        let open: Vec<i64> = visible.iter().filter(|(u, _)| s.is_open_attention(u)).map(|(u, _)| u.id).collect();
        assert_eq!(open, vec![99]);

        s.toggle_branch_filter("feat/other");
        assert!(s.filtered_timeline().is_empty());
        s.toggle_branch_filter("feat/other");

        s.dismiss_attention(99);
        assert!(!s.projects[0].needs_you);
        assert!(!s.is_open_attention(&s.search_hits[0]));
        assert_eq!(s.toasts.front().unwrap().options.kind, ToastKind::UndoDismiss { update_id: 99 });

        s.undismiss_attention(99);
        assert!(s.projects[0].needs_you);
        assert!(s.is_open_attention(&s.search_hits[0]));
    }

    #[test]
    fn pulse_halo_only_animates_when_actually_visible() {
        let mut s = State::new();
        let mut pulsing = sample_summary(1, "alpha", "Alpha");
        pulsing.active_pulse = Some(feeder_core::Pulse {
            project_id: 1,
            agent: "Claude Code".into(),
            branch: None,
            status: "Working".into(),
            updated_at: 100,
            expires_at: 200,
        });
        let quiet = sample_summary(2, "beta", "Beta");
        s.projects = vec![pulsing, quiet];

        s.tab = Tab::Home;
        assert!(s.pulse_halo_visible());
        s.tab = Tab::Feed;
        assert!(!s.pulse_halo_visible(), "Feed draws a Spinner, never a breathing orb halo");
        s.tab = Tab::Notebook;
        assert!(!s.pulse_halo_visible());

        s.open = Some(2);
        assert!(!s.pulse_halo_visible(), "a quiet open project does not tick for another project's pulse");
        s.open = Some(1);
        assert!(s.pulse_halo_visible());
        s.files.view = files::ProjectView::Files;
        assert!(!s.pulse_halo_visible());
        s.files.view = files::ProjectView::Updates;
        s.window_visible = false;
        assert!(!s.pulse_halo_visible());
    }

    #[test]
    fn empty_edits_stay_open_and_rapid_replies_get_distinct_temp_ids() {
        let mut s = State::new();
        s.projects.push(sample_summary(1, "alpha", "Alpha"));
        s.feed.push(sample_update(10, 1, "Initial body"));

        s.editing_update = Some(10);
        s.edit_update_body = TextEdit::new("   ");
        s.save_update_edit();
        assert_eq!(s.editing_update, Some(10), "blank edit must stay open instead of discarding silently");

        s.replying_to = Some(10);
        s.reply_draft = TextEdit::new("   ");
        s.submit_reply(10);
        assert_eq!(s.replying_to, Some(10), "blank reply must stay open");

        s.reply_draft = TextEdit::new("First reply");
        s.submit_reply(10);
        s.replying_to = Some(10);
        s.reply_draft = TextEdit::new("Second reply");
        s.submit_reply(10);
        let ids: Vec<i64> = s.replies[&10].iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    fn sample_flag(id: i64, project_id: Option<i64>, source_update_id: Option<i64>) -> feeder_core::Flag {
        feeder_core::Flag {
            id,
            kind: feeder_core::FlagKind::Missing,
            page_id: None,
            project_id,
            agent: "you".into(),
            note: "Remember this".into(),
            suggestion: None,
            status: feeder_core::FlagStatus::Open,
            resolution: None,
            resolved_by: None,
            change_id: None,
            created_at: 0,
            resolved_at: None,
            source_update_id,
        }
    }

    #[test]
    fn flagged_update_stays_marked_until_its_flag_closes() {
        let mut s = State::new();
        s.notebook.gardener.open_flags.push(sample_flag(1, Some(1), Some(10)));
        assert!(s.open_flag_for_update(10));
        assert!(!s.open_flag_for_update(11));
        s.notebook.gardener.open_flags.clear();
        assert!(!s.open_flag_for_update(10));
    }

    #[test]
    fn review_card_counts_only_flags_in_the_chat_scope() {
        let mut s = State::new();
        s.notebook.gardener.open_flags.push(sample_flag(1, Some(1), Some(10)));
        s.notebook.gardener.open_flags.push(sample_flag(2, Some(2), Some(20)));
        assert_eq!(gardener::scoped_flags(&s).len(), 2);
        s.notebook.chat.scope = feeder_core::gardener::Scope::Project(1);
        let ids: Vec<i64> = gardener::scoped_flags(&s).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn home_shows_the_question_behind_needs_you() {
        let mut sum = sample_summary(1, "alpha", "Alpha");
        sum.needs_you = true;
        sum.last_body = Some("Done: drafted pricing.\n\nDecision needed: should EU prices include tax?".into());
        assert_eq!(home::needs_you_question(&sum).as_deref(), Some("Should EU prices include tax?"));
        sum.last_body = Some("Done: nothing to ask.".into());
        assert_eq!(home::needs_you_question(&sum), None);
    }

    #[test]
    fn lightbox_compare_orders_before_and_after_chronologically() {
        let mut s = State::new();
        let mut older = sample_update(10, 1, "Older");
        older.created_at = 1_000;
        older.screenshot = Some("/tmp/older.png".into());
        let mut newer = sample_update(11, 1, "Newer");
        newer.created_at = 2_000;
        newer.media = vec![
            feeder_core::MediaAttachment {
                id: 201,
                update_id: 11,
                position: 0,
                path: "/tmp/a.png".into(),
                width: None,
                height: None,
                caption: None,
            },
            feeder_core::MediaAttachment {
                id: 202,
                update_id: 11,
                position: 1,
                path: "/tmp/b.png".into(),
                width: None,
                height: None,
                caption: None,
            },
        ];
        s.feed = vec![newer, older];

        assert_eq!(s.lightbox_compare_pair(-10, 201), Some((-10, 201)));
        assert_eq!(s.lightbox_compare_pair(201, -10), Some((-10, 201)));
        assert_eq!(s.lightbox_compare_pair(202, 201), Some((201, 202)));
    }

    #[test]
    fn review_status_hides_flag_action_and_home_filter_resets_when_empty() {
        let mut s = State::new();
        s.loaded = true;
        let mut sum = sample_summary(1, "alpha", "Alpha");
        sum.needs_you = false;
        s.projects.push(sum);
        s.home_filter = HomeFilter::NeedsYou;
        s.reconcile_home_filter();
        assert_eq!(s.home_filter, HomeFilter::All);

        assert!(!s.update_is_flagged_or_reviewed(10));
        s.review_statuses.insert(10, FeedReviewStatus::Dismissed);
        assert!(!s.update_is_flagged_or_reviewed(10));
        s.review_statuses.insert(10, FeedReviewStatus::Queued);
        assert!(s.update_is_flagged_or_reviewed(10));
        s.review_statuses.insert(10, FeedReviewStatus::Reviewed { note_id: Some(7) });
        assert!(s.update_is_flagged_or_reviewed(10));
    }

    #[test]
    fn onboarding_steps_advance_retreat_and_reset_on_reopen() {
        assert_eq!(OnboardingStep::Connect.index(), 0);
        assert_eq!(OnboardingStep::Connect.prev(), None);
        assert_eq!(OnboardingStep::Connect.next(), Some(OnboardingStep::FirstUpdate));
        assert_eq!(OnboardingStep::FirstUpdate.index(), 1);
        assert_eq!(OnboardingStep::FirstUpdate.prev(), Some(OnboardingStep::Connect));
        assert_eq!(OnboardingStep::FirstUpdate.next(), Some(OnboardingStep::Curator));
        assert_eq!(OnboardingStep::Curator.index(), 2);
        assert_eq!(OnboardingStep::Curator.prev(), Some(OnboardingStep::FirstUpdate));
        assert_eq!(OnboardingStep::Curator.next(), None);

        let mut s = State::new();
        s.onboarding_step = Some(OnboardingStep::Curator);
        s.finish_onboarding();
        assert!(s.settings.onboarding_skipped);
        assert_eq!(s.onboarding_step, None);

        s.start_onboarding();
        assert!(!s.settings.onboarding_skipped);
        assert_eq!(s.onboarding_step, Some(OnboardingStep::Connect));
    }
}
