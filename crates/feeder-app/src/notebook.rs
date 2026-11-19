//! The Notebook tab: Feeder's shared engineering knowledge base, modelled on Obsidian & Notion
//! using native `rune-kit` components (`SplitView`, `Toolbar`, `Tree`, `Select`, `TextInput`,
//! `TextArea`, `SegmentedControl`, `Markdown`, `Menu`, `Divider`, `Badge`, `Button`).

use std::collections::HashMap;

use feeder_core::{
    CANONICAL_TOPICS, Kind, Page, PageDraft, Standing, ago, infer_topic, normalize_topic, now_ms, topic_counts, topic_label,
};
use rune::{
    Color, Cursor, El, Icon, Key, KeyEvent, Link, LinkPaint, ScrollSpec, TextEdit, Transition, Tx, col, el, icon, oklch, row, text,
};
use rune_kit::shell::{SidebarPeek, SidebarToggle};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Divider, Kit, Markdown, Menu, MenuItem, PaneState, Placement,
    SegmentedControl, Select, SelectAppearance, SidebarHeader, SidebarRow, SplitView, TextArea, TextInput, Toolbar, Tree, TreeItem,
    TreeModel, TreeRow, TreeStyle, scroll_bar,
};

use crate::style::{self, Palette};
use crate::watch::NotebookOp;
use crate::{State, ToastItem, ToastKind, agents, chrome, feed, orb};

/// Keeps index rows and document prose comfortably scannable.
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
    /// `(page id, when)` for the Copy button's "Copied" feedback.
    pub copied: Option<(i64, i64)>,
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
            copied: None,
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

/// A page being written or revised in the Notion-style document editor.
pub struct Editor {
    /// The page being revised, or None for a new one.
    pub id: Option<i64>,
    pub kind: Kind,
    pub topic_choice: TopicChoice,
    pub topic_open: bool,
    pub topic_custom: TextEdit,
    pub title: TextEdit,
    pub tags: TextEdit,
    pub what: TextEdit,
    pub why: TextEdit,
    pub rule: TextEdit,
    pub error: Option<String>,
}

impl Editor {
    fn blank(default_topic: Option<&str>) -> Editor {
        let mut title = TextEdit::new("");
        title.focused = true;
        Editor {
            id: None,
            kind: Kind::Fix,
            topic_choice: match default_topic {
                Some(t) if !t.is_empty() => TopicChoice::Selected(t.to_string()),
                _ => TopicChoice::Auto,
            },
            topic_open: false,
            topic_custom: TextEdit::new(""),
            title,
            tags: TextEdit::new(""),
            what: TextEdit::new(""),
            why: TextEdit::new(""),
            rule: TextEdit::new(""),
            error: None,
        }
    }

    fn from_page(p: &Page) -> Editor {
        let mut title = TextEdit::new(&p.title);
        title.focused = true;
        Editor {
            id: Some(p.id),
            kind: p.kind,
            topic_choice: TopicChoice::Selected(p.topic.clone()),
            topic_open: false,
            topic_custom: TextEdit::new(""),
            title,
            tags: TextEdit::new(p.tags.join(", ")),
            what: TextEdit::new(&p.context),
            why: TextEdit::new(p.cause.as_deref().unwrap_or("")),
            rule: TextEdit::new(&p.rule),
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
        let why = self.why.text.trim();
        PageDraft {
            kind: self.kind,
            topic: self.resolved_topic_arg(),
            title: self.title.text.clone(),
            tags: vec![self.tags.text.clone()],
            context: self.what.text.clone(),
            cause: (!why.is_empty()).then(|| why.to_string()),
            rule: self.rule.text.clone(),
        }
    }

    fn is_typing(&self) -> bool {
        self.title.focused || self.topic_custom.focused || self.tags.focused || self.what.focused || self.why.focused || self.rule.focused
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
                let search_blob = format!(
                    "{} {} {} {} {} {} {}",
                    pg.title,
                    pg.topic,
                    topic_label(&pg.topic),
                    pg.tags.join(" "),
                    pg.rule,
                    pg.context,
                    pg.cause.as_deref().unwrap_or("")
                )
                .to_lowercase();
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
    let rule = p.rule.to_lowercase();
    let rest = format!("{} {}", p.context, p.cause.as_deref().unwrap_or("")).to_lowercase();
    words.iter().try_fold(0, |acc, w| {
        let hit = if title.contains(w.as_str()) {
            6
        } else if topic_slug.contains(w.as_str()) || topic_name.contains(w.as_str()) {
            5
        } else if p.tags.iter().any(|t| t.contains(w.as_str())) {
            4
        } else if rule.contains(w.as_str()) {
            3
        } else if rest.contains(w.as_str()) {
            1
        } else {
            return None;
        };
        Some(acc + hit)
    })
}

/// Each kind's muted semantic ink dot/accent, legible in both light and dark modes.
pub fn kind_color(kind: Kind, p: &Palette) -> Color {
    // (hue, light chroma, dark chroma): each chroma stays inside sRGB at its lightness, so no
    // ink is clipped toward a neighbouring hue.
    let (hue, light, dark) = match kind {
        Kind::Fix => (155.0, 0.13, 0.13),
        Kind::Pitfall => (32.0, 0.16, 0.14),
        Kind::Pattern => (252.0, 0.14, 0.125),
        Kind::Decision => (300.0, 0.13, 0.12),
    };
    if p.dark { oklch(0.76, dark, hue) } else { oklch(0.53, light, hue) }
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

    /// Checks the editor's page against the notebook's bar and files it, or shows what's missing.
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
        let verb = if id.is_some() { "Revised" } else { "Added" };
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

    // Shrinks before the view tabs do: the last crumb truncates instead of running under them.
    let mut row_el = row().key("nb-breadcrumb").gap(2.0).center_y().shrink().min_w(0.0);
    if let Some(ed) = &nb.editor {
        row_el = row_el
            .child(crumb_btn("nb-bc-all", "Notebook", false).on_press(|s: &mut State| {
                s.notebook.editor = None;
                s.notebook_shelf(Shelf::All);
            }))
            .child(sep())
            .child(
                text(if ed.id.is_some() { "Revise page" } else { "New page" }).font(k.ty.ui_compact).color(p.text).truncate().pl(CRUMB_PAD),
            );
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

    let new_page = Button::new("New Page")
        .icon(&PLUS_ICON)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook_new_page())
        .build(k)
        .w_full()
        .key("nb-new");

    let all_selected = nb.editor.is_none() && nb.open.is_none() && nb.shelf == Shelf::All;
    let all_row = SidebarRow::new("All pages")
        .badge(current_count.to_string())
        .selected(all_selected)
        .on_select(|s: &mut State, _| s.notebook_shelf(Shelf::All))
        .build(k)
        .key("nb-shelf-all");

    let header = col().key("nb-sidebar-header").w_full().px(10.0).pt(4.0).pb(8.0).gap(8.0).child(search).child(new_page).child(all_row);

    let mut topics_head = SidebarHeader::new("Topics");
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
                .child(text("Topics appear as agents add pages.").font(style::META).color(p.text_3).wrap()),
        );
    } else {
        let palette = *p;
        let q = query.clone();
        let mut tree_style = TreeStyle::from_kit(k).chevron_only_folders(true).guides_on_hover(0.55);
        tree_style.row_height = 26.0;
        let tree_el = Tree::new(|s: &State| &s.notebook.tree, |s: &mut State| &mut s.notebook.tree, NavNode::label)
            .name("Notebook topics")
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
                    let mut item = TreeItem::new(title).dot(kind_color(*kind, &palette));
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

    let mut kind_labels = vec!["All"];
    for kind in Kind::ALL {
        kind_labels.push(kind.plural());
    }
    let selected_kind_idx = match nb.kind_filter {
        None => Some(0),
        Some(kind) => Kind::ALL.iter().position(|&x| x == kind).map(|i| i + 1),
    };
    let kind_control = SegmentedControl::new(&kind_labels, selected_kind_idx)
        .label("Filter by kind")
        .fit()
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            s.notebook.kind_filter = if i == 0 { None } else { Kind::ALL.get(i - 1).copied() };
        })
        .build(k);

    // The heading and the filter are the whole header: the list below already shows how many
    // pages there are, and a tagline under a heading only restates it.
    let header =
        col().key("nb-header").w_full().pb(16.0).gap(20.0).child(text(heading).font(style::DISPLAY).color(p.text)).child(kind_control);

    let mut list = col().key("nb-index").w_full();
    if pages.is_empty() {
        list = list.child(
            col()
                .w_full()
                .py(36.0)
                .gap(8.0)
                .child(text("No matching pages").font(style::NAME).color(p.text))
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
    let ink = kind_color(page.kind, p);
    let show_topic = !matches!(s.notebook.shelf, Shelf::Topic(_));

    let mut meta_top = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(el().size(7.0, 7.0).radius(3.5).fill(ink))
        .child(text(page.kind.label().to_string()).font(style::META).color(ink));

    if show_topic {
        meta_top = meta_top.child(Badge::new(topic_label(&page.topic)).variant(BadgeVariant::Subtle).build(k));
    }
    meta_top = meta_top.child(rune::spacer()).child(text(ago(page.updated_at, s.now)).font(style::META).color(p.text_3));

    // The hover surface reaches ROW_BLEED past the column so the row's text sits on the same
    // edge as the heading and the kind filter above it.
    col()
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
        .child(text(page.title.clone()).font(style::SECTION_TITLE).color(p.text).wrap())
        .child(text(page.rule.clone()).font(style::BODY).color(p.text_2).wrap().max_lines(2))
        .child(byline(s, page, p, 12.0).mt(2.0))
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
    let ink = kind_color(page.kind, p);
    let md = feed::markdown_style(p, k).body_font(style::PROSE);
    let project = page.project_id.and_then(|pid| s.summary(pid)).map(|sum| sum.project.name.clone());
    let markdown = page.to_markdown(project.as_deref(), s.now);
    let copied = s.notebook.copied.is_some_and(|(pid, at)| pid == id && now_ms() - at < 2_000);

    let back_a11y = if page.is_archived() { "Back to Archived".to_string() } else { format!("Back to {}", topic_label(&page.topic)) };
    let back_shelf = if page.is_archived() { Shelf::Archived } else { Shelf::Topic(page.topic.clone()) };

    let copy_btn = Button::new(if copied { "Copied" } else { "Copy" })
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help("Copy page as Markdown")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            tx.copy(markdown.clone());
            s.notebook.copied = Some((id, now_ms()));
        })
        .build(k)
        .key("nb-copy");

    let helped_label = if page.helped > 0 { format!("Helped ({})", page.helped) } else { "Helped".to_string() };
    let helped_btn = Button::new(helped_label)
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help("Mark that this page helped you")
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
        if page.is_archived() { ("Restore page", Standing::Current) } else { ("Archive page", Standing::Archived) };
    let more_trigger = row()
        .key("nb-more-btn")
        .role("button")
        .label("Page actions")
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
        .item(MenuItem::new("Delete Page…").danger(true).on_select(move |s: &mut State| {
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
            // A text link, flush with the title's edge below it; the toolbar's breadcrumb names
            // where it goes.
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
            .child(text("Delete this page permanently?").font(style::BODY).color(p.text))
            .child(rune::spacer())
            .child(
                Button::new("Keep")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press(|s: &mut State| s.notebook.delete_confirm = None)
                    .build(k),
            )
            .child(
                Button::new("Delete Page")
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

    let mut title_line = col().w_full().gap(8.0).child(text(page.title.clone()).font(style::TITLE).color(p.text).wrap());
    if page.is_archived() {
        title_line = title_line.child(row().child(Badge::new("Archived").variant(BadgeVariant::Neutral).standard().build(k)));
    }
    doc = doc.child(title_line);

    // Notion-style document Properties block.
    let prop_row = |label: &str, value: El| {
        row()
            .w_full()
            .min_h(26.0)
            .gap(12.0)
            .center_y()
            .child(text(label.to_string()).font(style::META).color(p.text_3).w(84.0))
            .child(row().grow(1.0).min_w(0.0).flex_wrap().gap(6.0).center_y().child(value))
    };

    let topic_slug = page.topic.clone();
    let topic_pill = row()
        .key("nb-prop-topic")
        .role("button")
        .label(format!("Open topic {}", topic_label(&topic_slug)))
        .h(22.0)
        .px(8.0)
        .center_y()
        .radius(6.0)
        .fill(k.c.layer_02)
        .hover(k.c.hover)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.notebook_shelf(Shelf::Topic(topic_slug.clone())))
        .child(text(topic_label(&page.topic).to_string()).font(style::META).color(p.text));

    let kind_val = row()
        .gap(6.0)
        .center_y()
        .child(el().size(7.0, 7.0).radius(3.5).fill(ink))
        .child(text(page.kind.label().to_string()).font(style::META).color(p.text));

    let mut tags_val = row().w_full().min_w(0.0).gap(5.0).flex_wrap().center_y();
    let page_topic = page.topic.clone();
    for tag in &page.tags {
        let t = tag.clone();
        let topic_for_tag = page_topic.clone();
        tags_val = tags_val.child(
            row()
                .key(format!("nb-chip-{tag}"))
                .role("button")
                .label(format!("Search the notebook for {tag}"))
                .h(22.0)
                .px(8.0)
                .center_y()
                .radius(6.0)
                .fill(k.c.layer_01)
                .border(1.0, k.c.border_muted)
                .hover(k.c.hover)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| {
                    s.notebook_shelf(Shelf::Topic(topic_for_tag.clone()));
                    s.notebook.query = TextEdit::new(&t);
                    s.notebook.apply_tree_filter();
                })
                .child(text(tag.clone()).font(style::META).color(p.text_2)),
        );
    }

    let revised = if page.revision > 1 {
        format!("rev {} · revised {}", page.revision, ago(page.updated_at, s.now))
    } else {
        format!("written {}", ago(page.created_at, s.now))
    };
    let source_val = row()
        .gap(8.0)
        .center_y()
        .flex_wrap()
        .child(byline(s, page, p, 13.0))
        .child(text("·").font(style::META).color(p.text_3))
        .child(text(revised).font(style::META).color(p.text_3));

    let mut props = col()
        .key("nb-properties")
        .w_full()
        .mt(18.0)
        .py(12.0)
        .gap(4.0)
        .child(prop_row("Topic", topic_pill))
        .child(prop_row("Kind", kind_val));
    if !page.tags.is_empty() {
        props = props.child(prop_row("Tags", tags_val));
    }
    props = props.child(prop_row("Source", source_val));

    doc = doc.child(Divider::horizontal().build(k).mt(18.0)).child(props).child(Divider::horizontal().build(k));

    // Document prose sections: sentence-case headings, no uppercase scream labels.
    doc = doc.child(
        col()
            .w_full()
            .mt(26.0)
            .gap(8.0)
            .child(text("What happened").font(style::HEADING).color(p.text))
            .child(Markdown::new(page.context.clone()).selectable(true).render(&md)),
    );

    if let Some(cause) = &page.cause {
        doc = doc.child(
            col()
                .w_full()
                .mt(22.0)
                .gap(8.0)
                .child(text("Why").font(style::HEADING).color(p.text))
                .child(Markdown::new(cause.clone()).selectable(true).render(&md)),
        );
    }

    // Obsidian / Notion left-accented callout for the actionable rule.
    let callout = row()
        .key("nb-rule")
        .w_full()
        .mt(26.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .items_stretch()
        .child(el().w(3.0).my(12.0).ml(14.0).radius(1.5).fill(ink))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .pl(12.0)
                .pr(16.0)
                .py(14.0)
                .gap(6.0)
                .child(text("Takeaway").font(style::CONTROL).color(ink))
                .child(text(page.rule.clone()).font(style::EMPHASIS).color(p.text).wrap().selectable(true)),
        );
    doc = doc.child(callout);

    let related = related_pages(&s.notebook, page);
    if !related.is_empty() {
        let mut rel = col().w_full().mt(32.0).gap(4.0).child(text("Related").font(style::HEADING).color(p.text_2).mb(6.0));
        for r in related {
            let rid = r.id;
            rel = rel.child(
                row()
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
                    .on_press(move |s: &mut State| s.notebook_open_page(rid))
                    .child(el().size(6.0, 6.0).radius(3.0).fill(kind_color(r.kind, p)))
                    .child(text(r.title.clone()).font(style::NAME).color(p.text).truncate().grow(1.0).min_w(0.0))
                    .child(Badge::new(topic_label(&r.topic)).variant(BadgeVariant::Subtle).build(k))
                    .child(text(r.kind.label().to_string()).font(style::META).color(p.text_3)),
            );
        }
        doc = doc.child(Divider::horizontal().build(k).mt(32.0)).child(rel);
    }

    doc
}

/// Up to three current pages related by shared tags or matching topic.
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

fn editor_view(nb: &Notebook, ed: &Editor, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let ink = kind_color(ed.kind, p);
    let heading = if ed.id.is_some() { "Revise page" } else { "New page" };
    let save = Button::new(if ed.id.is_some() { "Save Revision" } else { "Add Page" })
        .variant(ButtonVariant::Contrast)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook_save())
        .build(k);
    let cancel = Button::new("Cancel")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .on_press(|s: &mut State| s.notebook.editor = None)
        .build(k);

    let top_bar = row()
        .w_full()
        .pb(18.0)
        .center_y()
        .child(text(heading).font(style::HEADING).color(p.text_2))
        .child(rune::spacer())
        .child(cancel)
        .child(save.ml(6.0));

    let title_input = TextInput::new(&ed.title)
        .plain()
        .large()
        .text_style(style::TITLE)
        .width(None)
        .placeholder("Untitled page")
        .label("Page title")
        .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.title))
        .on_submit(|s: &mut State, _| s.notebook_save())
        .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
        .build(k)
        .key("nb-f-title");

    // Topic Select options: Auto (new pages), all canonical topics, any existing custom topics, Custom...
    let mut topic_options: Vec<(String, TopicChoice)> = Vec::new();
    if ed.id.is_none() {
        let tags: Vec<String> =
            ed.tags.text.split([',', ' ']).map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty()).collect();
        let inferred = infer_topic(&tags, &ed.title.text, &ed.what.text);
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
    topic_options.push(("Custom topic…".to_string(), TopicChoice::Custom));

    let option_labels: Vec<String> = topic_options.iter().map(|(l, _)| l.clone()).collect();
    let current_topic_label = topic_options
        .iter()
        .find(|(_, choice)| *choice == ed.topic_choice)
        .map(|(l, _)| l.clone())
        .unwrap_or_else(|| "Custom topic…".to_string());

    let map_opts = topic_options.clone();
    let topic_select = Select::new("nb-ed-topic", &option_labels)
        .appearance(SelectAppearance::Field)
        .width(220.0)
        .label("Topic")
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

    let mut topic_cell = row().gap(8.0).center_y().flex_wrap().child(topic_select);
    if matches!(ed.topic_choice, TopicChoice::Custom) {
        topic_cell = topic_cell.child(
            TextInput::new(&ed.topic_custom)
                .placeholder("e.g. graphql, caching")
                .label("Custom topic")
                .width(Some(180.0))
                .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.topic_custom))
                .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
                .build(k)
                .key("nb-f-topic-custom"),
        );
    }

    let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.label()).collect();
    let kind_pick = SegmentedControl::new(&kinds, Kind::ALL.iter().position(|x| *x == ed.kind))
        .label("Kind of lesson")
        .fit()
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            if let Some(ed) = s.notebook.editor.as_mut() {
                ed.kind = Kind::ALL[i];
            }
        })
        .build(k);

    let tags_input = TextInput::new(&ed.tags)
        .placeholder("sqlite, locks, layout")
        .label("Tags")
        .width(None)
        .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.tags))
        .on_submit(|s: &mut State, _| s.notebook_save())
        .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
        .build(k)
        .key("nb-f-tags");

    let prop_row = |label: &str, control: El| {
        row()
            .w_full()
            .min_h(32.0)
            .gap(12.0)
            .center_y()
            .child(text(label.to_string()).font(style::META).color(p.text_3).w(84.0))
            .child(row().grow(1.0).min_w(0.0).child(control))
    };

    let props = col()
        .w_full()
        .py(12.0)
        .gap(8.0)
        .child(prop_row("Topic", topic_cell))
        .child(prop_row("Kind", kind_pick))
        .child(prop_row("Tags", tags_input));

    let what_area = TextArea::new(&ed.what)
        .placeholder("What went wrong, with the exact error text")
        .label("What happened")
        .width(None)
        .auto_grow(3, 12)
        .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.what))
        .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
        .build(k)
        .key("nb-f-what");

    let why_area = TextArea::new(&ed.why)
        .placeholder("Why it happened (optional)")
        .label("Why")
        .width(None)
        .auto_grow(2, 8)
        .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.why))
        .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
        .build(k)
        .key("nb-f-why");

    let rule_area = TextArea::new(&ed.rule)
        .placeholder("What to do next time")
        .label("Takeaway")
        .width(None)
        .auto_grow(2, 6)
        .on_edit(|s: &mut State| editor_field(s, |ed| &mut ed.rule))
        .on_key(|s: &mut State, e: &KeyEvent, _| editor_key(s, e))
        .build(k)
        .key("nb-f-rule");

    let field_head = |title: &str, count: String| {
        row()
            .w_full()
            .center_y()
            .child(text(title.to_string()).font(style::HEADING).color(p.text))
            .child(rune::spacer())
            .child(text(count).font(style::META).color(p.text_3))
    };

    let takeaway_box = row()
        .w_full()
        .mt(22.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .items_stretch()
        .child(el().w(3.0).my(12.0).ml(14.0).radius(1.5).fill(ink))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .pl(12.0)
                .pr(16.0)
                .py(14.0)
                .gap(8.0)
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .child(text("Takeaway").font(style::CONTROL).color(ink))
                        .child(rune::spacer())
                        .child(text(counter(&ed.rule.text, *feeder_core::RULE_CHARS.end())).font(style::META).color(p.text_3)),
                )
                .child(rule_area),
        );

    let mut body = col()
        .key("nb-editor")
        .w_full()
        .max_w(DOC_MAX_W)
        .transition(Transition::opacity())
        .child(top_bar)
        .child(title_input)
        .child(
            row()
                .w_full()
                .mt(4.0)
                .child(text(ed.kind.hint()).font(style::META).color(p.text_3))
                .child(rune::spacer())
                .child(text(counter(&ed.title.text, *feeder_core::TITLE_CHARS.end())).font(style::META).color(p.text_3)),
        )
        .child(Divider::horizontal().build(k).mt(14.0))
        .child(props)
        .child(Divider::horizontal().build(k))
        .child(
            col()
                .w_full()
                .mt(22.0)
                .gap(8.0)
                .child(field_head("What happened", counter(&ed.what.text, *feeder_core::CONTEXT_CHARS.end())))
                .child(what_area),
        )
        .child(
            col()
                .w_full()
                .mt(18.0)
                .gap(8.0)
                .child(field_head("Why (optional)", counter(&ed.why.text, feeder_core::MAX_CAUSE_CHARS)))
                .child(why_area),
        )
        .child(takeaway_box);

    if let Some(err) = &ed.error {
        body = body.child(text(err.clone()).font(style::META).color(p.amber).wrap().mt(14.0));
    }

    body.child(
        row()
            .w_full()
            .mt(20.0)
            .gap(8.0)
            .center_y()
            .child(text("⌘↩ to save · Esc to cancel").font(style::META).color(p.text_3))
            .child(rune::spacer())
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press(|s: &mut State| s.notebook.editor = None)
                    .build(k),
            )
            .child(
                Button::new(if ed.id.is_some() { "Save Revision" } else { "Add Page" })
                    .variant(ButtonVariant::Contrast)
                    .control_size(ControlSize::Regular)
                    .on_press(|s: &mut State| s.notebook_save())
                    .build(k),
            ),
    )
}

fn editor_key(s: &mut State, e: &KeyEvent) -> bool {
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
    if matches!(e.key, Key::Enter) && e.mods.cmd {
        s.notebook_save();
        return true;
    }
    false
}

fn counter(text: &str, max: usize) -> String {
    format!("{}/{max}", text.trim().chars().count())
}

fn empty_notebook(p: &Palette, k: &Kit) -> El {
    col()
        .key("nb-empty")
        .w_full()
        .items_center()
        .pt(88.0)
        .gap(14.0)
        .child(text("The notebook is empty").font(style::DISPLAY).color(p.text))
        .child(
            text("Agents add a page here when they fix a hard bug, hit a dead end, or settle a design decision.")
                .font(style::PROSE)
                .color(p.text_2)
                .wrap()
                .max_w(480.0)
                .align(rune::Align::Center),
        )
        .child(
            Button::new("Write the First Page")
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
            context: "Something happened while building.".into(),
            cause: None,
            rule: "Do the careful thing next time.".into(),
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

        // Watcher pushes updated pages — topic:rust should remain collapsed.
        nb.set_pages(nb.pages.clone());
        assert!(!nb.tree.is_expanded("topic:rust"));

        // Searching expands matching topics via TreeModel filter, clearing restores collapse.
        nb.query = TextEdit::new("scroll");
        nb.apply_tree_filter();
        assert!(nb.tree.is_filtering());
        assert_eq!(nb.tree.visible_len(), 2); // topic:rust + page:3
        nb.query = TextEdit::new("");
        nb.apply_tree_filter();
        assert!(!nb.tree.is_expanded("topic:rust"));
    }

    #[test]
    fn escape_unwinds_menu_editor_page_then_search() {
        let mut s = State::new();
        s.notebook = notebook();
        s.notebook.query = TextEdit::new("rune");
        s.notebook.apply_tree_filter();
        s.notebook.open = Some(3);
        s.notebook.action_menu_open = true;
        assert!(s.notebook_escape() && !s.notebook.action_menu_open && s.notebook.open == Some(3));
        s.notebook_edit(3);
        assert!(s.notebook.editor.as_ref().is_some_and(|e| e.id == Some(3) && e.title.text.starts_with("Scroll")));
        assert!(s.notebook_escape() && s.notebook.editor.is_none() && s.notebook.open == Some(3));
        assert!(s.notebook_escape() && s.notebook.open.is_none());
        assert!(s.notebook_escape() && s.notebook.query.text.is_empty());
        assert!(!s.notebook_escape());
    }
}
