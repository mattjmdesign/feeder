//! The Notebook tab: Feeder's freeform Markdown knowledge library, modelled on Obsidian & Notion.
//!
//! Writing a new note opens a borderless blank canvas (title + single category selector + live
//! syntax-decorated `.md` editor with formatting toolbar, keyboard shortcuts, smart list
//! continuation, and Write/Preview toggle). Reading renders full GFM Markdown with syntax-highlighted
//! code blocks, tables, Obsidian `> [!note]` callouts, and interactive persisted task checkboxes.

use std::collections::HashMap;

use feeder_core::{
    CANONICAL_TOPICS, Kind, Page, PageDraft, Standing, ago, infer_topic, normalize_topic, toggle_markdown_task, topic_counts, topic_label,
};
use rune::{
    Color, Cursor, El, Icon, Key, KeyEvent, Link, LinkPaint, ScrollSpec, Span, TextEdit, Transition, Tx, col, el, icon, oklch, row, text,
    text_input,
};
use rune_kit::shell::{SidebarPeek, SidebarToggle};
use rune_kit::syntax::{self, SyntaxTheme};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Divider, Kit, Markdown, Menu, MenuItem, PaneState, Placement,
    SegmentedControl, Select, SelectAppearance, SidebarHeader, SidebarRow, SplitView, TextInput, Toolbar, Tree, TreeItem, TreeModel,
    TreeRow, TreeStyle, scroll_bar,
};

use crate::style::{self, Palette};
use crate::watch::NotebookOp;
use crate::{CopyTarget, State, ToastItem, ToastKind, agents, chrome, feed, orb};

/// How far a list row's hover surface reaches past the text column on each side.
const ROW_BLEED: f32 = 10.0;
const INDEX_MAX_W: f32 = 760.0;
const DOC_MAX_W: f32 = 720.0;

static PLUS_ICON: Icon = Icon {
    name: "nb-plus",
    view: 16.0,
    body: r#"<path d="M8 3.25V12.75M3.25 8H12.75" stroke="currentColor" stroke-width="1.35" stroke-linecap="round"/>"#,
};

static MORE_ICON: Icon = Icon {
    name: "nb-more",
    view: 16.0,
    body: r#"<circle cx="3.5" cy="8" r="1.2" fill="currentColor"/><circle cx="8" cy="8" r="1.2" fill="currentColor"/><circle cx="12.5" cy="8" r="1.2" fill="currentColor"/>"#,
};

/// What the index view is currently showing.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Shelf {
    #[default]
    All,
    Topic(String),
    Archived,
}

/// Tree node stored inside `TreeModel<NavNode>` in the Obsidian-style sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavNode {
    Topic { slug: String, label: String, count: usize },
    Page { id: i64, title: String, kind: Kind, topic: String, search_blob: String },
}

impl NavNode {
    fn label(&self) -> &str {
        match self {
            NavNode::Topic { label, .. } => label,
            NavNode::Page { title, .. } => title,
        }
    }
}

pub struct Notebook {
    /// Every page (current + archived), newest revision first.
    pub pages: Vec<Page>,
    pub pane: PaneState,
    pub tree: TreeModel<NavNode>,
    pub query: TextEdit,
    pub shelf: Shelf,
    pub kind_filter: Option<Kind>,
    /// The page being read.
    pub open: Option<i64>,
    pub editor: Option<Editor>,
    pub delete_confirm: Option<i64>,
    pub action_menu_open: bool,
    /// Opens the first current page once snapshots arrive (used by `--scene notebook-page`).
    pub auto_open_first: bool,
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook {
            pages: vec![],
            pane: PaneState::new(252.0).range(200.0, 360.0),
            tree: TreeModel::new().key("nb-tree"),
            query: TextEdit::new(""),
            shelf: Shelf::All,
            kind_filter: None,
            open: None,
            editor: None,
            delete_confirm: None,
            action_menu_open: false,
            auto_open_first: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicChoice {
    Auto,
    Selected(String),
    Custom,
}

/// A freeform Markdown note being written or revised on a borderless canvas.
pub struct Editor {
    /// The page being revised, or None for a brand-new note.
    pub id: Option<i64>,
    /// Preserved metadata from existing notes (defaults to `Kind::Note` for fresh notes).
    pub kind: Kind,
    pub tags: Vec<String>,
    pub topic_choice: TopicChoice,
    pub topic_open: bool,
    pub topic_custom: TextEdit,
    pub title: TextEdit,
    pub body: TextEdit,
    pub preview: bool,
    pub error: Option<String>,
}

impl Editor {
    fn blank(default_topic: Option<&str>) -> Editor {
        let mut title = TextEdit::new("");
        title.focused = true;
        Editor {
            id: None,
            kind: Kind::Note,
            tags: Vec::new(),
            topic_choice: match default_topic {
                Some(t) if !t.is_empty() => TopicChoice::Selected(t.to_string()),
                _ => TopicChoice::Auto,
            },
            topic_open: false,
            topic_custom: TextEdit::new(""),
            title,
            body: TextEdit::new(""),
            preview: false,
            error: None,
        }
    }

    fn from_page(p: &Page) -> Editor {
        let mut body = TextEdit::new(&p.body);
        body.focused = true;
        Editor {
            id: Some(p.id),
            kind: p.kind,
            tags: p.tags.clone(),
            topic_choice: TopicChoice::Selected(p.topic.clone()),
            topic_open: false,
            topic_custom: TextEdit::new(""),
            title: TextEdit::new(&p.title),
            body,
            preview: false,
            error: None,
        }
    }

    fn resolved_topic_arg(&self) -> Option<String> {
        match &self.topic_choice {
            TopicChoice::Auto => None,
            TopicChoice::Selected(slug) => Some(slug.clone()),
            TopicChoice::Custom => normalize_topic(&self.topic_custom.text),
        }
    }

    fn draft(&self) -> PageDraft {
        PageDraft {
            kind: self.kind,
            topic: self.resolved_topic_arg(),
            title: self.title.text.clone(),
            tags: self.tags.clone(),
            body: self.body.text.clone(),
        }
    }

    fn is_typing(&self) -> bool {
        self.title.focused || self.topic_custom.focused || self.body.focused
    }
}

impl Notebook {
    pub fn is_typing(&self) -> bool {
        self.query.focused || self.editor.as_ref().is_some_and(Editor::is_typing)
    }

    pub fn page(&self, id: i64) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    fn current(&self) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(|p| !p.is_archived())
    }

    pub fn set_pages(&mut self, pages: Vec<Page>) {
        self.pages = pages;
        if self.auto_open_first {
            self.auto_open_first = false;
            let first_id = if self.open.is_none() { self.current().next().map(|p| p.id) } else { None };
            if let Some(first_id) = first_id {
                self.open = Some(first_id);
            }
        }
        self.sync_tree();
    }

    /// Rebuilds the sidebar `TreeModel` from `self.pages`, keeping prior expand/collapse state.
    pub fn sync_tree(&mut self) {
        let topics = topic_counts(&self.pages);
        let mut was_expanded: HashMap<String, bool> = HashMap::new();
        for ts in &topics {
            let id = format!("topic:{}", ts.topic);
            if self.tree.contains(&id) {
                was_expanded.insert(id.clone(), self.tree.is_expanded(&id));
            }
        }
        self.tree.clear();

        for ts in &topics {
            let folder_id = format!("topic:{}", ts.topic);
            self.tree.insert(None, &folder_id, NavNode::Topic { slug: ts.topic.clone(), label: ts.label.clone(), count: ts.pages });
            let mut topic_pages: Vec<&Page> = self.pages.iter().filter(|p| !p.is_archived() && p.topic == ts.topic).collect();
            topic_pages.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
            for pg in topic_pages {
                let search_blob =
                    format!("{} {} {} {} {}", pg.title, pg.topic, topic_label(&pg.topic), pg.tags.join(" "), pg.body).to_lowercase();
                self.tree.insert(
                    Some(&folder_id),
                    format!("page:{}", pg.id),
                    NavNode::Page { id: pg.id, title: pg.title.clone(), kind: pg.kind, topic: pg.topic.clone(), search_blob },
                );
            }
            let open = was_expanded.get(&folder_id).copied().unwrap_or(true);
            self.tree.set_expanded(&folder_id, open);
        }

        self.apply_tree_filter();
        self.sync_tree_selection();
    }

    pub fn apply_tree_filter(&mut self) {
        let words: Vec<String> = self.query.text.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            self.tree.clear_filter();
        } else {
            self.tree.filter(|node| match node {
                NavNode::Topic { .. } => false,
                NavNode::Page { search_blob, .. } => words.iter().all(|w| search_blob.contains(w.as_str())),
            });
        }
    }

    pub fn sync_tree_selection(&mut self) {
        if let Some(id) = self.open {
            self.tree.select(&format!("page:{id}"));
        } else if let Shelf::Topic(t) = &self.shelf {
            self.tree.select(&format!("topic:{t}"));
        } else {
            self.tree.clear_selection();
        }
    }

    /// Pages visible in the active index view after shelf, kind, and query filters.
    pub fn visible(&self) -> Vec<&Page> {
        let on_shelf = |p: &&Page| -> bool {
            let shelf_ok = match &self.shelf {
                Shelf::All => !p.is_archived(),
                Shelf::Topic(t) => !p.is_archived() && p.topic == *t,
                Shelf::Archived => p.is_archived(),
            };
            if !shelf_ok {
                return false;
            }
            if let Some(k) = self.kind_filter
                && p.kind != k
            {
                return false;
            }
            true
        };
        let words: Vec<String> = self.query.text.split_whitespace().map(str::to_lowercase).collect();
        let mut scored: Vec<(usize, &Page)> =
            self.pages.iter().filter(on_shelf).filter_map(|p| score(p, &words).map(|sc| (sc, p))).collect();
        if !words.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
        }
        scored.into_iter().map(|(_, p)| p).collect()
    }
}

/// How well a page matches every search word (title counts most), or None when a word is missing.
fn score(p: &Page, words: &[String]) -> Option<usize> {
    let title = p.title.to_lowercase();
    let topic_slug = p.topic.to_lowercase();
    let topic_name = topic_label(&p.topic).to_lowercase();
    let body = p.body.to_lowercase();
    words.iter().try_fold(0, |acc, w| {
        let hit = if title.contains(w.as_str()) {
            6
        } else if topic_slug.contains(w.as_str()) || topic_name.contains(w.as_str()) {
            5
        } else if p.tags.iter().any(|t| t.contains(w.as_str())) {
            4
        } else if body.contains(w.as_str()) {
            2
        } else {
            return None;
        };
        Some(acc + hit)
    })
}

/// Each kind's muted semantic ink dot/accent, legible in both light and dark modes.
pub fn kind_color(kind: Kind, p: &Palette) -> Color {
    match kind {
        Kind::Note => p.text_3,
        Kind::Fix => {
            if p.dark {
                oklch(0.76, 0.13, 155.0)
            } else {
                oklch(0.53, 0.13, 155.0)
            }
        }
        Kind::Pitfall => {
            if p.dark {
                oklch(0.76, 0.14, 32.0)
            } else {
                oklch(0.53, 0.16, 32.0)
            }
        }
        Kind::Pattern => {
            if p.dark {
                oklch(0.76, 0.125, 252.0)
            } else {
                oklch(0.53, 0.14, 252.0)
            }
        }
        Kind::Decision => {
            if p.dark {
                oklch(0.76, 0.12, 300.0)
            } else {
                oklch(0.53, 0.13, 300.0)
            }
        }
    }
}

impl State {
    fn notebook_send(&self, op: NotebookOp) {
        if let Some(w) = &self.watcher {
            w.notebook(op);
        }
    }

    pub fn notebook_new_page(&mut self) {
        let default_topic = match &self.notebook.shelf {
            Shelf::Topic(t) => Some(t.as_str()),
            _ => None,
        };
        self.notebook.open = None;
        self.notebook.delete_confirm = None;
        self.notebook.action_menu_open = false;
        self.notebook.editor = Some(Editor::blank(default_topic));
    }

    pub fn notebook_edit(&mut self, id: i64) {
        if let Some(page) = self.notebook.page(id).cloned() {
            self.notebook.editor = Some(Editor::from_page(&page));
            self.notebook.delete_confirm = None;
            self.notebook.action_menu_open = false;
        }
    }

    pub fn notebook_open_page(&mut self, id: i64) {
        self.notebook.open = Some(id);
        self.notebook.editor = None;
        self.notebook.delete_confirm = None;
        self.notebook.action_menu_open = false;
        if let Some(page) = self.notebook.page(id).cloned()
            && !page.is_archived()
        {
            self.notebook.tree.expand_to(&format!("page:{id}"));
        }
        self.notebook.sync_tree_selection();
    }

    pub fn notebook_toggle_task(&mut self, page_id: i64, task_index: usize, checked: bool) {
        if let Some(page) = self.notebook.pages.iter_mut().find(|p| p.id == page_id)
            && let Some(updated) = toggle_markdown_task(&page.body, task_index, checked)
        {
            page.body = updated;
        }
        self.notebook_send(NotebookOp::ToggleTask(page_id, task_index, checked));
    }

    fn notebook_select_tree_node(&mut self, id: &str) {
        if let Some(slug) = id.strip_prefix("topic:") {
            self.notebook.shelf = Shelf::Topic(slug.to_string());
            self.notebook.kind_filter = None;
            self.notebook.open = None;
            self.notebook.editor = None;
            self.notebook.delete_confirm = None;
            self.notebook.action_menu_open = false;
            self.notebook.sync_tree_selection();
        } else if let Some(pid) = id.strip_prefix("page:").and_then(|s| s.parse::<i64>().ok()) {
            self.notebook_open_page(pid);
        }
    }

    /// Checks the editor's note and saves it, or surfaces what's missing inline.
    pub fn notebook_save(&mut self) {
        let Some(editor) = self.notebook.editor.as_mut() else { return };
        let draft = match editor.draft().checked() {
            Ok(d) => d,
            Err(e) => {
                editor.error = Some(sentence_case(&e.to_string().replace('`', "")));
                return;
            }
        };
        let (title, id) = (draft.title.clone(), editor.id);
        match id {
            Some(id) => {
                self.notebook_send(NotebookOp::Revise(id, draft));
                self.notebook.open = Some(id);
            }
            None => self.notebook_send(NotebookOp::Create(draft)),
        }
        self.notebook.editor = None;
        let verb = if id.is_some() { "Saved" } else { "Created" };
        self.push_toast(
            "notebook-saved",
            ToastItem { title: format!("{verb} \"{title}\""), description: None, kind: ToastKind::Info },
            Some(2.5),
        );
    }

    /// Esc inside the notebook unwinds overlays -> editor -> delete confirmation -> open page -> search query.
    pub fn notebook_escape(&mut self) -> bool {
        let nb = &mut self.notebook;
        if nb.action_menu_open {
            nb.action_menu_open = false;
            return true;
        }
        if let Some(ed) = nb.editor.as_mut()
            && ed.topic_open
        {
            ed.topic_open = false;
            return true;
        }
        if nb.editor.take().is_some() || nb.delete_confirm.take().is_some() || nb.open.take().is_some() {
            nb.sync_tree_selection();
            return true;
        }
        if !nb.query.text.is_empty() {
            nb.query = TextEdit::new("");
            nb.apply_tree_filter();
            return true;
        }
        false
    }

    fn notebook_shelf(&mut self, shelf: Shelf) {
        self.notebook.shelf = shelf;
        self.notebook.open = None;
        self.notebook.editor = None;
        self.notebook.delete_confirm = None;
        self.notebook.action_menu_open = false;
        self.notebook.sync_tree_selection();
    }
}

/// The open editor's field `pick` chooses, or the scratch field once the editor has closed.
fn editor_field(s: &mut State, pick: fn(&mut Editor) -> &mut TextEdit) -> &mut TextEdit {
    match s.notebook.editor.as_mut() {
        Some(ed) => pick(ed),
        None => &mut s.scratch_edit,
    }
}

fn sentence_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

pub fn view(s: &State, p: &Palette, k: &Kit, inset: f32, reduced_motion: bool, layer: &mut Vec<El>) -> El {
    let nb = &s.notebook;
    let body = if let Some(editor) = &nb.editor {
        editor_view(nb, editor, p, k, layer)
    } else if let Some(page) = nb.open.and_then(|id| nb.page(id)) {
        page_view(s, page, p, k, layer)
    } else {
        index_view(s, p, k)
    };

    let viewport = col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(col().w_full().items_center().px(40.0).pt(24.0).pb(80.0).child(body))
        .scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(16.0);
    let main_area = scroll_bar(k, "notebook-scroll-area", "notebook-scroll", viewport).w_full().grow(1.0).min_h(0.0);

    let toolbar = Toolbar::new().leading(toolbar_breadcrumb(nb, p, k)).trailing(chrome::tabs(s, k)).trailing(chrome::menu(s, p, k, layer));

    SplitView::new(main_area)
        .sidebar(&nb.pane, |s: &mut State| &mut s.notebook.pane, sidebar_view(s, p, k))
        .toolbar(toolbar)
        .titlebar_inset(inset)
        .frame_inset(style::PANE_INSET)
        .sidebar_toggle(SidebarToggle::new().in_header().past_lights(10.0))
        .sidebar_peek(SidebarPeek::new())
        .reduced_motion(reduced_motion)
        .build(&style::pane_kit(k))
}

/// A small button's inline padding, so a plain crumb's label sits where a pressable one's does.
const CRUMB_PAD: f32 = 10.0;

fn toolbar_breadcrumb(nb: &Notebook, p: &Palette, k: &Kit) -> El {
    let sep = || text("/").font(style::META).color(p.text_3).px(2.0);
    let crumb_btn = |key: &str, label: &str, active: bool| {
        let btn = Button::new(label.to_string())
            .variant(if active { ButtonVariant::Ghost } else { ButtonVariant::GhostMuted })
            .control_size(ControlSize::Small);
        btn.build(k).key(key.to_string())
    };

    let mut row_el = row().key("nb-breadcrumb").gap(2.0).center_y().shrink().min_w(0.0);
    if let Some(ed) = &nb.editor {
        let title_crumb = if ed.title.text.trim().is_empty() {
            if ed.id.is_some() { "Edit note".to_string() } else { "New note".to_string() }
        } else {
            ed.title.text.trim().to_string()
        };
        row_el = row_el
            .child(crumb_btn("nb-bc-all", "Notebook", false).on_press(|s: &mut State| {
                s.notebook.editor = None;
                s.notebook_shelf(Shelf::All);
            }))
            .child(sep())
            .child(text(title_crumb).font(k.ty.ui_compact).color(p.text).truncate().max_w(260.0).pl(CRUMB_PAD));
        return row_el;
    }

    if let Some(page) = nb.open.and_then(|id| nb.page(id)) {
        let slug = page.topic.clone();
        let t_label = topic_label(&slug).to_string();
        let target_shelf = if page.is_archived() { Shelf::Archived } else { Shelf::Topic(slug.clone()) };
        let parent_name = if page.is_archived() { "Archived".to_string() } else { t_label };
        return row_el
            .child(crumb_btn("nb-bc-root", "Notebook", false).on_press(|s: &mut State| s.notebook_shelf(Shelf::All)))
            .child(sep())
            .child(crumb_btn("nb-bc-topic", &parent_name, false).on_press(move |s: &mut State| s.notebook_shelf(target_shelf.clone())))
            .child(sep())
            .child(text(page.title.clone()).font(k.ty.ui_compact).color(p.text).truncate().max_w(260.0).shrink().min_w(0.0).pl(CRUMB_PAD));
    }

    match &nb.shelf {
        Shelf::All => row_el.child(text("Notebook").font(k.ty.ui_medium).color(p.text).pl(CRUMB_PAD)),
        Shelf::Topic(slug) => row_el
            .child(crumb_btn("nb-bc-all", "Notebook", false).on_press(|s: &mut State| s.notebook_shelf(Shelf::All)))
            .child(sep())
            .child(text(topic_label(slug).to_string()).font(k.ty.ui_medium).color(p.text).truncate().pl(CRUMB_PAD)),
        Shelf::Archived => row_el
            .child(crumb_btn("nb-bc-all", "Notebook", false).on_press(|s: &mut State| s.notebook_shelf(Shelf::All)))
            .child(sep())
            .child(text("Archived").font(k.ty.ui_medium).color(p.text).pl(CRUMB_PAD)),
    }
}

fn sidebar_view(s: &State, p: &Palette, k: &Kit) -> El {
    let nb = &s.notebook;
    let current_count = nb.current().count();
    let topics = topic_counts(&nb.pages);
    let archived_count = nb.pages.iter().filter(|pg| pg.is_archived()).count();
    let query = nb.query.text.trim().to_string();

    let mut search_input = TextInput::new(&nb.query)
        .placeholder("Search notebook")
        .label("Search the notebook")
        .leading_icon(k.glyph.search)
        .width(None)
        .on_edit(|s: &mut State| &mut s.notebook.query)
        .on_change(|s: &mut State, _| s.notebook.apply_tree_filter())
        .on_action(|s: &mut State, _| {
            s.notebook.query = TextEdit::new("");
            s.notebook.apply_tree_filter();
        })
        .on_key(|s: &mut State, e: &KeyEvent, _| matches!(e.key, Key::Escape) && s.notebook_escape());
    if !nb.query.text.is_empty() {
        search_input = search_input.clear_button();
    }
    let search = search_input.build(k).key("nb-search");

    let new_page = Button::new("New Note")
        .icon(&PLUS_ICON)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook_new_page())
        .build(k)
        .w_full()
        .key("nb-new");

    let all_selected = nb.editor.is_none() && nb.open.is_none() && nb.shelf == Shelf::All;
    let all_row = SidebarRow::new("All notes")
        .badge(current_count.to_string())
        .selected(all_selected)
        .on_select(|s: &mut State, _| s.notebook_shelf(Shelf::All))
        .build(k)
        .key("nb-shelf-all");

    let header = col().key("nb-sidebar-header").w_full().px(10.0).pt(4.0).pb(8.0).gap(8.0).child(search).child(new_page).child(all_row);

    let mut topics_head = SidebarHeader::new("Categories");
    if !topics.is_empty() {
        topics_head = topics_head.count(topics.len());
    }

    let mut sb =
        col().key("nb-sidebar").w_full().h_full().min_h(0.0).child(header).child(el().w_full().px(8.0).child(topics_head.build(k)));

    if nb.tree.is_empty() {
        sb = sb.child(
            col()
                .w_full()
                .grow(1.0)
                .px(16.0)
                .py(12.0)
                .child(text("Categories appear as you or agents add notes.").font(style::META).color(p.text_3).wrap()),
        );
    } else {
        let palette = *p;
        let q = query.clone();
        let mut tree_style = TreeStyle::from_kit(k).chevron_only_folders(true).guides_on_hover(0.55);
        tree_style.row_height = 26.0;
        let tree_el = Tree::new(|s: &State| &s.notebook.tree, |s: &mut State| &mut s.notebook.tree, NavNode::label)
            .name("Notebook categories")
            .style(tree_style)
            .row(move |node, _row: &TreeRow| match node {
                NavNode::Topic { label, count, .. } => {
                    let mut item = TreeItem::new(label).badge(count.to_string());
                    if !q.is_empty() {
                        item = item.highlight(&q);
                    }
                    item
                }
                NavNode::Page { title, kind, .. } => {
                    let mut item = TreeItem::new(title);
                    if *kind != Kind::Note {
                        item = item.dot(kind_color(*kind, &palette));
                    }
                    if !q.is_empty() {
                        item = item.highlight(&q);
                    }
                    item
                }
            })
            .on_select(|s: &mut State, id, _| s.notebook_select_tree_node(id))
            .on_open(|s: &mut State, id, _| s.notebook_select_tree_node(id))
            .build(s, k)
            .grow(1.0)
            .min_h(0.0)
            .px(6.0);
        sb = sb.child(tree_el);
    }

    if archived_count > 0 {
        let arch_selected = nb.editor.is_none() && nb.open.is_none() && nb.shelf == Shelf::Archived;
        let footer = col().key("nb-sidebar-footer").w_full().px(8.0).py(8.0).gap(6.0).child(Divider::horizontal().build(k)).child(
            SidebarRow::new("Archived")
                .badge(archived_count.to_string())
                .selected(arch_selected)
                .on_select(|s: &mut State, _| s.notebook_shelf(Shelf::Archived))
                .build(k)
                .key("nb-shelf-archived"),
        );
        sb = sb.child(footer);
    }
    sb
}

fn index_view(s: &State, p: &Palette, k: &Kit) -> El {
    let nb = &s.notebook;
    if nb.pages.is_empty() && s.loaded {
        return col().w_full().max_w(INDEX_MAX_W).child(empty_notebook(p, k));
    }

    let heading = match &nb.shelf {
        Shelf::All => "Notebook".to_string(),
        Shelf::Topic(slug) => topic_label(slug).to_string(),
        Shelf::Archived => "Archived".to_string(),
    };

    let pages = nb.visible();

    // Only show the kind filter bar when the library has tagged kinds beyond plain Notes.
    let present_kinds: Vec<Kind> = Kind::ALL.iter().copied().filter(|&kind| nb.pages.iter().any(|pg| pg.kind == kind)).collect();
    let show_kind_filter = present_kinds.len() > 1 || nb.kind_filter.is_some();

    let mut header = col().key("nb-header").w_full().pb(16.0).gap(16.0).child(text(heading).font(style::DISPLAY).color(p.text));
    if show_kind_filter {
        let mut kind_labels = vec!["All"];
        for kind in &present_kinds {
            kind_labels.push(kind.plural());
        }
        let selected_kind_idx = match nb.kind_filter {
            None => Some(0),
            Some(kind) => present_kinds.iter().position(|&x| x == kind).map(|i| i + 1),
        };
        let pick_kinds = present_kinds.clone();
        let kind_control = SegmentedControl::new(&kind_labels, selected_kind_idx)
            .label("Filter by kind")
            .fit()
            .on_pick(move |s: &mut State, i: usize, _: &mut Tx| {
                s.notebook.kind_filter = if i == 0 { None } else { pick_kinds.get(i - 1).copied() };
            })
            .build(k);
        header = header.child(kind_control);
    }

    let mut list = col().key("nb-index").w_full();
    if pages.is_empty() {
        list = list.child(
            col()
                .w_full()
                .py(36.0)
                .gap(8.0)
                .child(text("No matching notes").font(style::NAME).color(p.text))
                .child(text("Clear a filter or search for fewer words.").font(style::META).color(p.text_3)),
        );
    } else {
        for (i, page) in pages.into_iter().enumerate() {
            if i > 0 {
                list = list.child(Divider::horizontal().build(k));
            }
            list = list.child(index_row(s, page, p, k));
        }
    }

    col().w_full().max_w(INDEX_MAX_W).child(header).child(Divider::horizontal().build(k)).child(list)
}

fn index_row(s: &State, page: &Page, p: &Palette, k: &Kit) -> El {
    let id = page.id;
    let show_topic = !matches!(s.notebook.shelf, Shelf::Topic(_));
    let excerpt = page.excerpt(160);

    let mut meta_top = row().w_full().gap(8.0).center_y();
    if show_topic {
        meta_top = meta_top.child(Badge::new(topic_label(&page.topic)).variant(BadgeVariant::Subtle).build(k));
    }
    if page.kind != Kind::Note {
        let ink = kind_color(page.kind, p);
        meta_top = meta_top
            .child(el().size(6.0, 6.0).radius(3.0).fill(ink))
            .child(text(page.kind.label().to_string()).font(style::META).color(ink));
    }
    meta_top = meta_top.child(rune::spacer()).child(text(ago(page.updated_at, s.now)).font(style::META).color(p.text_3));

    let mut card = col()
        .key(format!("nb-page-{id}"))
        .role("button")
        .label(page.title.clone())
        .focusable(true)
        .mx(-ROW_BLEED)
        .px(ROW_BLEED)
        .py(15.0)
        .gap(6.0)
        .radius(8.0)
        .hover(p.hover)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.notebook_open_page(id))
        .child(meta_top)
        .child(text(page.title.clone()).font(style::SECTION_TITLE).color(p.text).wrap());
    if !excerpt.is_empty() {
        card = card.child(text(excerpt).font(style::BODY).color(p.text_2).wrap().max_lines(2));
    }
    card.child(byline(s, page, p, 12.0).mt(2.0))
}

/// "Claude Code · Feeder · helped 3×": author, source project, and usefulness count.
fn byline(s: &State, page: &Page, p: &Palette, mark: f32) -> El {
    let mut r = row().gap(6.0).center_y().child(agents::mark(&page.agent, p, mark, s.agent_icon(&page.agent)));
    r = r.child(text(page.agent.clone()).font(style::META).color(p.text_2));
    if let Some(project) = page.project_id.and_then(|pid| s.summary(pid)) {
        r = r
            .child(text("·").font(style::META).color(p.text_3))
            .child(orb::orb(&project.project.orb, mark))
            .child(text(project.project.name.clone()).font(style::META).color(p.text_2));
    }
    if page.helped > 0 {
        r = r
            .child(text("·").font(style::META).color(p.text_3))
            .child(text(format!("helped {}×", page.helped)).font(style::META).color(p.text_3));
    }
    r
}

fn page_view(s: &State, page: &Page, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let id = page.id;
    let project = page.project_id.and_then(|pid| s.summary(pid)).map(|sum| sum.project.name.clone());
    let markdown = page.to_markdown(project.as_deref(), s.now);
    let copied = s.is_copied(CopyTarget::NotebookPage(id));

    let back_a11y = if page.is_archived() { "Back to Archived".to_string() } else { format!("Back to {}", topic_label(&page.topic)) };
    let back_shelf = if page.is_archived() { Shelf::Archived } else { Shelf::Topic(page.topic.clone()) };

    let copy_btn = Button::new(if copied { "Copied" } else { "Copy" })
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help("Copy note as Markdown")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            tx.copy(markdown.clone());
            s.mark_copied(CopyTarget::NotebookPage(id));
        })
        .build(k)
        .key("nb-copy");

    let helped_label = if page.helped > 0 { format!("Helped ({})", page.helped) } else { "Helped".to_string() };
    let helped_btn = Button::new(helped_label)
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help("Mark that this note helped you")
        .on_press(move |s: &mut State| s.notebook_send(NotebookOp::Helped(id)))
        .build(k)
        .key("nb-helped");

    let edit_btn = Button::new("Edit")
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Regular)
        .on_press(move |s: &mut State| s.notebook_edit(id))
        .build(k)
        .key("nb-edit");

    let (archive_label, standing) =
        if page.is_archived() { ("Restore Note", Standing::Current) } else { ("Archive Note", Standing::Archived) };
    let more_trigger = row()
        .key("nb-more-btn")
        .role("button")
        .label("Note actions")
        .focusable(true)
        .size(28.0, 24.0)
        .center()
        .radius(6.0)
        .hover(k.c.hover)
        .fill(if s.notebook.action_menu_open { k.c.pressed } else { Color::TRANSPARENT })
        .cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.notebook.action_menu_open = !s.notebook.action_menu_open)
        .child(icon(&MORE_ICON, 15.0).color(k.c.icon_muted));

    let more_menu = Menu::new("nb-page-menu")
        .trigger(more_trigger)
        .open(s.notebook.action_menu_open)
        .placement(Placement::BottomEnd)
        .min_width(180.0)
        .on_close(|s: &mut State| s.notebook.action_menu_open = false)
        .item(MenuItem::new(archive_label).on_select(move |s: &mut State| {
            s.notebook.action_menu_open = false;
            s.notebook_send(NotebookOp::SetStanding(id, standing));
            s.notebook.open = None;
            s.notebook.sync_tree_selection();
        }))
        .separator()
        .item(MenuItem::new("Delete Note…").danger(true).on_select(move |s: &mut State| {
            s.notebook.action_menu_open = false;
            s.notebook.delete_confirm = Some(id);
        }))
        .build(k, layer);

    let actions = row().gap(6.0).center_y().child(copy_btn).child(helped_btn).child(edit_btn).child(more_menu);

    let top_bar = row()
        .w_full()
        .pb(20.0)
        .center_y()
        .child(
            text("Back")
                .key("nb-back")
                .font(style::CONTROL)
                .color(p.text_2)
                .link(Link::new(0.."Back".len(), "back").label(back_a11y).hover(LinkPaint::new().color(p.text).underline(p.text_3)))
                .on_link(move |s: &mut State, _: &str, _: &mut Tx| s.notebook_shelf(back_shelf.clone())),
        )
        .child(rune::spacer())
        .child(actions);

    let mut doc = col().key(format!("nb-sheet-{id}")).w_full().max_w(DOC_MAX_W).transition(Transition::opacity()).child(top_bar);

    if s.notebook.delete_confirm == Some(id) {
        let confirm = row()
            .key("nb-delete-confirm")
            .w_full()
            .mb(18.0)
            .px(14.0)
            .py(10.0)
            .gap(10.0)
            .center_y()
            .radius(8.0)
            .fill(k.c.layer_01)
            .border(1.0, k.c.danger.border)
            .child(text("Delete this note permanently?").font(style::BODY).color(p.text))
            .child(rune::spacer())
            .child(
                Button::new("Keep")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press(|s: &mut State| s.notebook.delete_confirm = None)
                    .build(k),
            )
            .child(
                Button::new("Delete Note")
                    .variant(ButtonVariant::Danger)
                    .control_size(ControlSize::Regular)
                    .on_press(move |s: &mut State| {
                        s.notebook_send(NotebookOp::Delete(id));
                        s.notebook.delete_confirm = None;
                        s.notebook.open = None;
                        s.notebook.sync_tree_selection();
                    })
                    .build(k),
            );
        doc = doc.child(confirm);
    }

    // Spacious Notion/Obsidian article header: title + single quiet metadata line.
    let mut title_line = col().w_full().gap(8.0).child(text(page.title.clone()).font(style::TITLE).color(p.text).wrap());
    if page.is_archived() {
        title_line = title_line.child(row().child(Badge::new("Archived").variant(BadgeVariant::Neutral).standard().build(k)));
    }
    doc = doc.child(title_line);

    let topic_slug = page.topic.clone();
    let topic_pill = row()
        .key("nb-prop-topic")
        .role("button")
        .label(format!("Open category {}", topic_label(&topic_slug)))
        .h(22.0)
        .px(8.0)
        .center_y()
        .radius(6.0)
        .fill(k.c.layer_02)
        .hover(k.c.hover)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.notebook_shelf(Shelf::Topic(topic_slug.clone())))
        .child(text(topic_label(&page.topic).to_string()).font(style::META).color(p.text));

    let revised = if page.revision > 1 {
        format!("rev {} · updated {}", page.revision, ago(page.updated_at, s.now))
    } else {
        ago(page.created_at, s.now)
    };

    let mut meta = row().key("nb-meta").w_full().mt(12.0).gap(8.0).center_y().flex_wrap().child(topic_pill);
    if page.kind != Kind::Note {
        let ink = kind_color(page.kind, p);
        meta = meta.child(
            row()
                .gap(5.0)
                .center_y()
                .child(el().size(6.0, 6.0).radius(3.0).fill(ink))
                .child(text(page.kind.label().to_string()).font(style::META).color(ink)),
        );
    }
    meta = meta
        .child(text("·").font(style::META).color(p.text_3))
        .child(byline(s, page, p, 12.0))
        .child(text("·").font(style::META).color(p.text_3))
        .child(text(revised).font(style::META).color(p.text_3));

    if !page.tags.is_empty() {
        let page_topic = page.topic.clone();
        for tag in &page.tags {
            let t = tag.clone();
            let topic_for_tag = page_topic.clone();
            meta = meta.child(
                row()
                    .key(format!("nb-chip-{tag}"))
                    .role("button")
                    .label(format!("Search notebook for {tag}"))
                    .h(20.0)
                    .px(7.0)
                    .center_y()
                    .radius(5.0)
                    .fill(k.c.layer_01)
                    .border(1.0, k.c.border_muted)
                    .hover(k.c.hover)
                    .cursor(Cursor::Pointer)
                    .on_press(move |s: &mut State| {
                        s.notebook_shelf(Shelf::Topic(topic_for_tag.clone()));
                        s.notebook.query = TextEdit::new(&t);
                        s.notebook.apply_tree_filter();
                    })
                    .child(text(format!("#{tag}")).font(style::META).color(p.text_3)),
            );
        }
    }

    doc = doc.child(meta).child(Divider::horizontal().build(k).mt(18.0));

    // Freeform Markdown article body with callouts, tables, syntax highlighting, and interactive tasks.
    doc = doc.child(render_article_markdown(&page.body, Some(id), p, k).mt(24.0));

    let related = related_pages(&s.notebook, page);
    if !related.is_empty() {
        let mut rel = col().w_full().mt(36.0).gap(4.0).child(text("Related").font(style::HEADING).color(p.text_2).mb(6.0));
        for r in related {
            let rid = r.id;
            let mut row_item = row()
                .key(format!("nb-related-{rid}"))
                .role("button")
                .label(r.title.clone())
                .w_full()
                .px(8.0)
                .py(8.0)
                .gap(10.0)
                .center_y()
                .radius(6.0)
                .hover(p.hover)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| s.notebook_open_page(rid));
            if r.kind != Kind::Note {
                row_item = row_item.child(el().size(6.0, 6.0).radius(3.0).fill(kind_color(r.kind, p)));
            }
            row_item = row_item
                .child(text(r.title.clone()).font(style::NAME).color(p.text).truncate().grow(1.0).min_w(0.0))
                .child(Badge::new(topic_label(&r.topic)).variant(BadgeVariant::Subtle).build(k));
            rel = rel.child(row_item);
        }
        doc = doc.child(Divider::horizontal().build(k).mt(36.0)).child(rel);
    }

    doc
}

/// Up to three current pages related by shared tags or matching category.
fn related_pages<'a>(nb: &'a Notebook, page: &Page) -> Vec<&'a Page> {
    let mut scored: Vec<(usize, &Page)> = nb
        .current()
        .filter(|p| p.id != page.id)
        .map(|p| {
            let shared_tags = p.tags.iter().filter(|t| page.tags.contains(t)).count();
            let same_topic = usize::from(p.topic == page.topic);
            (shared_tags * 3 + same_topic, p)
        })
        .filter(|(sc, _)| *sc > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.helped.cmp(&a.1.helped)).then(b.1.updated_at.cmp(&a.1.updated_at)));
    scored.into_iter().take(3).map(|(_, p)| p).collect()
}

// ── Rich Article Renderer (GFM + Obsidian Callouts + Interactive Tasks) ──

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalloutKind {
    Note,
    Tip,
    Warning,
    Important,
    Caution,
}

impl CalloutKind {
    fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "note" | "info" => Some(Self::Note),
            "tip" | "hint" | "takeaway" => Some(Self::Tip),
            "warning" | "warn" => Some(Self::Warning),
            "important" => Some(Self::Important),
            "caution" | "danger" | "bug" => Some(Self::Caution),
            _ => None,
        }
    }

    fn default_title(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Warning => "Warning",
            Self::Important => "Important",
            Self::Caution => "Caution",
        }
    }

    fn accent(self, p: &Palette) -> Color {
        let (hue, light_c, dark_c) = match self {
            Self::Note => (252.0, 0.14, 0.13),
            Self::Tip => (155.0, 0.13, 0.13),
            Self::Warning => (55.0, 0.15, 0.14),
            Self::Important => (300.0, 0.13, 0.125),
            Self::Caution => (26.0, 0.17, 0.15),
        };
        if p.dark { oklch(0.76, dark_c, hue) } else { oklch(0.52, light_c, hue) }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ArticleChunk {
    Markdown { text: String, task_offset: usize },
    Callout { kind: CalloutKind, title: String, body: String, task_offset: usize },
}

fn is_task_list_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("+ ")) else {
        return false;
    };
    rest.starts_with("[ ] ") || rest.starts_with("[x] ") || rest.starts_with("[X] ")
}

fn count_tasks(md: &str) -> usize {
    let mut in_fence = false;
    let mut count = 0;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence && is_task_list_line(line) {
            count += 1;
        }
    }
    count
}

fn parse_callout_header(line: &str) -> Option<(CalloutKind, String)> {
    let rest = line.trim_start().strip_prefix('>')?.trim_start();
    let after_open = rest.strip_prefix("[!")?;
    let end = after_open.find(']')?;
    let tag = &after_open[..end];
    let kind = CalloutKind::parse(tag)?;
    let custom = after_open[end + 1..].trim();
    let title = if custom.is_empty() { kind.default_title().to_string() } else { custom.to_string() };
    Some((kind, title))
}

fn split_article_chunks(src: &str) -> Vec<ArticleChunk> {
    let lines: Vec<&str> = src.lines().collect();
    let mut chunks = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut task_offset = 0usize;
    let mut i = 0;

    let flush_md = |buf: &mut Vec<&str>, chunks: &mut Vec<ArticleChunk>, task_offset: &mut usize| {
        while buf.first().is_some_and(|l| l.trim().is_empty()) {
            buf.remove(0);
        }
        while buf.last().is_some_and(|l| l.trim().is_empty()) {
            buf.pop();
        }
        if buf.is_empty() {
            return;
        }
        let text = buf.join("\n");
        buf.clear();
        let n = count_tasks(&text);
        chunks.push(ArticleChunk::Markdown { text, task_offset: *task_offset });
        *task_offset += n;
    };

    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            buf.push(line);
            i += 1;
            continue;
        }
        if !in_fence && let Some((kind, title)) = parse_callout_header(line) {
            flush_md(&mut buf, &mut chunks, &mut task_offset);
            let mut body_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                if let Some(after) = lines[i].trim_start().strip_prefix('>') {
                    body_lines.push(after.strip_prefix(' ').unwrap_or(after));
                    i += 1;
                } else {
                    break;
                }
            }
            let body = body_lines.join("\n").trim().to_string();
            let n = count_tasks(&body);
            chunks.push(ArticleChunk::Callout { kind, title, body, task_offset });
            task_offset += n;
            continue;
        }
        buf.push(line);
        i += 1;
    }
    flush_md(&mut buf, &mut chunks, &mut task_offset);
    chunks
}

fn render_article_markdown(src: &str, page_id: Option<i64>, p: &Palette, k: &Kit) -> El {
    let md_style = feed::markdown_style(p, k).body_font(style::PROSE);
    let chunks = split_article_chunks(src);
    if chunks.is_empty() {
        return el().w_full();
    }
    let mut stack = col().w_full().gap(14.0);
    for (idx, chunk) in chunks.into_iter().enumerate() {
        match chunk {
            ArticleChunk::Markdown { text, task_offset } => {
                let mut md = Markdown::new(text).selectable(true);
                if let Some(pid) = page_id {
                    md = md.on_task(move |s: &mut State, _key, local_idx, checked| {
                        s.notebook_toggle_task(pid, task_offset + local_idx, checked);
                    });
                } else {
                    md = md.tasks_read_only();
                }
                stack = stack.child(md.render(&md_style).key(format!("nb-md-{idx}")));
            }
            ArticleChunk::Callout { kind, title, body, task_offset } => {
                let accent = kind.accent(p);
                let mut inner =
                    col().grow(1.0).min_w(0.0).pl(12.0).pr(16.0).py(12.0).gap(6.0).child(text(title).font(style::CONTROL).color(accent));
                if !body.is_empty() {
                    let mut md = Markdown::new(body).selectable(true);
                    if let Some(pid) = page_id {
                        md = md.on_task(move |s: &mut State, _key, local_idx, checked| {
                            s.notebook_toggle_task(pid, task_offset + local_idx, checked);
                        });
                    } else {
                        md = md.tasks_read_only();
                    }
                    inner = inner.child(md.render(&md_style));
                }
                stack = stack.child(
                    row()
                        .key(format!("nb-callout-{idx}"))
                        .w_full()
                        .radius(8.0)
                        .fill(k.c.layer_01)
                        .border(1.0, k.c.border_muted)
                        .items_stretch()
                        .child(el().w(3.0).my(10.0).ml(12.0).radius(1.5).fill(accent))
                        .child(inner),
                );
            }
        }
    }
    stack
}

// ── Borderless Notion/Obsidian Freeform Editor ──

#[derive(Clone, Copy)]
enum MdAction {
    Heading(usize),
    Wrap(&'static str, &'static str, &'static str),
    Link,
    LinePrefix(&'static str),
    Callout,
    CodeBlock,
    Table,
    Rule,
}

fn editor_view(nb: &Notebook, ed: &Editor, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let mode_toggle = SegmentedControl::new(&["Write", "Preview"], Some(usize::from(ed.preview)))
        .label("Editor mode")
        .fit()
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            if let Some(ed) = s.notebook.editor.as_mut() {
                ed.preview = i == 1;
            }
        })
        .build(k);

    let cancel = Button::new("Cancel")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook.editor = None)
        .build(k);

    let save = Button::new(if ed.id.is_some() { "Save" } else { "Save Note" })
        .variant(ButtonVariant::Contrast)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook_save())
        .build(k);

    // Single minimal category selector — borderless inline pill right on the top row.
    let mut topic_options: Vec<(String, TopicChoice)> = Vec::new();
    if ed.id.is_none() {
        let inferred = infer_topic(&ed.tags, &ed.title.text, &ed.body.text);
        topic_options.push((format!("Auto ({})", topic_label(&inferred)), TopicChoice::Auto));
    }
    for spec in CANONICAL_TOPICS {
        topic_options.push((spec.label.to_string(), TopicChoice::Selected(spec.slug.to_string())));
    }
    for ts in topic_counts(&nb.pages) {
        if !CANONICAL_TOPICS.iter().any(|c| c.slug == ts.topic) {
            topic_options.push((ts.label, TopicChoice::Selected(ts.topic)));
        }
    }
    if let TopicChoice::Selected(slug) = &ed.topic_choice
        && !topic_options.iter().any(|(_, c)| matches!(c, TopicChoice::Selected(s) if s == slug))
    {
        topic_options.push((topic_label(slug).to_string(), TopicChoice::Selected(slug.clone())));
    }
    topic_options.push(("New category…".to_string(), TopicChoice::Custom));

    let option_labels: Vec<String> = topic_options.iter().map(|(l, _)| l.clone()).collect();
    let current_topic_label = topic_options
        .iter()
        .find(|(_, choice)| *choice == ed.topic_choice)
        .map(|(l, _)| l.clone())
        .unwrap_or_else(|| "New category…".to_string());

    let map_opts = topic_options.clone();
    let topic_select = Select::new("nb-ed-topic", &option_labels)
        .appearance(SelectAppearance::Inline)
        .width(190.0)
        .label("Category")
        .current(Some(&current_topic_label))
        .open(ed.topic_open)
        .placement(Placement::BottomStart)
        .on_toggle(|s: &mut State| {
            if let Some(ed) = s.notebook.editor.as_mut() {
                ed.topic_open = !ed.topic_open;
            }
        })
        .on_close(|s: &mut State| {
            if let Some(ed) = s.notebook.editor.as_mut() {
                ed.topic_open = false;
            }
        })
        .on_choose(move |s: &mut State, chosen: &str, _| {
            if let Some(ed) = s.notebook.editor.as_mut() {
                if let Some((_, choice)) = map_opts.iter().find(|(l, _)| l == chosen) {
                    ed.topic_choice = choice.clone();
                    if matches!(choice, TopicChoice::Custom) {
                        ed.topic_custom.focused = true;
                    }
                }
                ed.topic_open = false;
            }
        })
        .build(k, layer);

    let mut left_meta = row().gap(4.0).center_y().child(text("Category").font(style::META).color(p.text_3)).child(topic_select);
    if matches!(ed.topic_choice, TopicChoice::Custom) {
        left_meta = left_meta.child(
            text_input(&ed.topic_custom, style::META)
                .key("nb-f-topic-custom")
                .single_line()
                .w(150.0)
                .color(p.text)
                .placeholder("category name…", p.text_3)
                .caret(p.text, p.selection)
                .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.topic_custom))
                .on_submit(|s: &mut State, tx: &mut Tx| {
                    if let Some(ed) = s.notebook.editor.as_mut() {
                        ed.topic_custom.focused = false;
                        ed.title.focused = true;
                        tx.focus_when_shown("nb-f-title", false);
                    }
                })
                .on_key(|s: &mut State, e: &KeyEvent, _| editor_common_key(s, e)),
        );
    }

    let top_bar = row()
        .w_full()
        .pb(20.0)
        .gap(8.0)
        .center_y()
        .child(left_meta)
        .child(rune::spacer())
        .child(mode_toggle)
        .child(cancel.ml(4.0))
        .child(save);

    // Borderless large Notion/Obsidian title directly on the page surface.
    let title_el = if ed.preview {
        let heading = if ed.title.text.trim().is_empty() { "Untitled".to_string() } else { ed.title.text.trim().to_string() };
        text(heading).font(style::TITLE).color(p.text).wrap()
    } else {
        text_input(&ed.title, style::TITLE)
            .key("nb-f-title")
            .w_full()
            .color(p.text)
            .placeholder("Untitled", p.text_3.alpha(0.55))
            .caret(p.text, p.selection)
            .label("Note title")
            .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.title))
            .on_submit(|s: &mut State, tx: &mut Tx| {
                if let Some(ed) = s.notebook.editor.as_mut() {
                    ed.title.focused = false;
                    ed.body.focused = true;
                    tx.focus_when_shown("nb-f-body", false);
                }
            })
            .on_key(|s: &mut State, e: &KeyEvent, _| editor_common_key(s, e))
    };

    let mut page = col().key("nb-editor").w_full().max_w(DOC_MAX_W).transition(Transition::opacity()).child(top_bar).child(title_el);

    if !ed.preview {
        page = page.child(format_toolbar(p, k).mt(14.0));
    }

    page = page.child(Divider::horizontal().build(k).mt(if ed.preview { 18.0 } else { 8.0 }));

    if let Some(err) = &ed.error {
        page = page.child(text(err.clone()).font(style::META).color(p.amber).wrap().mt(12.0));
    }

    let canvas = if ed.preview {
        if ed.body.text.trim().is_empty() {
            text("Nothing to preview yet.").font(style::PROSE).color(p.text_3).mt(24.0)
        } else {
            render_article_markdown(&ed.body.text, None, p, k).mt(22.0)
        }
    } else {
        let spans = markdown_decorations(&ed.body.text, p, k);
        text_input(&ed.body, style::PROSE)
            .key("nb-f-body")
            .w_full()
            .min_h(420.0)
            .pt(18.0)
            .color(p.text)
            .placeholder(
                "Start writing in Markdown…  # headings, **bold**, `code`, - [ ] checklists, > [!tip] callouts, or ```code blocks",
                p.text_3.alpha(0.55),
            )
            .caret(p.text, p.selection)
            .decorations(spans)
            .label("Note body")
            .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.body))
            .on_submit(|s: &mut State, _| smart_enter(s))
            .on_key(|s: &mut State, e: &KeyEvent, _| body_key(s, e))
    };

    let word_count = ed.body.text.split_whitespace().count();
    let footer = row()
        .w_full()
        .mt(24.0)
        .pt(12.0)
        .center_y()
        .child(text("⌘B bold · ⌘I italic · ⌘E code · ⌘K link · ⌘⇧P preview · ⌘S save").font(style::META).color(p.text_3))
        .child(rune::spacer())
        .child(text(if word_count == 1 { "1 word".to_string() } else { format!("{word_count} words") }).font(style::META).color(p.text_3));

    page.child(canvas).child(Divider::horizontal().build(k).mt(20.0)).child(footer)
}

fn format_toolbar(p: &Palette, k: &Kit) -> El {
    let tool = |key: &'static str, label: &'static str, help: &'static str, action: MdAction| {
        el().key(key)
            .role("button")
            .label(help)
            .focusable(false)
            .h(26.0)
            .px(7.0)
            .radius(5.0)
            .center()
            .hover(k.c.hover)
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                if let Some(ed) = s.notebook.editor.as_mut() {
                    ed.preview = false;
                    ed.title.focused = false;
                    ed.body.focused = true;
                    apply_md_action(&mut ed.body, action);
                    tx.focus_when_shown("nb-f-body", false);
                }
            })
            .child(text(label).font(style::META).color(p.text_2))
    };

    let sep = || el().w(1.0).h(14.0).mx(3.0).fill(p.hairline);

    row()
        .key("nb-md-toolbar")
        .w_full()
        .gap(2.0)
        .center_y()
        .flex_wrap()
        .child(tool("nb-fmt-h1", "H1", "Heading 1 (⌘1)", MdAction::Heading(1)))
        .child(tool("nb-fmt-h2", "H2", "Heading 2 (⌘2)", MdAction::Heading(2)))
        .child(tool("nb-fmt-h3", "H3", "Heading 3 (⌘3)", MdAction::Heading(3)))
        .child(sep())
        .child(tool("nb-fmt-b", "B", "Bold (⌘B)", MdAction::Wrap("**", "**", "bold")))
        .child(tool("nb-fmt-i", "I", "Italic (⌘I)", MdAction::Wrap("*", "*", "italic")))
        .child(tool("nb-fmt-code", "<>", "Inline code (⌘E)", MdAction::Wrap("`", "`", "code")))
        .child(tool("nb-fmt-link", "Link", "Insert link (⌘K)", MdAction::Link))
        .child(sep())
        .child(tool("nb-fmt-ul", "• List", "Bullet list", MdAction::LinePrefix("- ")))
        .child(tool("nb-fmt-ol", "1. List", "Numbered list", MdAction::LinePrefix("1. ")))
        .child(tool("nb-fmt-todo", "☐ Task", "Checklist item", MdAction::LinePrefix("- [ ] ")))
        .child(sep())
        .child(tool("nb-fmt-quote", "Quote", "Blockquote", MdAction::LinePrefix("> ")))
        .child(tool("nb-fmt-callout", "Callout", "Obsidian callout", MdAction::Callout))
        .child(tool("nb-fmt-fence", "Code", "Fenced code block", MdAction::CodeBlock))
        .child(tool("nb-fmt-table", "Table", "Markdown table", MdAction::Table))
        .child(tool("nb-fmt-rule", "—", "Horizontal divider", MdAction::Rule))
}

fn editor_common_key(s: &mut State, e: &KeyEvent) -> bool {
    if matches!(e.key, Key::Escape) {
        if let Some(ed) = s.notebook.editor.as_mut()
            && ed.topic_open
        {
            ed.topic_open = false;
        } else {
            s.notebook.editor = None;
        }
        return true;
    }
    if (matches!(e.key, Key::Enter) && e.mods.cmd) || e.is_command('s') {
        s.notebook_save();
        return true;
    }
    false
}

fn body_key(s: &mut State, e: &KeyEvent) -> bool {
    if editor_common_key(s, e) {
        return true;
    }
    let Some(ed) = s.notebook.editor.as_mut() else {
        return false;
    };
    if e.is_command('b') {
        apply_md_action(&mut ed.body, MdAction::Wrap("**", "**", "bold"));
        return true;
    }
    if e.is_command('i') {
        apply_md_action(&mut ed.body, MdAction::Wrap("*", "*", "italic"));
        return true;
    }
    if e.is_command('e') {
        apply_md_action(&mut ed.body, MdAction::Wrap("`", "`", "code"));
        return true;
    }
    if e.is_command('k') {
        apply_md_action(&mut ed.body, MdAction::Link);
        return true;
    }
    if e.is_command('1') {
        apply_md_action(&mut ed.body, MdAction::Heading(1));
        return true;
    }
    if e.is_command('2') {
        apply_md_action(&mut ed.body, MdAction::Heading(2));
        return true;
    }
    if e.is_command('3') {
        apply_md_action(&mut ed.body, MdAction::Heading(3));
        return true;
    }
    if matches!(e.key, Key::Tab) && !e.mods.cmd && !e.mods.ctrl {
        indent_current_line(&mut ed.body, !e.mods.shift);
        return true;
    }
    false
}

fn smart_enter(s: &mut State) {
    let Some(ed) = s.notebook.editor.as_mut() else { return };
    smart_enter_edit(&mut ed.body);
}

fn smart_enter_edit(edit: &mut TextEdit) {
    let (start, end) = edit.range();
    if start != end {
        edit.insert("\n");
        return;
    }
    let line_idx = edit.line_of_byte(start);
    let line_range = edit.line_range(line_idx);
    let line = &edit.text[line_range.clone()];
    let before_caret = &edit.text[line_range.start..start];
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];

    let continuation = if rest.starts_with("- [ ] ") || rest.starts_with("- [x] ") || rest.starts_with("- [X] ") {
        Some((6usize, format!("{indent}- [ ] ")))
    } else if rest.starts_with("- ") {
        Some((2usize, format!("{indent}- ")))
    } else if rest.starts_with("* ") {
        Some((2usize, format!("{indent}* ")))
    } else if rest.starts_with("> ") {
        Some((2usize, format!("{indent}> ")))
    } else if let Some((prefix_len, next_num)) = parse_ordered_prefix(rest) {
        Some((prefix_len, format!("{indent}{next_num}. ")))
    } else {
        None
    };

    if let Some((marker_len, next_prefix)) = continuation {
        let marker_end = indent_len + marker_len;
        if before_caret.len() >= marker_end {
            // Pressing Enter on an empty list/quote line exits the list cleanly.
            if line[marker_end..].trim().is_empty() && start == line_range.end {
                let mut updated = edit.text.clone();
                updated.replace_range(line_range.start..line_range.end, "");
                edit.replace_text(updated);
                edit.set_caret(line_range.start);
                return;
            }
            edit.insert(&format!("\n{next_prefix}"));
            return;
        }
    }
    edit.insert("\n");
}

fn parse_ordered_prefix(rest: &str) -> Option<(usize, usize)> {
    let digits_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_len == 0 || !rest[digits_len..].starts_with(". ") {
        return None;
    }
    let n: usize = rest[..digits_len].parse().ok()?;
    Some((digits_len + 2, n + 1))
}

fn indent_current_line(edit: &mut TextEdit, indent: bool) {
    let (start, _) = edit.range();
    let line_idx = edit.line_of_byte(start);
    let line_range = edit.line_range(line_idx);
    let mut updated = edit.text.clone();
    if indent {
        updated.insert_str(line_range.start, "  ");
        edit.replace_text(updated);
        edit.set_caret(start + 2);
    } else if updated[line_range.clone()].starts_with("  ") {
        updated.replace_range(line_range.start..line_range.start + 2, "");
        edit.replace_text(updated);
        edit.set_caret(start.saturating_sub(2).max(line_range.start));
    } else if updated[line_range.clone()].starts_with('\t') {
        updated.replace_range(line_range.start..line_range.start + 1, "");
        edit.replace_text(updated);
        edit.set_caret(start.saturating_sub(1).max(line_range.start));
    }
}

fn apply_md_action(edit: &mut TextEdit, action: MdAction) {
    match action {
        MdAction::Wrap(before, after, placeholder) => {
            let (start, end) = edit.range();
            let text_len = edit.text.len();
            if start < end {
                // If already wrapped right around selection, unwrap cleanly.
                let wrapped_around = start >= before.len()
                    && end + after.len() <= text_len
                    && &edit.text[start - before.len()..start] == before
                    && &edit.text[end..end + after.len()] == after;
                if wrapped_around {
                    let inner = edit.text[start..end].to_string();
                    let mut updated = edit.text.clone();
                    updated.replace_range(start - before.len()..end + after.len(), &inner);
                    edit.replace_text(updated);
                    edit.set_selection(start - before.len(), start - before.len() + inner.len());
                    return;
                }
                let selected = edit.selected().to_string();
                let replacement = format!("{before}{selected}{after}");
                edit.insert(&replacement);
                edit.set_selection(start + before.len(), start + before.len() + selected.len());
            } else {
                let replacement = format!("{before}{placeholder}{after}");
                edit.insert(&replacement);
                edit.set_selection(start + before.len(), start + before.len() + placeholder.len());
            }
        }
        MdAction::Link => {
            let (start, end) = edit.range();
            if start < end {
                let selected = edit.selected().to_string();
                let replacement = format!("[{selected}](https://)");
                edit.insert(&replacement);
                let url_start = start + 1 + selected.len() + 2;
                edit.set_selection(url_start, url_start + "https://".len());
            } else {
                edit.insert("[link](https://)");
                edit.set_selection(start + 1, start + 5);
            }
        }
        MdAction::Heading(level) => {
            let (start, _) = edit.range();
            let line_idx = edit.line_of_byte(start);
            let line_range = edit.line_range(line_idx);
            let line = edit.text[line_range.clone()].to_string();
            let existing_hashes = line.bytes().take_while(|&b| b == b'#').count();
            let has_heading = existing_hashes > 0 && line[existing_hashes..].starts_with(' ');
            let target_prefix = format!("{} ", "#".repeat(level));
            let (new_line, delta): (String, isize) = if has_heading && existing_hashes == level {
                let stripped = &line[existing_hashes + 1..];
                (stripped.to_string(), -((existing_hashes + 1) as isize))
            } else if has_heading {
                let stripped = &line[existing_hashes + 1..];
                let old_len = existing_hashes + 1;
                (format!("{target_prefix}{stripped}"), target_prefix.len() as isize - old_len as isize)
            } else {
                (format!("{target_prefix}{line}"), target_prefix.len() as isize)
            };
            let mut updated = edit.text.clone();
            updated.replace_range(line_range.clone(), &new_line);
            edit.replace_text(updated);
            let next_caret = ((start as isize + delta).max(line_range.start as isize) as usize).min(edit.text.len());
            edit.set_caret(next_caret);
        }
        MdAction::LinePrefix(prefix) => {
            let (start, _) = edit.range();
            let line_idx = edit.line_of_byte(start);
            let line_range = edit.line_range(line_idx);
            let line = edit.text[line_range.clone()].to_string();
            let (new_line, delta): (String, isize) = if let Some(rest) = line.strip_prefix(prefix) {
                (rest.to_string(), -(prefix.len() as isize))
            } else {
                (format!("{prefix}{line}"), prefix.len() as isize)
            };
            let mut updated = edit.text.clone();
            updated.replace_range(line_range.clone(), &new_line);
            edit.replace_text(updated);
            let next_caret = ((start as isize + delta).max(line_range.start as isize) as usize).min(edit.text.len());
            edit.set_caret(next_caret);
        }
        MdAction::Callout => {
            let (start, _) = edit.range();
            let lead = if start == 0 || edit.text[..start].ends_with('\n') { "" } else { "\n\n" };
            let snippet = format!("{lead}> [!tip]\n> ");
            edit.insert(&snippet);
        }
        MdAction::CodeBlock => {
            let (start, end) = edit.range();
            let lead = if start == 0 || edit.text[..start].ends_with('\n') { "" } else { "\n" };
            if start < end {
                let selected = edit.selected().to_string();
                let snippet = format!("{lead}```\n{selected}\n```\n");
                edit.insert(&snippet);
            } else {
                let snippet = format!("{lead}```\n\n```\n");
                let caret = start + lead.len() + 4;
                edit.insert(&snippet);
                edit.set_caret(caret);
            }
        }
        MdAction::Table => {
            let (start, _) = edit.range();
            let lead = if start == 0 || edit.text[..start].ends_with('\n') { "" } else { "\n\n" };
            let snippet = format!("{lead}| Column A | Column B |\n| --- | --- |\n| Value | Value |\n");
            edit.insert(&snippet);
        }
        MdAction::Rule => {
            let (start, _) = edit.range();
            let lead = if start == 0 || edit.text[..start].ends_with('\n') { "" } else { "\n\n" };
            edit.insert(&format!("{lead}---\n\n"));
        }
    }
}

// ── Live Obsidian-Style Markdown Decorations ──

/// Produces live styled [`Span`] decorations for raw Markdown inside the borderless editor:
/// larger bold headings with faint `#`, inline bold/italic/code pills, syntax-highlighted fenced
/// code blocks, callout headers, list/task markers, and underlined links.
fn markdown_decorations(src: &str, p: &Palette, k: &Kit) -> Vec<Span> {
    let mut spans = Vec::new();
    let syntax_theme = SyntaxTheme::from_kit(k);
    let syntax_dim = p.text_3.alpha(0.75);
    let code_bg = k.c.layer_02;

    let mut offset = 0usize;
    let mut in_fence = false;
    let mut fence_lang: Option<String> = None;
    let mut fence_body_start = 0usize;

    for raw_line in src.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line_start = offset;
        let line_end = line_start + line.len();
        offset += raw_line.len();

        let trimmed = line.trim_start();
        if let Some(after_ticks) = trimmed.strip_prefix("```") {
            if line_start < line_end {
                spans.push(Span::new(line_start, line_end).mono().color(syntax_dim));
            }
            if !in_fence {
                in_fence = true;
                let lang = after_ticks.split_whitespace().next().unwrap_or("");
                fence_lang = (!lang.is_empty()).then(|| lang.to_string());
                fence_body_start = offset;
            } else {
                if fence_body_start < line_start {
                    let code_slice = &src[fence_body_start..line_start];
                    spans.push(Span::new(fence_body_start, line_start).mono().size(13.0));
                    let hl = syntax::highlight(fence_lang.as_deref(), code_slice);
                    spans.extend(syntax_theme.spans(&hl, fence_body_start));
                }
                in_fence = false;
                fence_lang = None;
            }
            continue;
        }

        if in_fence {
            continue;
        }

        if line.is_empty() {
            continue;
        }

        // Horizontal rule
        if matches!(trimmed, "---" | "***" | "___") {
            spans.push(Span::new(line_start, line_end).color(syntax_dim));
            continue;
        }

        // ATX Headings (# .. ####)
        let hashes = line.bytes().take_while(|&b| b == b'#').count();
        if (1..=4).contains(&hashes) && line[hashes..].starts_with(' ') {
            let size = match hashes {
                1 => 23.0,
                2 => 19.0,
                3 => 16.5,
                _ => 15.0,
            };
            spans.push(Span::new(line_start, line_start + hashes + 1).size(size).color(syntax_dim));
            if line_start + hashes + 1 < line_end {
                spans.push(Span::new(line_start + hashes + 1, line_end).size(size).weight(600.0).color(p.text));
            }
            continue;
        }

        // Obsidian callout header: `> [!tip] Optional title`
        if let Some((kind, _)) = parse_callout_header(line) {
            let accent = kind.accent(p);
            spans.push(Span::new(line_start, line_end).weight(600.0).color(accent));
            continue;
        }

        // Blockquotes
        let indent = line.len() - trimmed.len();
        let mut content_offset = line_start + indent;
        if let Some(after_q) = trimmed.strip_prefix('>') {
            let marker_end = content_offset + 1 + usize::from(after_q.starts_with(' '));
            spans.push(Span::new(content_offset, marker_end).color(syntax_dim));
            if marker_end < line_end {
                spans.push(Span::new(marker_end, line_end).color(p.text_2));
                decorate_inline(&src[marker_end..line_end], marker_end, p, syntax_dim, code_bg, &mut spans);
            }
            continue;
        }

        // Task list or unordered/ordered list marker
        if trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
            let done = trimmed.as_bytes()[3] != b' ';
            let marker_end = content_offset + 6;
            spans.push(Span::new(content_offset, marker_end).mono().color(if done { CalloutKind::Tip.accent(p) } else { syntax_dim }));
            if done && marker_end < line_end {
                spans.push(Span::new(marker_end, line_end).color(p.text_3));
            }
            content_offset = marker_end;
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            let marker_end = content_offset + 2;
            spans.push(Span::new(content_offset, marker_end).color(syntax_dim));
            content_offset = marker_end;
        } else if let Some((plen, _)) = parse_ordered_prefix(trimmed) {
            let marker_end = content_offset + plen;
            spans.push(Span::new(content_offset, marker_end).color(syntax_dim));
            content_offset = marker_end;
        }

        if content_offset < line_end {
            decorate_inline(&src[content_offset..line_end], content_offset, p, syntax_dim, code_bg, &mut spans);
        }
    }

    // Highlight an unclosed code fence while the user is actively typing inside it.
    if in_fence && fence_body_start < src.len() {
        let code_slice = &src[fence_body_start..];
        spans.push(Span::new(fence_body_start, src.len()).mono().size(13.0));
        let hl = syntax::highlight(fence_lang.as_deref(), code_slice);
        spans.extend(syntax_theme.spans(&hl, fence_body_start));
    }

    spans
}

fn decorate_inline(text: &str, base: usize, p: &Palette, dim: Color, code_bg: Color, out: &mut Vec<Span>) {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        // Inline code: `code`
        if bytes[i] == b'`'
            && let Some(rel) = text[i + 1..].find('`')
            && rel > 0
        {
            let close = i + 1 + rel;
            out.push(Span::new(base + i, base + i + 1).mono().color(dim));
            out.push(Span::new(base + i + 1, base + close).mono().size(13.5).bg(code_bg, 2.5));
            out.push(Span::new(base + close, base + close + 1).mono().color(dim));
            i = close + 1;
            continue;
        }

        // Bold: **text**
        if i + 3 < len
            && bytes[i] == b'*'
            && bytes[i + 1] == b'*'
            && let Some(rel) = text[i + 2..].find("**")
            && rel > 0
        {
            let close = i + 2 + rel;
            out.push(Span::new(base + i, base + i + 2).color(dim));
            out.push(Span::new(base + i + 2, base + close).bold());
            out.push(Span::new(base + close, base + close + 2).color(dim));
            i = close + 2;
            continue;
        }

        // Italic: *text*
        if i + 2 < len
            && bytes[i] == b'*'
            && bytes[i + 1] != b'*'
            && bytes[i + 1] != b' '
            && let Some(rel) = text[i + 1..].find('*')
            && rel > 0
        {
            let close = i + 1 + rel;
            out.push(Span::new(base + i, base + i + 1).color(dim));
            out.push(Span::new(base + i + 1, base + close).italic());
            out.push(Span::new(base + close, base + close + 1).color(dim));
            i = close + 1;
            continue;
        }

        // Markdown link: [label](url)
        if bytes[i] == b'['
            && let Some(mid_rel) = text[i + 1..].find("](")
            && let Some(end_rel) = text[i + 1 + mid_rel + 2..].find(')')
        {
            let mid = i + 1 + mid_rel;
            let end = mid + 2 + end_rel;
            out.push(Span::new(base + i, base + i + 1).color(dim));
            if i + 1 < mid {
                out.push(Span::new(base + i + 1, base + mid).color(p.text).underline(p.text_3));
            }
            out.push(Span::new(base + mid, base + end + 1).color(dim));
            i = end + 1;
            continue;
        }

        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
}

fn empty_notebook(p: &Palette, k: &Kit) -> El {
    col()
        .key("nb-empty")
        .w_full()
        .items_center()
        .pt(88.0)
        .gap(14.0)
        .child(text("Your knowledge library is empty").font(style::DISPLAY).color(p.text))
        .child(
            text("Write freeform Markdown notes or let agents save architecture decisions, gotchas, and patterns here.")
                .font(style::PROSE)
                .color(p.text_2)
                .wrap()
                .max_w(480.0)
                .align(rune::Align::Center),
        )
        .child(
            Button::new("Write the First Note")
                .icon(&PLUS_ICON)
                .variant(ButtonVariant::Contrast)
                .control_size(ControlSize::Large)
                .on_press(|s: &mut State| s.notebook_new_page())
                .build(k)
                .mt(8.0)
                .key("nb-empty-new"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: i64, kind: Kind, topic: &str, title: &str, tags: &[&str], standing: Standing) -> Page {
        Page {
            id,
            kind,
            topic: topic.into(),
            title: title.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            body: "## Context\n\nSomething happened while building.\n\n## Takeaway\n\nDo the careful thing next time.".into(),
            project_id: None,
            agent: "Codex".into(),
            created_at: id,
            updated_at: id,
            revision: 1,
            standing,
            helped: 0,
        }
    }

    fn notebook() -> Notebook {
        let mut nb = Notebook::default();
        nb.set_pages(vec![
            page(3, Kind::Pitfall, "rust", "Scroll view collapses inside a grow column", &["rune", "layout"], Standing::Current),
            page(2, Kind::Fix, "sqlite", "SQLite reads stall during long writes", &["sqlite", "locks"], Standing::Current),
            page(4, Kind::Pattern, "nextjs", "Keep App Router metadata in server components", &["app-router"], Standing::Current),
            page(1, Kind::Pattern, "rust", "Old layout trick", &["layout"], Standing::Archived),
        ]);
        nb
    }

    #[test]
    fn topics_kinds_and_search_narrow_the_index() {
        let mut nb = notebook();
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [3, 2, 4], "archived pages stay off All");
        nb.shelf = Shelf::Topic("rust".into());
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [3]);
        nb.query = TextEdit::new("rune");
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [3], "search matches tags");
        nb.query = TextEdit::new("");
        nb.shelf = Shelf::Archived;
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [1]);
        nb.shelf = Shelf::All;
        nb.query = TextEdit::new("sqlite stall");
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [2], "every word must match");
        nb.query = TextEdit::new("next.js");
        assert_eq!(nb.visible().iter().map(|p| p.id).collect::<Vec<_>>(), [4], "topic label matches search too");
    }

    #[test]
    fn tree_builds_topics_and_preserves_collapse_across_updates() {
        let mut nb = notebook();
        assert!(nb.tree.contains("topic:rust"));
        assert!(nb.tree.contains("page:3"));
        assert!(!nb.tree.contains("page:1"), "archived pages stay out of the topic tree");
        nb.tree.set_expanded("topic:rust", false);
        assert!(!nb.tree.is_expanded("topic:rust"));

        nb.set_pages(nb.pages.clone());
        assert!(!nb.tree.is_expanded("topic:rust"));

        nb.query = TextEdit::new("scroll");
        nb.apply_tree_filter();
        assert!(nb.tree.is_filtering());
        assert_eq!(nb.tree.visible_len(), 2);
        nb.query = TextEdit::new("");
        nb.apply_tree_filter();
        assert!(!nb.tree.is_expanded("topic:rust"));
    }

    #[test]
    fn escape_unwinds_menu_editor_page_then_search() {
        let mut s = State::new();
        s.notebook = notebook();
        s.notebook.query = TextEdit::new("sqlite");
        s.notebook.open = Some(2);
        s.notebook.editor = Some(Editor::blank(None));
        s.notebook.action_menu_open = true;

        assert!(s.notebook_escape());
        assert!(!s.notebook.action_menu_open);
        assert!(s.notebook_escape());
        assert!(s.notebook.editor.is_none());
        assert!(s.notebook_escape());
        assert!(s.notebook.open.is_none());
        assert!(s.notebook_escape());
        assert!(s.notebook.query.text.is_empty());
        assert!(!s.notebook_escape());
    }

    #[test]
    fn markdown_toolbar_actions_and_smart_enter_behave_like_obsidian() {
        let mut edit = TextEdit::new("hello world");
        edit.set_selection(6, 11);
        apply_md_action(&mut edit, MdAction::Wrap("**", "**", "bold"));
        assert_eq!(edit.text, "hello **world**");
        // Applying again unwraps cleanly.
        apply_md_action(&mut edit, MdAction::Wrap("**", "**", "bold"));
        assert_eq!(edit.text, "hello world");

        // Heading toggle & level switch.
        edit.set_caret(0);
        apply_md_action(&mut edit, MdAction::Heading(2));
        assert_eq!(edit.text, "## hello world");
        apply_md_action(&mut edit, MdAction::Heading(1));
        assert_eq!(edit.text, "# hello world");
        apply_md_action(&mut edit, MdAction::Heading(1));
        assert_eq!(edit.text, "hello world");

        // Smart enter continues checklist & exits on empty item.
        let mut list = TextEdit::new("- [ ] Ship carousels");
        list.set_caret(list.text.len());
        smart_enter_edit(&mut list);
        assert_eq!(list.text, "- [ ] Ship carousels\n- [ ] ");
        smart_enter_edit(&mut list);
        assert_eq!(list.text, "- [ ] Ship carousels\n");
    }

    #[test]
    fn article_chunks_split_callouts_and_preserve_task_offsets() {
        let md = "- [ ] First task\n\n> [!tip] Keep it simple\n> - [ ] Inside callout\n\n- [x] After callout";
        let chunks = split_article_chunks(md);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], ArticleChunk::Markdown { text: "- [ ] First task".into(), task_offset: 0 });
        assert_eq!(
            chunks[1],
            ArticleChunk::Callout {
                kind: CalloutKind::Tip,
                title: "Keep it simple".into(),
                body: "- [ ] Inside callout".into(),
                task_offset: 1,
            }
        );
        assert_eq!(chunks[2], ArticleChunk::Markdown { text: "- [x] After callout".into(), task_offset: 2 });
    }

    #[test]
    fn markdown_decorations_stay_on_valid_utf8_boundaries() {
        let p = style::palette(true);
        let k = style::kit(&p);
        let src = "# Heading — café\n\n**bold** and *italic* with `code` and [link](https://example.com)\n\n> [!warning]\n> Watch out\n\n- [ ] Todo item\n- [x] Done item\n\n```rust\nlet x = \"日本語\";\n```\n";
        let spans = markdown_decorations(src, &p, &k);
        assert!(!spans.is_empty());
        for sp in spans {
            let s = sp.start as usize;
            let e = sp.end as usize;
            assert!(s <= e && e <= src.len(), "out of bounds span {s}..{e}");
            assert!(src.is_char_boundary(s) && src.is_char_boundary(e), "non-char-boundary span {s}..{e}");
        }
    }
}
