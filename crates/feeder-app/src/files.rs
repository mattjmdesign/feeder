//! Files: a project's working tree as a mind map and a file list, with uncommitted changes
//! opened out and the newest edit marked. Read-only: the watcher reads git and file times every
//! two seconds while this view is open (`feeder_core::read_working_tree`); nothing is written and
//! no agent is involved. Git says where and when files changed, not who changed them, so the
//! view says "last edit", never "editing now".

mod preview;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use feeder_core::{ChangeKind, TreeStatus, WorkingTree, ago};
use rune::{Color, Drag, El, KeyEvent, LineCap, MagnifyEvent, Phase, Shape, Transition, Tx, WheelEvent, canvas, col, el, row, text};
use rune_kit::{Button, ButtonVariant, FileNode, FileTree, Kit, Tree, TreeItem, TreeModel, TreeRow, TreeStyle};

use crate::State;
use crate::style::{self, Palette};

/// Which of a project's two views is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProjectView {
    #[default]
    Updates,
    Files,
}

/// Changed files a folder lists before the rest fold into "N more changed".
const MAX_CHANGED: usize = 8;
/// An edit this recent counts as recent work (green); older is shown neutral.
const RECENT_MS: i64 = 10 * 60 * 1000;
/// Labels longer than this are shortened; the full path stays in the VoiceOver label.
const MAX_LABEL: usize = 26;
const SLOT: f32 = 40.0;
const NODE_H: f32 = 30.0;
const GAP_X: f32 = 40.0;
const PILL_PAD: f32 = 12.0;
const PILL_GAP: f32 = 8.0;
const MIN_ZOOM: f32 = 0.3;
const MAX_ZOOM: f32 = 2.0;
const ZOOM_STEP: f32 = 1.25;
/// The file list's width beside the map.
const LIST_W: f32 = 280.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Edited,
    New,
    Deleted,
    Renamed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    change: Option<Change>,
    added: u32,
    removed: u32,
    modified: Option<i64>,
    from: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Camera {
    x: f32,
    y: f32,
    zoom: f32,
}

/// Everything the Files view keeps between frames. One per window; it follows the open project.
#[derive(Default)]
pub struct FilesState {
    pub view: ProjectView,
    preview: preview::Preview,
    /// The last good read and which project it belongs to.
    tree_data: Option<(i64, WorkingTree)>,
    entries: BTreeMap<String, Entry>,
    latest: Option<String>,
    list: TreeModel<FileNode<Entry>>,
    /// Folders opened on the map; "" is the root.
    open: BTreeSet<String>,
    /// Folders whose unchanged files are listed instead of folded into a count.
    show_files: BTreeSet<String>,
    /// The list follows the newest edit until the person picks a file.
    follow: bool,
    pending_reveal: bool,
    /// The last refresh failed; what's shown is the read before it.
    stale: bool,
    camera: Cell<Option<Camera>>,
    anchor: RefCell<Option<(String, [f32; 2])>>,
    directories: Dir,
    layout: RefCell<Option<MapLayout>>,
    layout_revision: Cell<u64>,
    drag_from: Option<[f32; 2]>,
    last_view: Cell<Option<[f32; 2]>>,
    last_map: Cell<Option<[f32; 2]>>,
    last_cam: Cell<Option<Camera>>,
}

impl FilesState {
    pub fn close_preview(&mut self) -> bool {
        if self.preview.is_open() {
            self.preview.close();
            true
        } else {
            false
        }
    }

    /// Takes a fresh read from the watcher. A failed read keeps the last good one on screen.
    pub fn apply(&mut self, project: i64, wt: WorkingTree) {
        let same_project = self.tree_data.as_ref().is_some_and(|(id, _)| *id == project);
        if let TreeStatus::Failed(_) = wt.status
            && same_project
            && self.tree_data.as_ref().is_some_and(|(_, old)| old.status == TreeStatus::Ready)
        {
            self.stale = true;
            return;
        }
        self.stale = false;
        let same_project = same_project && self.tree_data.as_ref().is_some_and(|(_, old)| old.root == wt.root);
        if same_project && self.tree_data.as_ref().is_some_and(|(_, old)| old.files == wt.files && old.changes == wt.changes) {
            self.tree_data = Some((project, wt));
            return;
        }
        let entries = entries_of(&wt);
        let latest = wt.changes.iter().filter_map(|c| c.modified_at.map(|at| (at, &c.path))).max().map(|(_, path)| path.clone());
        let latest_changed = !same_project || self.latest != latest;
        if !same_project {
            self.preview.close();
            self.open.clear();
            self.show_files.clear();
            self.camera.set(None);
            self.last_cam.set(None);
            self.last_map.set(None);
            self.anchor.get_mut().take();
            self.follow = true;
            self.pending_reveal = false;
        }
        let fresh: Vec<&str> = entries
            .iter()
            .filter(|(path, entry)| {
                entry.change.is_some() && (!same_project || self.entries.get(*path).is_none_or(|old| old.change.is_none()))
            })
            .map(|(path, _)| path.as_str())
            .collect();
        // Rebuild Rune's tree only when the set of paths changes. Save timestamps and
        // line counts can update the existing rows without losing their UI state.
        if !same_project || !entries.keys().eq(self.entries.keys()) {
            let expanded: BTreeSet<&str> = if same_project {
                self.entries.keys().flat_map(|path| ancestors(path)).filter(|a| self.list.is_expanded(a)).collect()
            } else {
                BTreeSet::new()
            };
            let mut list =
                FileTree::new().compact_folders(true).key("files-list").build(entries.iter().map(|(p, e)| (p.as_str(), e.clone())));
            for a in expanded {
                list.set_expanded(a, true);
            }
            if same_project {
                for id in self.list.selected() {
                    list.select(id);
                }
            }
            self.list = list;
        } else {
            for (path, entry) in &entries {
                if self.entries.get(path) != Some(entry)
                    && let Some(row) = self.list.get_mut(path)
                {
                    row.data = Some(entry.clone());
                }
            }
        }
        for path in fresh {
            self.open.extend(ancestors(path).map(str::to_owned));
            self.list.expand_to(path);
        }
        if self.follow
            && latest_changed
            && let Some(path) = &latest
        {
            self.list.expand_to(path);
            self.list.select(path);
            self.pending_reveal = true;
        }
        self.directories = trie(&entries);
        self.entries = entries;
        self.latest = latest;
        self.tree_data = Some((project, wt));
        self.invalidate_layout();
    }

    fn invalidate_layout(&self) {
        self.layout.borrow_mut().take();
    }

    fn anchor_at(&mut self, node: String, position: [f32; 2]) {
        *self.anchor.get_mut() = Some((node, position));
        self.invalidate_layout();
    }

    /// Scrolls the list to the newest edit after a rebuild (needs a `Tx`, so it runs from poll).
    pub fn poll(&mut self, tx: &mut Tx, images: Option<&rune::ImageCache>) -> bool {
        if self.pending_reveal {
            self.pending_reveal = false;
            if let Some(l) = self.latest.clone() {
                self.list.reveal(&l, tx);
            }
        }
        self.preview.poll(images)
    }

    /// The read for `project`, if the watcher has delivered one.
    fn data(&self, project: i64) -> Option<&WorkingTree> {
        self.tree_data.as_ref().filter(|(id, _)| *id == project).map(|(_, wt)| wt)
    }

    fn changed(&self) -> usize {
        self.directories.changed
    }

    fn latest_at(&self) -> Option<i64> {
        self.latest.as_ref().and_then(|l| self.entries.get(l)).and_then(|e| e.modified)
    }

    /// The newest edit is recent, on a fresh read.
    fn recent(&self, now: i64) -> bool {
        !self.stale && self.latest_at().is_some_and(|t| now - t < RECENT_MS)
    }
}

fn entries_of(wt: &WorkingTree) -> BTreeMap<String, Entry> {
    let mut out: BTreeMap<String, Entry> =
        wt.files.iter().map(|p| (p.clone(), Entry { change: None, added: 0, removed: 0, modified: None, from: None })).collect();
    for c in &wt.changes {
        let (change, from) = match &c.kind {
            ChangeKind::Edited => (Change::Edited, None),
            ChangeKind::New => (Change::New, None),
            ChangeKind::Deleted => (Change::Deleted, None),
            ChangeKind::Renamed { from } => (Change::Renamed, Some(from.clone())),
        };
        if let Some(old) = &from {
            out.remove(old);
        }
        out.insert(c.path.clone(), Entry { change: Some(change), added: c.added, removed: c.removed, modified: c.modified_at, from });
    }
    out
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(end, _)| &path[..end])
}

// ---------------------------------------------------------------- labels

fn short(label: &str) -> String {
    let n = label.chars().count();
    if n <= MAX_LABEL {
        return label.to_owned();
    }
    let keep = MAX_LABEL - 1;
    let head: String = label.chars().take(keep / 2).collect();
    let tail: String = label.chars().skip(n - (keep - keep / 2)).collect();
    format!("{head}…{tail}")
}

/// One shortening rule for list and map: a path keeps its first folder and as much of its end as
/// fits (`packages/…/navigation/sidebar`); a name keeps both ends.
fn short_path(label: &str) -> String {
    if label.chars().count() <= MAX_LABEL || !label.contains('/') {
        return short(label);
    }
    let parts: Vec<&str> = label.split('/').collect();
    let first = parts[0];
    let mut tail = String::new();
    for part in parts[1..].iter().rev() {
        let next = if tail.is_empty() { (*part).to_owned() } else { format!("{part}/{tail}") };
        if first.chars().count() + 3 + next.chars().count() > MAX_LABEL {
            break;
        }
        tail = next;
    }
    if tail.is_empty() { format!("…/{}", short(parts.last().unwrap_or(&label))) } else { format!("{first}/…/{tail}") }
}

fn counts(e: &Entry) -> String {
    match (e.added, e.removed) {
        (a, 0) => format!("+{a}"),
        (0, r) => format!("−{r}"),
        (a, r) => format!("+{a} −{r}"),
    }
}

// ---------------------------------------------------------------- map layout

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, Entry)>,
    total: usize,
    changed: usize,
}

fn trie(entries: &BTreeMap<String, Entry>) -> Dir {
    let mut root = Dir::default();
    for (path, entry) in entries {
        let mut dir = &mut root;
        let (folders, file) = path.rsplit_once('/').unwrap_or(("", path));
        let changed = usize::from(entry.change.is_some());
        dir.total += 1;
        dir.changed += changed;
        for folder in folders.split('/').filter(|name| !name.is_empty()) {
            dir = dir.dirs.entry(folder.to_owned()).or_default();
            dir.total += 1;
            dir.changed += changed;
        }
        dir.files.push((file.to_owned(), entry.clone()));
    }
    root
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Root,
    Folder { open: bool },
    File(Option<Change>),
    Fold,
    FoldChanged,
}

struct MapNode {
    id: String,
    label: String,
    full: String,
    meta: String,
    kind: Kind,
    changed: bool,
    latest: bool,
    on_path: bool,
    depth: usize,
    y: f32,
    w: f32,
    children: Vec<usize>,
}

impl MapNode {
    fn mark(&self) -> Option<&'static str> {
        match self.kind {
            Kind::File(Some(Change::New)) => Some("+"),
            Kind::File(Some(Change::Edited)) | Kind::FoldChanged => Some("●"),
            Kind::File(Some(Change::Deleted)) => Some("−"),
            Kind::File(Some(Change::Renamed)) => Some("→"),
            _ => None,
        }
    }

    fn parts(&self) -> [Option<(&str, rune::TextStyle)>; 5] {
        [
            self.mark().map(|mark| (mark, style::LABEL)),
            self.latest.then_some(("Latest", style::LABEL)),
            Some((&self.label, style::CONTROL)),
            (!self.meta.is_empty()).then_some((&self.meta, style::META)),
            (self.kind == Kind::Folder { open: false }).then_some(("▸", style::META)),
        ]
    }

    fn width(&self) -> f32 {
        let mut width = 2.0 * PILL_PAD + 2.0; // padding and both borders
        for (i, (content, font)) in self.parts().into_iter().flatten().enumerate() {
            if i != 0 {
                width += PILL_GAP;
            }
            // Round each run up to allow for pixel snapping at the display scale.
            width += text::measure(content, &[], font, None).0.ceil();
        }
        width.max(56.0)
    }
}

struct Layout<'a> {
    fs: &'a FilesState,
    name: &'a str,
    nodes: Vec<MapNode>,
    next_slot: f32,
}

impl Layout<'_> {
    fn push(&mut self, n: MapNode) -> usize {
        let mut n = n;
        n.w = n.width();
        self.nodes.push(n);
        self.nodes.len() - 1
    }
    fn leaf_y(&mut self) -> f32 {
        let y = self.next_slot * SLOT;
        self.next_slot += 1.0;
        y
    }

    fn dir(&mut self, dir: &Dir, path: &str, name: &str, depth: usize) -> usize {
        // A folder holding only one folder joins it (`site/src/pages`), as the list does.
        let (mut dir, mut path, mut name) = (dir, path.to_owned(), name.to_owned());
        while depth > 0 && dir.files.is_empty() && dir.dirs.len() == 1 {
            let (n, d) = dir.dirs.iter().next().unwrap();
            path = format!("{path}/{n}");
            name = format!("{name}/{n}");
            dir = d;
        }
        let fs = self.fs;
        let is_open = depth == 0 || fs.open.contains(&path);
        let changed = dir.changed;
        let on_path = fs.latest.as_deref().is_some_and(|l| depth == 0 || l.strip_prefix(&path).is_some_and(|tail| tail.starts_with('/')));
        let meta = if changed > 0 {
            format!("{changed} changed")
        } else if depth == 0 {
            "no changes".to_owned()
        } else {
            let n = dir.total;
            format!("{n} file{}", if n == 1 { "" } else { "s" })
        };
        let kind = if depth == 0 { Kind::Root } else { Kind::Folder { open: is_open } };
        let full = if depth == 0 { self.name.to_owned() } else { path.clone() };
        let label = short_path(if depth == 0 { &full } else { &name });
        let me = self.push(MapNode {
            id: path.clone(),
            w: 0.0,
            label,
            full,
            meta,
            kind,
            changed: changed > 0,
            latest: false,
            on_path,
            depth,
            y: 0.0,
            children: Vec::new(),
        });
        if !is_open {
            self.nodes[me].y = self.leaf_y();
            return me;
        }
        let mut kids = Vec::new();
        for (n, d) in &dir.dirs {
            let p = if path.is_empty() { n.clone() } else { format!("{path}/{n}") };
            kids.push(self.dir(d, &p, n, depth + 1));
        }
        let show_all = fs.show_files.contains(&path);
        let (mut folded_changed, mut listed_changed) = (0, 0);
        // The newest edits list first, so a long list folds its oldest.
        let folded = if show_all { 0 } else { dir.files.iter().filter(|(_, entry)| entry.change.is_none()).count() };
        let mut files: Vec<_> = dir.files.iter().filter(|(_, entry)| show_all || entry.change.is_some()).collect();
        let newest = fs.latest.as_deref().and_then(|p| {
            let (parent, file) = p.rsplit_once('/').unwrap_or(("", p));
            (parent == path).then_some(file)
        });
        files.sort_by(|a, b| {
            (newest == Some(b.0.as_str()))
                .cmp(&(newest == Some(a.0.as_str())))
                .then_with(|| b.1.modified.cmp(&a.1.modified))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (n, f) in files {
            if f.change.is_some() && !show_all {
                if listed_changed >= MAX_CHANGED {
                    folded_changed += 1;
                    continue;
                }
                listed_changed += 1;
            }
            let p = if path.is_empty() { n.clone() } else { format!("{path}/{n}") };
            let latest = fs.latest.as_deref() == Some(p.as_str());
            let meta = match f.change {
                Some(Change::Deleted) => "deleted".to_owned(),
                Some(_) => counts(f),
                None => String::new(),
            };
            let label = match (&f.change, &f.from) {
                (Some(Change::Renamed), Some(old)) => format!("{} → {}", short(old.rsplit('/').next().unwrap_or(old)), short(n)),
                _ => short(n),
            };
            let y = self.leaf_y();
            let id = self.push(MapNode {
                id: p.clone(),
                w: 0.0,
                label,
                full: p,
                meta,
                kind: Kind::File(f.change),
                changed: f.change.is_some(),
                latest,
                on_path: latest,
                depth: depth + 1,
                y,
                children: Vec::new(),
            });
            kids.push(id);
        }
        if folded_changed > 0 {
            let label = format!("{folded_changed} more changed");
            let y = self.leaf_y();
            let id = self.push(MapNode {
                id: path.clone(),
                w: 0.0,
                full: label.clone(),
                label,
                meta: String::new(),
                kind: Kind::FoldChanged,
                changed: true,
                latest: false,
                on_path: false,
                depth: depth + 1,
                y,
                children: Vec::new(),
            });
            kids.push(id);
        }
        if folded > 0 {
            let label = format!("{folded} more file{}", if folded == 1 { "" } else { "s" });
            let y = self.leaf_y();
            let id = self.push(MapNode {
                id: path.clone(),
                w: 0.0,
                full: label.clone(),
                label,
                meta: String::new(),
                kind: Kind::Fold,
                changed: false,
                latest: false,
                on_path: false,
                depth: depth + 1,
                y,
                children: Vec::new(),
            });
            kids.push(id);
        }
        if kids.is_empty() {
            self.nodes[me].y = self.leaf_y();
        } else {
            let first = self.nodes[kids[0]].y;
            let last = self.nodes[*kids.last().unwrap()].y;
            self.nodes[me].y = (first + last) / 2.0;
        }
        self.nodes[me].children = kids;
        me
    }
}

struct MapLayout {
    name: String,
    font_epoch: u64,
    revision: u64,
    nodes: Vec<MapNode>,
    columns: Vec<f32>,
    size: [f32; 2],
    latest: Option<[f32; 2]>,
    kinds: Vec<Change>,
}

impl FilesState {
    fn map_layout(&self, name: &str) -> std::cell::Ref<'_, MapLayout> {
        let font_epoch = text::font_epoch();
        let mut cached = self.layout.borrow_mut();
        if cached.as_ref().is_none_or(|layout| layout.name != name || layout.font_epoch != font_epoch) {
            let revision = self.layout_revision.get().wrapping_add(1);
            self.layout_revision.set(revision);
            *cached = Some(map_layout(self, name, revision));
        }
        drop(cached);
        std::cell::Ref::map(self.layout.borrow(), |cached| cached.as_ref().expect("map layout was just built"))
    }
}

fn map_layout(fs: &FilesState, name: &str, revision: u64) -> MapLayout {
    let mut lay = Layout { fs, name, nodes: Vec::new(), next_slot: 0.0 };
    lay.dir(&fs.directories, "", "", 0);
    let nodes = lay.nodes;
    let height = lay.next_slot * SLOT;

    let depth_max = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    let mut widths = vec![0.0f32; depth_max + 1];
    let mut kinds = Vec::new();
    for n in &nodes {
        widths[n.depth] = widths[n.depth].max(n.w);
        if let Kind::File(Some(change)) = n.kind
            && !kinds.contains(&change)
        {
            kinds.push(change);
        }
    }
    let mut columns = vec![0.0f32; depth_max + 1];
    for depth in 1..=depth_max {
        columns[depth] = columns[depth - 1] + widths[depth - 1] + GAP_X;
    }
    let width = columns[depth_max] + widths[depth_max];
    let latest = nodes.iter().find(|n| n.latest).map(|n| [columns[n.depth] + n.w, n.y + NODE_H / 2.0]);
    MapLayout { name: name.to_owned(), font_epoch: text::font_epoch(), revision, nodes, columns, size: [width, height], latest, kinds }
}

fn map_stage(layout: &MapLayout, p: &Palette, recent: bool) -> El {
    let [width, height] = layout.size;
    let nodes = &layout.nodes;
    let col_x = &layout.columns;
    // Lines carry the structure, so even the quietest stays at text_3 (above 3:1); weight, not
    // faintness, sets the hierarchy.
    let mut lines = Shape::new();
    let mut active = Shape::new();
    for n in nodes {
        for &c in &n.children {
            let ch = &nodes[c];
            let (x0, y0) = (col_x[n.depth] + n.w, n.y + NODE_H / 2.0);
            let (x1, y1) = (col_x[ch.depth], ch.y + NODE_H / 2.0);
            let mx = (x0 + x1) / 2.0;
            let path = rune::Path::new().move_to([x0, y0]).cubic_to([mx, y0], [mx, y1], [x1, y1]);
            if ch.on_path {
                active = active.stroke_path(&path, 2.0, LineCap::Round, if recent { p.live } else { p.text_2 });
            } else if ch.changed {
                lines = lines.stroke_path(&path, 1.25, LineCap::Round, p.text_2);
            } else {
                lines = lines.stroke_path(&path, 1.0, LineCap::Round, p.text_3);
            }
        }
    }
    let mut stage = el()
        .size(width, height)
        .child(canvas(lines).key("files-lines").size(width, height).absolute(0.0, 0.0))
        .child(canvas(active).key("files-active").size(width, height).absolute(0.0, 0.0));
    for n in nodes {
        let position = [col_x[n.depth], n.y];
        stage = stage.child(map_node(n, position, p, recent).absolute(position[0], position[1]));
    }
    stage
}

fn map_node(n: &MapNode, position: [f32; 2], p: &Palette, recent: bool) -> El {
    let deleted = n.kind == Kind::File(Some(Change::Deleted));
    let (fg, border, fill) = match n.kind {
        Kind::Root => (p.text, p.text_3, p.card),
        // Green only for a recent edit; an older "latest" keeps a normal border and its tag.
        _ if n.latest && recent => (p.text, p.live, p.live.alpha(0.10)),
        _ if n.latest => (p.text, p.text_3, p.card),
        _ if deleted => (p.text_3, p.hairline, p.bg),
        Kind::FoldChanged => (p.text_2, p.text_3.alpha(0.55), p.bg),
        _ if n.changed => (p.text, p.text_3.alpha(0.55), p.card),
        Kind::Fold => (p.text_3, p.hairline, p.bg),
        _ => (p.text_2, p.hairline, p.card),
    };
    let mut r = row().items_center().gap(PILL_GAP).h(NODE_H).w(n.w).px(PILL_PAD).radius(NODE_H / 2.0).fill(fill).border(1.0, border);
    let mark_color = match n.kind {
        Kind::File(Some(Change::New)) => p.live,
        Kind::File(Some(Change::Edited | Change::Renamed)) => p.text_2,
        _ => p.text_3,
    };
    let colors = [mark_color, p.text_2, fg, p.text_3, p.text_3];
    for (i, (part, color)) in n.parts().into_iter().zip(colors).enumerate() {
        if let Some((content, font)) = part {
            let mut label = text(content.to_owned()).font(font).color(color);
            if i == 2 && deleted {
                label = label.strike(0, content.len(), Some(color));
            }
            r = r.child(label);
        }
    }
    let id = n.id.clone();
    let key = match n.kind {
        Kind::Fold => format!("files-fold:{}", n.id),
        Kind::FoldChanged => format!("files-fold-changed:{}", n.id),
        _ => format!("files-node:{}", n.id),
    };
    let r = r.key(key);
    let r = if matches!(n.kind, Kind::Folder { .. } | Kind::File(_)) { r.help(n.full.clone()) } else { r };
    match n.kind {
        // The root only labels the map; folders and files act.
        Kind::Root => r.label(format!("{}, {}", n.full, n.meta)),
        Kind::Folder { open } => {
            r.label(format!("{}, folder, {}", n.full, n.meta)).role("button").expanded(open).on_press(move |s: &mut State| {
                if open {
                    s.files.open.remove(&id);
                } else {
                    s.files.open.insert(id.clone());
                }
                s.files.anchor_at(id.clone(), position);
            })
        }
        Kind::File(c) => {
            let what = match c {
                Some(Change::New) => "new file",
                Some(Change::Edited) => "edited",
                Some(Change::Deleted) => "deleted",
                Some(Change::Renamed) => "renamed",
                None => "unchanged",
            };
            let latest = if n.latest { ", latest edit" } else { "" };
            let meta = if n.meta.is_empty() || deleted { String::new() } else { format!(", {}", n.meta) };
            let r = r.label(format!("{}, {what}{latest}{meta}", n.full));
            if deleted {
                r
            } else {
                // Pressing a file shows it in the list and stops following the newest edit.
                r.role("button").on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    s.files.follow = false;
                    s.files.list.reveal(&id, tx);
                })
            }
        }
        Kind::Fold | Kind::FoldChanged => r.label(format!("Show {}", n.full)).role("button").on_press(move |s: &mut State| {
            s.files.show_files.insert(id.clone());
            s.files.invalidate_layout();
        }),
    }
}

// ---------------------------------------------------------------- camera

/// Where the map sits before the person moves it: the whole map when it fits with 12 pt text or
/// larger, otherwise actual size with the newest edit in view.
fn auto_camera(m: &MapLayout, view: [f32; 2]) -> Camera {
    let [w, h] = m.size;
    let fit = ((view[0] - 64.0) / w).min((view[1] - 112.0) / h).min(1.0);
    if fit >= 12.0 / 13.0 {
        return Camera { x: 32.0, y: (view[1] - 48.0 - h * fit) / 2.0, zoom: fit };
    }
    let [lx, ly] = m.latest.unwrap_or([w, h / 2.0]);
    Camera { x: (view[0] - 48.0 - lx).min(32.0), y: (view[1] / 2.0 - ly).min(32.0), zoom: 1.0 }
}

/// The whole map, centred, clear of the legend and zoom bar (the Fit button and ⌘9).
fn fit_camera(size: [f32; 2], view: [f32; 2]) -> Camera {
    let zoom = ((view[0] - 64.0) / size[0]).min((view[1] - 112.0) / size[1]).clamp(MIN_ZOOM, MAX_ZOOM);
    Camera { x: (view[0] - size[0] * zoom) / 2.0, y: (view[1] - 48.0 - size[1] * zoom) / 2.0, zoom }
}

/// Keeps at least 80 pt of the map on screen so it can't be lost off an edge.
fn clamp_camera(c: Camera, size: [f32; 2], view: [f32; 2]) -> Camera {
    let keep = 80.0_f32;
    let axis = |position: f32, size: f32, viewport: f32| {
        let keep = keep.min(size / 2.0).min(viewport / 2.0);
        position.clamp(keep - size, viewport - keep)
    };
    Camera { x: axis(c.x, size[0] * c.zoom, view[0]), y: axis(c.y, size[1] * c.zoom, view[1]), zoom: c.zoom }
}

/// Zooms to `zoom` keeping the map point under `at` (viewport points) where it is.
fn zoom_about(c: Camera, zoom: f32, at: [f32; 2]) -> Camera {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let k = zoom / c.zoom;
    Camera { x: at[0] - (at[0] - c.x) * k, y: at[1] - (at[1] - c.y) * k, zoom }
}

/// ⌘+ / ⌘− zoom about the middle, ⌘0 actual size, ⌘9 the whole map. Only while Files shows.
pub fn on_key(s: &mut State, e: &KeyEvent) -> bool {
    let fs = &mut s.files;
    if s.open.is_none() || fs.view != ProjectView::Files {
        return false;
    }
    if fs.preview.is_open() {
        if matches!(e.key, rune::Key::Escape) && !s.settings_open && !s.menu_open && !s.palette_open {
            fs.preview.close();
            return true;
        }
        return false;
    }
    let (Some(view), Some(size)) = (fs.last_view.get(), fs.last_map.get()) else { return false };
    let c = fs.camera.get().or(fs.last_cam.get()).unwrap_or(Camera { x: 0.0, y: 0.0, zoom: 1.0 });
    let centre = [view[0] / 2.0, view[1] / 2.0];
    let next = if e.is_command('=') || e.is_command('+') {
        zoom_about(c, c.zoom * ZOOM_STEP, centre)
    } else if e.is_command('-') {
        zoom_about(c, c.zoom / ZOOM_STEP, centre)
    } else if e.is_command('0') {
        zoom_about(c, 1.0, centre)
    } else if e.is_command('9') {
        fit_camera(size, view)
    } else {
        return false;
    };
    fs.camera.set(Some(clamp_camera(next, size, view)));
    true
}

// ---------------------------------------------------------------- views

/// The pannable, zoomable viewport around the map, with its zoom bar and legend.
fn map_canvas(fs: &FilesState, now: i64, p: &Palette, k: &Kit, m: &MapLayout, view: [f32; 2]) -> El {
    let size = m.size;
    let [mw, mh] = size;
    let mut cam = fs.camera.get().or(fs.last_cam.get()).unwrap_or_else(|| auto_camera(m, view));
    // Keep the clicked folder under the pointer as its children take up more room.
    // Edges and their nodes use the same new geometry in the same frame.
    if let Some((id, [x, y])) = fs.anchor.borrow_mut().take() {
        if let Some(node) = m.nodes.iter().find(|n| n.id == id) {
            cam.x += (x - m.columns[node.depth]) * cam.zoom;
            cam.y += (y - node.y) * cam.zoom;
        }
        fs.camera.set(Some(cam));
    }
    let cam = clamp_camera(cam, size, view);
    fs.last_view.set(Some(view));
    fs.last_map.set(Some(size));
    fs.last_cam.set(Some(cam));
    let fit = fit_camera(size, view);
    let centre = [view[0] / 2.0, view[1] / 2.0];
    let recent = fs.recent(now);
    let stage = rune::memo("files-map-stage", (m.revision, p.bg == style::palette(true).bg, recent), || map_stage(m, p, recent));
    // `scale` draws about the centre and keeps the layout size, so offset back to the top-left.
    let placed = stage.scale(cam.zoom).absolute(cam.x - (mw - mw * cam.zoom) / 2.0, cam.y - (mh - mh * cam.zoom) / 2.0);

    let set = move |to: Camera| move |s: &mut State| s.files.camera.set(Some(clamp_camera(to, size, view)));
    let controls = row()
        .gap(2.0)
        .items_center()
        .padding(4.0)
        .radius(10.0)
        .fill(p.card)
        .border(1.0, p.hairline)
        .child(
            Button::new("−")
                .variant(ButtonVariant::Ghost)
                .help("Zoom Out (⌘−)")
                .on_press(set(zoom_about(cam, cam.zoom / ZOOM_STEP, centre)))
                .build(k),
        )
        // The percentage is the Actual Size button, so it isn't shown twice.
        .child(
            Button::new(format!("{:.0}%", cam.zoom * 100.0))
                .variant(ButtonVariant::Ghost)
                .help("Actual Size (⌘0)")
                .on_press(set(zoom_about(cam, 1.0, centre)))
                .build(k)
                .w(56.0),
        )
        .child(
            Button::new("+")
                .variant(ButtonVariant::Ghost)
                .help("Zoom In (⌘+)")
                .on_press(set(zoom_about(cam, cam.zoom * ZOOM_STEP, centre)))
                .build(k),
        )
        .child(el().size(1.0, 16.0).fill(p.hairline))
        .child(Button::new("Fit").variant(ButtonVariant::Ghost).help("Fit to Window (⌘9)").on_press(set(fit)).build(k));

    let mut viewport = el()
        .key("files-viewport")
        .size(view[0], view[1])
        .clip_rounded(8.0)
        .instant_changes()
        .label("Code map. Drag or scroll to move, pinch or ⌘ scroll to zoom.")
        .on_wheel(move |s: &mut State, e: WheelEvent, _tx: &mut Tx| {
            let c = s.files.last_cam.get().unwrap_or(cam);
            let next = if e.mods.cmd {
                zoom_about(c, c.zoom * (-e.dy * 0.01).exp(), [e.x, e.y])
            } else {
                Camera { x: c.x + e.dx, y: c.y + e.dy, zoom: c.zoom }
            };
            s.files.camera.set(Some(clamp_camera(next, size, view)));
            true
        })
        .on_magnify(move |s: &mut State, e: MagnifyEvent, _tx: &mut Tx| {
            let c = s.files.last_cam.get().unwrap_or(cam);
            s.files.camera.set(Some(clamp_camera(zoom_about(c, c.zoom * (1.0 + e.delta), [e.x, e.y]), size, view)));
            true
        })
        // A two-finger double tap switches between the whole map and actual size.
        .on_smart_magnify(move |s: &mut State, x: f32, y: f32, _tx: &mut Tx| {
            let c = s.files.last_cam.get().unwrap_or(cam);
            let next = if (c.zoom - 1.0).abs() < 0.01 { fit } else { zoom_about(c, 1.0, [x, y]) };
            s.files.camera.set(Some(clamp_camera(next, size, view)));
            true
        })
        // Dragging the background moves the map.
        .on_drag(move |s: &mut State, d: &mut Drag, _tx: &mut Tx| match d.phase {
            Phase::Down => {
                let c = s.files.last_cam.get().unwrap_or(cam);
                s.files.drag_from = Some([c.x, c.y]);
            }
            Phase::Move => {
                if let Some([x, y]) = s.files.drag_from {
                    let z = s.files.last_cam.get().unwrap_or(cam).zoom;
                    s.files.camera.set(Some(clamp_camera(Camera { x: x + d.dx, y: y + d.dy, zoom: z }, size, view)));
                }
            }
            Phase::Up => s.files.drag_from = None,
        })
        .child(placed);
    let legend = (!m.kinds.is_empty()).then(|| legend(p, &m.kinds).padding(8.0).px(10.0).radius(10.0).fill(p.card).border(1.0, p.hairline));
    viewport = viewport.child(controls.pin(None, None, Some(4.0), Some(8.0)));
    if let Some(legend) = legend {
        let bottom = if view[0] < 540.0 { 48.0 } else { 8.0 };
        viewport = viewport.child(legend.pin(Some(4.0), None, None, Some(bottom)));
    }
    viewport
}

/// The markers on screen; nothing when nothing has changed.
fn legend(p: &Palette, kinds: &[Change]) -> El {
    let chip = |mark: &str, c: Color, label: &str| {
        row()
            .gap(6.0)
            .items_center()
            .child(text(mark.to_owned()).font(style::LABEL).color(c))
            .child(text(label.to_owned()).font(style::META).color(p.text_2))
    };
    let mut r = row().gap(10.0).items_center();
    for (kind, mark, c, label) in [
        (Change::Edited, "●", p.text_2, "Edited"),
        (Change::New, "+", p.live, "New"),
        (Change::Renamed, "→", p.text_2, "Renamed"),
        (Change::Deleted, "−", p.text_3, "Deleted"),
    ] {
        if kinds.contains(&kind) {
            r = r.child(chip(mark, c, label));
        }
    }
    r
}

fn preview_file(s: &mut State, path: &str) {
    let Some(entry) = s.files.entries.get(path) else { return };
    let Some(root) = s.open.and_then(|id| s.files.data(id)).and_then(|tree| tree.root.clone()) else { return };
    s.files.follow = false;
    s.files.preview.open(root, path, entry.change == Some(Change::Deleted), s.wake.clone());
}

fn file_list(s: &State, p: &Palette, k: &Kit) -> El {
    let fs = &s.files;
    let latest = fs.latest.clone();
    let pal = *p;
    let recent = fs.recent(s.now);
    Tree::new(|s: &State| &s.files.list, |s: &mut State| &mut s.files.list, FileNode::label)
        .name("Files")
        .style(TreeStyle::from_kit(k).guides_on_hover(0.55))
        .row(move |n: &FileNode<Entry>, r: &TreeRow| {
            // Let the tree fit the name to its measured width and retain the full tooltip.
            let mut item = TreeItem::new(&n.name).icon(n.icon(r.expanded == Some(true)));
            match &n.data {
                Some(e) if e.change == Some(Change::Deleted) => item = item.label_color(pal.text_3).badge("Deleted"),
                Some(e) if e.change.is_some() => {
                    item = item.changes(e.added, e.removed);
                    if latest.as_deref() == Some(n.path.as_str()) {
                        item = item.badge("Latest");
                        if recent {
                            item = item.dot(pal.live);
                        }
                    }
                }
                Some(_) => item = item.label_color(pal.text_2),
                None => {}
            }
            item
        })
        .on_select(|s: &mut State, id, _| {
            s.files.follow = false;
            preview_file(s, id);
        })
        .on_open(|s: &mut State, id, _| preview_file(s, id))
        .build(s, k)
        .grow(1.0)
        .min_h(0.0)
}

/// One centred message with at most one action, for states with nothing to map.
fn notice(p: &Palette, title: &str, detail: Option<String>, action: Option<El>) -> El {
    let mut c = col().gap(8.0).items_center().child(text(title.to_owned()).font(style::NAME).color(p.text));
    if let Some(d) = detail {
        c = c.child(text(d).font(style::META).color(p.text_3).truncate_middle().single_line().max_w(420.0));
    }
    if let Some(a) = action {
        c = c.child(a.mt(8.0));
    }
    col().grow(1.0).center().child(c)
}

/// The skeleton while the first read runs, in the loaded view's two panels.
fn loading(p: &Palette) -> El {
    let pill = |w: f32| el().size(w, NODE_H).radius(NODE_H / 2.0).fill(p.hairline);
    let line = |w: f32| el().size(w, 12.0).radius(6.0).fill(p.hairline);
    let list = col()
        .w(LIST_W)
        .fill(p.card)
        .radius(12.0)
        .border(1.0, p.hairline)
        .padding(16.0)
        .gap(14.0)
        .child(line(120.0))
        .child(line(180.0))
        .child(line(150.0))
        .child(line(200.0));
    let map = row()
        .gap(GAP_X)
        .items_center()
        .child(pill(150.0))
        .child(col().gap(SLOT - NODE_H).child(pill(90.0)).child(pill(120.0)).child(pill(80.0)));
    row()
        .gap(8.0)
        .grow(1.0)
        .label("Reading files")
        .busy(true)
        .child(list)
        .child(col().grow(1.0).fill(p.card).radius(12.0).border(1.0, p.hairline).center().child(map))
}

/// The status line under the project's title: what's known, not that someone is working.
pub fn status_line(s: &State, id: i64) -> Option<String> {
    let fs = &s.files;
    let wt = fs.data(id)?;
    if wt.status != TreeStatus::Ready {
        return None;
    }
    let changed = fs.changed();
    let mut line = if changed == 0 {
        match &wt.last_commit {
            Some(c) => format!("No local changes · last commit {} · {}", ago(c.at, s.now), short_commit(&c.subject)),
            None => "No local changes".into(),
        }
    } else {
        match fs.latest_at() {
            Some(t) => format!("{changed} changed · last edit {}", ago(t, s.now)),
            None => format!("{changed} changed"),
        }
    };
    if let Some(b) = &wt.branch {
        line = format!("{} · {line}", short(b));
    }
    Some(line)
}

fn short_commit(subject: &str) -> String {
    if subject.chars().count() > 60 { format!("{}…", subject.chars().take(59).collect::<String>()) } else { subject.to_owned() }
}

/// The Files view for project `id`, filling `size` (the space under the project's header).
pub fn view(s: &State, id: i64, p: &Palette, k: &Kit, size: [f32; 2]) -> El {
    let fs = &s.files;
    let name = s.summary(id).map_or("", |sm| sm.project.name.as_str());
    let list_w = LIST_W.min((size[0] * 0.34).max(180.0));
    let Some(wt) = fs.data(id) else { return col().key("files").grow(1.0).px(8.0).pb(8.0).child(loading(p)) };
    let pane = |child: El| {
        col().key("files-pane").grow(1.0).min_w(0.0).min_h(0.0).fill(p.card).radius(12.0).border(1.0, p.hairline).padding(8.0).child(child)
    };
    let list =
        |s: &State| col().w(list_w).min_h(0.0).fill(p.card).radius(12.0).border(1.0, p.hairline).padding(8.0).child(file_list(s, p, k));
    let status = status_line(s, id);
    let body = match &wt.status {
        TreeStatus::NoFolder => {
            pane(notice(p, "No folder linked", Some("Post an update from this repository to link its folder.".into()), None))
        }
        TreeStatus::Missing => pane(notice(p, "Folder not found", wt.root.as_deref().map(style::home_relative), None)),
        // Without git there's nothing to compare against, but the folder is still worth showing.
        TreeStatus::Ready | TreeStatus::NotGit if fs.preview.is_open() => {
            let preview_pane = col()
                .key("files-pane")
                .grow(1.0)
                .min_w(0.0)
                .min_h(0.0)
                .fill(p.card)
                .radius(12.0)
                .border(1.0, p.hairline)
                .clip_rounded(12.0)
                .instant_changes()
                .child(preview::view(s, p, k).unwrap_or_else(el));
            row().gap(8.0).grow(1.0).min_h(0.0).child(list(s)).child(preview_pane)
        }
        TreeStatus::NotGit => row().gap(8.0).grow(1.0).min_h(0.0).child(list(s)).child(pane(notice(
            p,
            "No git history, so changes can't be shown",
            wt.root.as_deref().map(style::home_relative),
            None,
        ))),
        TreeStatus::Failed(_) => pane(notice(p, "Couldn't read this folder", wt.root.as_deref().map(style::home_relative), None)),
        TreeStatus::Ready => {
            // Inner size of the map pane: the list, the gap and the pane's padding come off.
            // The status line and the stale notice each take a 16 pt line plus the 8 pt gap.
            let lines = f32::from(u8::from(status.is_some()) + u8::from(fs.stale)) * 24.0;
            let view = [(size[0] - list_w - 8.0 - 16.0 - 16.0).max(1.0), (size[1] - 8.0 - 16.0 - lines).max(1.0)];
            let m = fs.map_layout(name);
            row().gap(8.0).grow(1.0).min_h(0.0).child(list(s)).child(pane(map_canvas(fs, s.now, p, k, &m, view)))
        }
    };
    let mut out = col().key("files").grow(1.0).min_h(0.0).px(8.0).pb(8.0).gap(8.0).transition(Transition::none());
    if let Some(line) = status {
        out = out.child(text(line).font(style::META).color(p.text_3).truncate().min_w(0.0).px(8.0));
    }
    if fs.stale {
        let since = ago(wt.read_at, s.now);
        out = out.child(
            row().gap(8.0).items_center().px(8.0).child(
                text(format!("⚠ Couldn't refresh · showing changes from {since}"))
                    .font(style::META)
                    .color(p.text_2)
                    .live_region(rune::LiveRegion::Polite),
            ),
        );
    }
    out.child(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use feeder_core::FileChange;

    #[test]
    fn latest_stays_visible_when_changes_share_a_timestamp() {
        let changes: Vec<_> = (0..MAX_CHANGED + 2)
            .map(|i| FileChange {
                path: format!("folder/file-{i:02}.rs"),
                kind: ChangeKind::Edited,
                added: 1,
                removed: 0,
                modified_at: Some(500),
            })
            .collect();
        let mut state = FilesState::default();
        let tree = WorkingTree {
            status: TreeStatus::Ready,
            root: None,
            branch: None,
            files: changes.iter().map(|change| change.path.clone()).collect(),
            changes,
            last_commit: None,
            read_at: 600,
        };
        state.apply(1, tree.clone());
        let revision = {
            let layout = state.map_layout("Project");
            let newest = layout.nodes.iter().find(|node| node.latest).unwrap();
            assert_eq!(newest.id, "folder/file-09.rs");
            assert!(layout.latest.is_some());
            layout.revision
        };
        state.apply(1, WorkingTree { read_at: 700, ..tree });
        assert_eq!(state.map_layout("Project").revision, revision);
    }

    #[test]
    fn zooming_out_a_small_map_keeps_camera_bounds_ordered() {
        let camera = Camera { x: 500.0, y: 500.0, zoom: MIN_ZOOM };
        let result = clamp_camera(camera, [58.0, 40.0], [200.0, 180.0]);
        assert!(result.x < 200.0);
        assert!(result.y < 180.0);
    }
}
