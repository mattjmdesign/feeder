//! One project's feed: the orb & brief up top, then every update newest first along a
//! timeline (optionally only updates with media), plus inline edit, two-way replies, and video playback.

use feeder_core::{AttentionKind, FeedUpdate, Orb, ProjectSummary, ago, day_key, day_label, parse_attention};
use rune::{
    Cursor, DragItem, El, GrowthSpec, Key, KeyEvent, ScrollSpec, Spring, SurfaceFit, Transition, Tx, auto_grow, col, el, memo, oklch, row,
    text, text_input,
};
use rune_kit::search::Fold;
use rune_kit::{Button, ButtonVariant, Kit, Markdown, MarkdownStyle, Menu, MenuItem, SegmentedControl, scroll_bar};

use crate::files::ProjectView;
use crate::style::{self, Palette};
use crate::{CopyTarget, State, Tab, agents, chrome, orb, prose};

const ORB: f32 = 84.0;
const COLUMN: f32 = 660.0;
const SHOT_MAX_H: f32 = 520.0;

pub fn visible_feed(s: &State) -> Vec<&FeedUpdate> {
    let query = s.project_search.text.trim();
    let fold = Fold::default();
    s.feed
        .iter()
        .filter(|u| match &s.branch_filter {
            Some(b) => u.branch.as_deref() == Some(b.as_str()),
            None => true,
        })
        .filter(|u| !s.project_media_only || u.has_media())
        .filter(|u| {
            query.is_empty()
                || fold.contains(&u.body, query)
                || fold.contains(&u.agent, query)
                || u.branch.as_deref().is_some_and(|b| fold.contains(b, query))
        })
        .collect()
}

pub fn bar_chrome(s: &State, id: i64, p: &Palette, k: &Kit) -> (El, Option<El>) {
    let (back_text, back_a11y) = match s.tab {
        Tab::Home => ("‹  Home", "Back to Home"),
        Tab::Feed => ("‹  Feed", "Back to Feed"),
        Tab::Notebook => ("‹  Notebook", "Back to Notebook"),
    };
    let back = chrome::bar_button("back", back_text, p).label(back_a11y).on_press(|s: &mut State| s.close_project());

    let Some(summary) = s.summary(id) else {
        return (back, None);
    };
    let archived = summary.project.archived;
    // Show in Finder and Pin are on the project's orb menu on Home, so the header doesn't repeat
    // them; Copy Context covers what the old resume prompt did.
    let mut actions = row().gap(4.0).center_y();
    // Updates is what agents said; Files is what changed on disk.
    let views = [ProjectView::Updates, ProjectView::Files];
    actions = actions.child(
        SegmentedControl::new(&["Updates", "Files"], views.iter().position(|v| *v == s.files.view))
            .label("Project view")
            .fit()
            .on_pick(move |s: &mut State, i: usize, _: &mut Tx| {
                s.files.view = views.get(i).copied().unwrap_or_default();
                s.ask();
            })
            .build(k)
            .mr(8.0),
    );
    if compact_toolbar(s) {
        return (back, Some(actions));
    }
    if !archived {
        actions = actions.child(
            chrome::bar_button("project-curator-btn", "Update Notes", p)
                .label("Run Curator for this project")
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_project_curator(id, tx)),
        );
    }
    if s.feed_for == Some(id) && !s.feed.is_empty() {
        let context_label = if s.is_copied(CopyTarget::ProjectContext) { "Copied" } else { "Copy Context" };
        actions = actions.child(chrome::bar_button("copy-context", context_label, p).on_press_tx(move |s: &mut State, tx: &mut Tx| {
            copy_project_context(s, id, tx);
        }));
    }
    actions = actions
        .child(chrome::bar_button("rename-button", if s.editing.is_some() { "Done" } else { "Rename" }, p).on_press_tx(rename_project))
        .child(
            chrome::bar_button("archive-button", if archived { "Unarchive" } else { "Archive" }, p)
                .on_press(move |s: &mut State| s.set_archived(id, !archived)),
        );
    (back, Some(actions))
}

fn compact_toolbar(s: &State) -> bool {
    s.window_size[0] < 800.0
}

pub fn project_menu(s: &State, mut menu: Menu) -> Menu {
    let Some(id) = s.open.filter(|_| compact_toolbar(s)) else { return menu };
    let Some(summary) = s.summary(id) else { return menu };
    let archived = summary.project.archived;
    if !archived {
        menu = menu.item(MenuItem::new("Update Notes").on_select_tx(move |s: &mut State, tx: &mut Tx| {
            s.menu_open = false;
            s.start_project_curator(id, tx);
        }));
    }
    if s.feed_for == Some(id) && !s.feed.is_empty() {
        menu = menu.item(MenuItem::new("Copy Context").on_select_tx(move |s: &mut State, tx: &mut Tx| {
            s.menu_open = false;
            copy_project_context(s, id, tx);
        }));
    }
    let rename = if s.editing.is_some() { "Done Renaming" } else { "Rename" };
    let archive = if archived { "Unarchive" } else { "Archive" };
    menu.item(MenuItem::new(rename).on_select_tx(|s: &mut State, tx: &mut Tx| {
        s.menu_open = false;
        rename_project(s, tx);
    }))
    .item(MenuItem::new(archive).on_select(move |s: &mut State| {
        s.menu_open = false;
        s.set_archived(id, !archived);
    }))
    .separator()
}

fn copy_project_context(s: &mut State, id: i64, tx: &mut Tx) {
    if let Some(summary) = s.summary(id) {
        tx.copy(handoff_markdown(summary, &s.feed, s.now));
        s.mark_copied(CopyTarget::ProjectContext);
    }
}

fn rename_project(s: &mut State, tx: &mut Tx) {
    if s.editing.is_some() {
        s.commit_rename();
    } else {
        if s.files.view == ProjectView::Files {
            s.files.view = ProjectView::Updates;
            s.ask();
        }
        s.start_rename(tx);
    }
}

pub fn view(s: &State, id: i64, p: &Palette, k: &Kit, reduced_motion: bool) -> El {
    let Some(summary) = s.summary(id) else {
        return col().key("feed").w_full().grow(1.0).min_h(0.0);
    };
    let archived = summary.project.archived;
    let pinned = summary.project.pinned;

    let count = match summary.updates {
        1 => "1 update".to_string(),
        n => format!("{n} updates"),
    };
    let mut meta = row().gap(7.0).center_y().child(text(count).font(style::META).color(p.text_3));
    if let (Some(at), Some(agent)) = (summary.last_at, &summary.last_agent) {
        meta = meta
            .child(text("·").font(style::META).color(p.text_3))
            .child(text(format!("last {}", ago(at, s.now))).font(style::META).color(p.text_3))
            .child(agents::mark(agent, p, 13.0, s.agent_icon(agent)));
    }
    if summary.needs_you && !archived {
        meta = meta.child(text("·").font(style::META).color(p.text_3)).child(attention_badge(p, None));
    }
    if let Some(pulse) = &summary.active_pulse
        && !archived
    {
        meta = meta.child(text("·").font(style::META).color(p.text_3)).child(
            row()
                .px(7.0)
                .py(1.5)
                .radius(6.0)
                .gap(5.0)
                .center_y()
                .fill(p.live.alpha(0.14))
                .child(el().size(5.0, 5.0).radius(2.5).fill(p.live))
                .child(text(pulse.status.clone()).font(style::META).color(p.live).truncate().max_w(180.0)),
        );
    }
    if pinned {
        meta = meta
            .child(text("·").font(style::META).color(p.text_3))
            .child(el().px(7.0).py(1.5).radius(6.0).border(1.0, p.hairline).child(text("Pinned").font(style::META).color(p.text_2)));
    }
    if archived {
        meta = meta
            .child(text("·").font(style::META).color(p.text_3))
            .child(el().px(7.0).py(1.5).radius(6.0).border(1.0, p.hairline).child(text("Archived").font(style::META).color(p.text_3)));
    }

    let orb_el = orb::orb(&summary.project.orb, ORB);
    let mut orb_box = el().key(format!("orb-{id}")).matched(format!("orb-{id}")).size(ORB, ORB).mb(14.0).motion(Spring::new(0.5, 0.18));
    if summary.active_pulse.is_some() && !archived {
        let phase = if reduced_motion { 0.65 } else { ((s.clock * 1.4).sin() as f32) * 0.5 + 0.5 };
        orb_box = orb_box.child(orb::pulse_halo(&summary.project.orb, ORB, phase));
    }
    orb_box = orb_box.child(if archived { orb_el.opacity(0.68) } else { orb_el });

    let mut intro =
        col().key("intro").w_full().items_center().gap(8.0).pb(24.0).child(orb_box).child(title(s, p, &summary.project.name)).child(meta);
    if let Some(path) = &summary.project.path {
        intro = intro.child(text(style::home_relative(path)).font(style::PATH).color(p.text_3).truncate().max_w(COLUMN));
    }
    intro = intro_extras(s, p, k, summary, intro);

    let md = markdown_style(p, k);
    let mut timeline = col().key("timeline").w_full();
    if s.feed_for == Some(id) {
        let has_media = s.feed.iter().any(FeedUpdate::has_media);
        let search_active = s.project_search_open || !s.project_search.text.is_empty();
        if has_media || s.branch_filter.is_some() || search_active {
            timeline = timeline.child(project_toolbar(s, p, has_media));
        }

        let visible = visible_feed(s);
        {
            let n = visible.len();
            if n > 0 {
                let mut rows = col().key("project-posts").w_full().growth(GrowthSpec::default()).virtualize(260.0).on_visible_rows(
                    |s: &mut State, range| {
                        s.project_visible_rows = Some(range);
                        crate::media::sync(s);
                        false
                    },
                );
                let mut prev_day: Option<i64> = None;
                for (i, u) in visible.iter().enumerate() {
                    let dk = day_key(u.created_at);
                    let mut row_el = col().key(format!("u-{}", u.id)).w_full();
                    if prev_day != Some(dk) {
                        row_el = row_el.child(day_divider(day_label(u.created_at, s.now), p, i > 0));
                        prev_day = Some(dk);
                    }
                    let same_next = visible.get(i + 1).is_some_and(|next| day_key(next.created_at) == dk);
                    rows = rows.child(row_el.child(entry(s, (p, k, &md), u, &summary.project.orb, i == 0, !same_next)));
                }
                timeline = timeline.child(rows);
            } else {
                let empty_msg = if !s.project_search.text.trim().is_empty() {
                    "No updates match this filter."
                } else if s.project_media_only {
                    "No updates with media yet."
                } else if s.branch_filter.is_some() {
                    "No updates on this branch."
                } else {
                    "No updates yet. Ask an agent in this project to post a Feeder update."
                };
                timeline = timeline.child(text(empty_msg).font(style::META).color(p.text_3).self_center().py(16.0));
            }
            if s.branch_filter.is_none()
                && !s.project_media_only
                && s.project_search.text.trim().is_empty()
                && s.feed.len() >= s.limit
                && (s.feed.len() as i64) < summary.updates
            {
                timeline = timeline.child(
                    chrome::bar_button("older", "Show Older", p)
                        .self_center()
                        .mt(12.0)
                        .border(1.0, p.hairline)
                        .on_press(|s: &mut State| s.show_more()),
                );
            }
        }
    }

    let column = col().key("column").w_full().max_w(COLUMN).child(intro).child(timeline);
    let viewport = col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(col().w_full().items_center().px(28.0).pt(36.0).pb(80.0).child(column))
        .scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(24.0);
    let scroller = scroll_bar(k, "feed-scroll-area", &format!("feed-scroll-{id}"), viewport).w_full().grow(1.0).min_h(0.0);

    col().key("feed").w_full().grow(1.0).min_h(0.0).transition(Transition::none()).child(scroller)
}

fn intro_extras(s: &State, p: &Palette, k: &Kit, summary: &ProjectSummary, intro: El) -> El {
    let id = summary.project.id;
    let has_brief = s.brief_editing == Some(id) || summary.project.brief.as_deref().map(str::trim).is_some_and(|b| !b.is_empty());
    if !has_brief {
        return intro.child(
            row().key("intro-actions").mt(4.0).center_y().child(
                chrome::bar_button("add-brief", "+ Add Project Brief", p)
                    .border(1.0, p.hairline)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_brief_edit(id, tx)),
            ),
        );
    }

    intro.child(brief_card(s, p, k, summary))
}

fn brief_card(s: &State, p: &Palette, k: &Kit, summary: &ProjectSummary) -> El {
    let id = summary.project.id;
    if s.brief_editing == Some(id) {
        let field = text_input(&s.brief_draft, style::BODY)
            .key("brief-input")
            .label("Project brief")
            .grow(1.0)
            .min_w(0.0)
            .color(p.text)
            .caret(p.text, p.selection)
            .placeholder("Goal, current focus, and the commands that check work…", p.text_3)
            .on_edit(|s: &mut State| &mut s.brief_draft)
            .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
                if matches!(e.key, Key::Escape) {
                    s.brief_editing = None;
                    return true;
                }
                if matches!(e.key, Key::Enter) && e.mods.cmd {
                    s.save_brief();
                    return true;
                }
                false
            });
        return col()
            .key("brief-editor")
            .w_full()
            .max_w(COLUMN)
            .mt(10.0)
            .padding(14.0)
            .gap(10.0)
            .radius(12.0)
            .fill(p.card)
            .border(1.0, p.hairline)
            .child(
                row()
                    .w_full()
                    .center_y()
                    .child(text("PINNED BRIEF").font(style::LABEL).color(p.text_3))
                    .child(rune::spacer())
                    .child(text("⌘Enter to save").font(style::META).color(p.text_3)),
            )
            .child(el().w_full().padding(10.0).radius(8.0).fill(p.bg).border(1.0, p.hairline).child(auto_grow(field, 2, 6).w_full()))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(
                        Button::new("Clear")
                            .variant(ButtonVariant::Ghost)
                            .on_press(|s: &mut State| {
                                s.brief_draft.text.clear();
                                s.save_brief();
                            })
                            .build(k),
                    )
                    .child(rune::spacer())
                    .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.brief_editing = None).build(k))
                    .child(Button::new("Save Brief").variant(ButtonVariant::Submit).on_press(|s: &mut State| s.save_brief()).build(k)),
            );
    }

    if let Some(brief) = summary.project.brief.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        col()
            .key("brief-bio")
            .w_full()
            .max_w(540.0)
            .mt(6.0)
            .gap(4.0)
            .items_center()
            .child(text(brief.to_string()).font(style::BODY).color(p.text_2).align(rune::Align::Center).wrap().selectable(true))
            .child(
                row()
                    .key("edit-brief")
                    .role("button")
                    .label("Edit project brief")
                    .focusable(true)
                    .h(22.0)
                    .px(8.0)
                    .center_y()
                    .radius(6.0)
                    .hover(p.hover)
                    .cursor(Cursor::Pointer)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_brief_edit(id, tx))
                    .child(text("Edit brief").font(style::META).color(p.text_3)),
            )
    } else {
        chrome::bar_button("add-brief", "+ Add Project Brief", p)
            .mt(4.0)
            .border(1.0, p.hairline)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_brief_edit(id, tx))
    }
}

fn project_toolbar(s: &State, p: &Palette, has_media: bool) -> El {
    let chip = |key: &str, label: &str, active: bool| {
        row()
            .key(key.to_string())
            .role("button")
            .label(label.to_string())
            .focusable(true)
            .h(26.0)
            .px(10.0)
            .center_y()
            .radius(8.0)
            .fill(if active { p.hover } else { p.card.alpha(0.0) })
            .border(1.0, if active { p.text_3.alpha(0.45) } else { p.hairline })
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(style::META).color(if active { p.text } else { p.text_3 }))
    };

    let mut bar = row().key("project-toolbar").w_full().pb(14.0).gap(6.0).center_y().flex_wrap();
    if s.project_search_open || !s.project_search.text.is_empty() {
        let search = text_input(&s.project_search, style::META)
            .key("project-search")
            .label("Filter updates in this project")
            .single_line()
            .grow(1.0)
            .min_w(0.0)
            .color(p.text)
            .caret(p.text, p.selection)
            .placeholder("Filter updates in this project…  ⌘F", p.text_3)
            .on_edit(|s: &mut State| &mut s.project_search)
            .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
                if matches!(e.key, Key::Escape) {
                    if !s.project_search.text.is_empty() {
                        s.project_search.text.clear();
                    } else {
                        s.project_search_open = false;
                        s.project_search.focused = false;
                    }
                    return true;
                }
                false
            });
        let clear = row()
            .key("clear-project-search")
            .role("button")
            .label("Close project search")
            .size(18.0, 18.0)
            .center()
            .radius(5.0)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .on_press(|s: &mut State| {
                s.project_search.text.clear();
                s.project_search_open = false;
                s.project_search.focused = false;
            })
            .child(text("×").font(style::META).color(p.text_3));
        let search_box = row()
            .grow(1.0)
            .min_w(160.0)
            .h(26.0)
            .px(10.0)
            .gap(6.0)
            .center_y()
            .radius(8.0)
            .fill(p.card)
            .border(1.0, p.hairline)
            .child(search)
            .child(clear);
        bar = bar.child(search_box);
    }
    if has_media {
        bar = bar
            .child(chip("pv-all", "All", !s.project_media_only).on_press(|s: &mut State| s.project_media_only = false))
            .child(chip("pv-media", "With media", s.project_media_only).on_press(|s: &mut State| s.project_media_only = true));
    }
    if let Some(branch) = &s.branch_filter {
        bar = bar.child(chip("pv-branch-clear", &format!("Branch: {branch} ×"), true).on_press(|s: &mut State| s.branch_filter = None));
    }
    bar
}

pub fn day_divider(label: String, p: &Palette, spaced: bool) -> El {
    row()
        .w_full()
        .pt(if spaced { 10.0 } else { 0.0 })
        .pb(12.0)
        .center_y()
        .gap(12.0)
        .child(text(label.to_uppercase()).font(style::LABEL).color(p.text_3))
        .child(rune::rule(1.0, p.hairline).grow(1.0))
}

fn handoff_markdown(summary: &ProjectSummary, feed: &[FeedUpdate], now: i64) -> String {
    let mut out = format!("# {}\n", summary.project.name);
    for u in feed.iter().take(3) {
        let branch = u.branch.as_deref().map(|b| format!(" · `{b}`")).unwrap_or_default();
        out.push_str(&format!("\n## {} · {}{branch}\n{}\n", u.agent, ago(u.created_at, now), u.body));
    }
    out
}

/// The project's name, or the field renaming it. The path under it never changes, so a nickname
/// always traces back to its repository.
fn title(s: &State, p: &Palette, name: &str) -> El {
    let Some(edit) = &s.editing else {
        return text(name.to_string())
            .key("title")
            .font(style::TITLE)
            .color(p.text)
            .cursor(Cursor::Text)
            .on_double_press_tx(|s: &mut State, tx: &mut Tx| s.start_rename(tx));
    };
    let mut field = text_input(edit, style::TITLE)
        .key("rename")
        .label("Project name")
        .single_line()
        .max_w(420.0)
        .color(p.text)
        .caret(p.text, p.selection)
        .placeholder("Folder name", p.text_3)
        .on_edit(|s: &mut State| match s.editing.as_mut() {
            Some(edit) => edit,
            None => &mut s.scratch_edit,
        })
        .on_submit(|s: &mut State, _tx: &mut Tx| s.commit_rename())
        .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
            if matches!(e.key, Key::Escape) {
                s.cancel_rename();
                return true;
            }
            false
        });
    if edit.text.is_empty() {
        field = field.min_w(156.0);
    }
    row().px(14.0).py(3.0).radius(10.0).fill(p.card).border(1.0, p.hairline).center().child(field)
}

pub fn markdown_style(p: &Palette, k: &Kit) -> MarkdownStyle {
    MarkdownStyle::from_kit(k)
        .body_font(style::BODY)
        .text(p.text)
        .muted(p.text_2)
        .inline_code_bg(p.hover)
        .inline_code_font(0.9, 400.0)
        .strong_weight(style::MEDIUM)
        .heading_font(1, style::SECTION_TITLE)
        .heading_font(2, style::SECTION_TITLE)
        .heading_font(3, style::NAME)
        .heading_font(4, style::NAME)
        .list(18.0, 5.0)
        .rule(p.hairline)
}

pub fn first_line(body: &str, max_chars: usize) -> String {
    let raw = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").trim_start_matches("- ").trim_start_matches("* ");
    let clean: String = raw.replace("**", "").replace('`', "");
    if clean.chars().count() <= max_chars {
        return clean;
    }
    let truncated: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

pub fn branch_pill_clickable(s: &State, p: &Palette, branch: Option<&str>) -> Option<El> {
    let b = branch.map(str::trim).filter(|b| !b.is_empty())?;
    let active = s.branch_filter.as_deref() == Some(b);
    let owned = b.to_string();
    Some(
        el().role("button")
            .label(format!("Filter by branch {b}"))
            .focusable(true)
            .px(6.0)
            .py(1.5)
            .radius(5.0)
            .fill(p.hover)
            .border(1.0, if active { p.text_3.alpha(0.6) } else { p.hairline.alpha(0.0) })
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.toggle_branch_filter(&owned))
            .child(text(b.to_string()).font(style::PATH).color(if active { p.text } else { p.text_3 }).truncate().max_w(150.0)),
    )
}

pub fn diff_badge(p: &Palette, diff_stat: Option<&str>) -> Option<El> {
    let d = diff_stat.map(str::trim).filter(|d| !d.is_empty())?;
    Some(
        el().px(6.0)
            .py(1.5)
            .radius(5.0)
            .border(1.0, p.hairline)
            .child(text(d.to_string()).font(style::PATH).color(p.text_3).truncate().max_w(130.0)),
    )
}

pub fn attention_badge(p: &Palette, kind: Option<AttentionKind>) -> El {
    let label = match kind {
        Some(AttentionKind::Blocked) => "Blocked",
        Some(AttentionKind::DecisionNeeded) => "Decision needed",
        None => "Needs you",
    };
    row()
        .px(6.0)
        .py(1.5)
        .radius(5.0)
        .gap(5.0)
        .center_y()
        .fill(p.amber.alpha(0.14))
        .child(el().size(5.0, 5.0).radius(2.5).fill(p.amber))
        // Amber stays in the dot and tint; the label uses body ink so it holds 4.5:1 on the tint.
        .child(text(label).font(style::META).color(p.text))
}

/// Renders the attention status for a card:
/// - open (latest update, unanswered, not dismissed): amber badge + one-click Dismiss button
/// - answered by a human reply: quiet muted "Answered" pill
/// - dismissed or superseded by a newer update: nothing
pub fn card_attention_tags(s: &State, p: &Palette, u: &FeedUpdate) -> Vec<El> {
    let Some(kind) = parse_attention(&u.body) else {
        return vec![];
    };
    if s.is_open_attention(u) {
        let uid = u.id;
        let dismiss = row()
            .key(format!("dismiss-u-{uid}"))
            .role("button")
            .label("Dismiss needs you")
            .focusable(true)
            .px(7.0)
            .py(1.5)
            .radius(5.0)
            .center_y()
            .border(1.0, p.hairline)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.dismiss_attention(uid))
            .child(text("Dismiss").font(style::META).color(p.text_3));
        return vec![attention_badge(p, Some(kind)), dismiss];
    }
    if s.has_human_reply(u.id) {
        return vec![el().px(6.0).py(1.5).radius(5.0).border(1.0, p.hairline).child(text("Answered").font(style::META).color(p.text_3))];
    }
    vec![]
}

pub fn card_actions(s: &State, p: &Palette, u: &FeedUpdate) -> El {
    let uid = u.id;
    let body = u.body.clone();
    let copy_label = if s.is_copied(CopyTarget::Update(uid)) { "Copied" } else { "Copy" };
    let small_btn = |key: String, label: &str, a11y: &str| {
        row()
            .key(key)
            .role("button")
            .label(a11y.to_string())
            .focusable(true)
            .h(22.0)
            .px(7.0)
            .center_y()
            .radius(6.0)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(style::META).color(p.text_3))
    };

    let copy_btn = small_btn(format!("copy-u-{uid}"), copy_label, "Copy update").on_press_tx(move |s: &mut State, tx: &mut Tx| {
        tx.copy(body.clone());
        s.mark_copied(CopyTarget::Update(uid));
    });
    let edit_btn = small_btn(format!("edit-u-{uid}"), if s.editing_update == Some(uid) { "Editing" } else { "Edit" }, "Edit update")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_edit_update(uid, tx));
    let reply_btn = small_btn(format!("reply-u-{uid}"), "Reply", "Reply to update")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_reply(uid, tx));
    let flag_btn =
        small_btn(format!("flag-u-{uid}"), "Flag", "Flag for Notebook").on_press(move |s: &mut State| s.flag_update_for_notebook(uid));
    let delete_btn =
        small_btn(format!("del-u-{uid}"), "Delete", "Delete update").on_press(move |s: &mut State| s.delete_confirm = Some(uid));

    row().gap(2.0).center_y().child(copy_btn).child(edit_btn).child(reply_btn).child(flag_btn).child(delete_btn)
}

/// While this update's flag waits for Curator: a quiet "Flagged" pill that opens the review.
/// Once Curator writes the note, `notebook_link` takes over, so the card always shows where it is.
pub fn flag_pill(s: &State, p: &Palette, u: &FeedUpdate) -> Option<El> {
    if !s.open_flag_for_update(u.id) {
        return None;
    }
    let (uid, project_id) = (u.id, u.project_id);
    Some(
        row()
            .key(format!("flagged-u-{uid}"))
            .role("button")
            .label("Flagged for Notebook, review with Curator")
            .help("Review with Curator")
            .focusable(true)
            .px(7.0)
            .py(1.5)
            .radius(5.0)
            .center_y()
            .border(1.0, p.hairline)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_flag_review(Some(project_id), tx))
            .child(text("Flagged · Review").font(style::META).color(p.text_2)),
    )
}

/// A quiet link to the Notebook page written from this Feed update, when one exists.
pub fn notebook_link(s: &State, p: &Palette, update_id: i64) -> Option<El> {
    let change = s.notebook.gardener.changes.iter().find(|c| !c.is_reverted() && c.source_updates.contains(&update_id))?;
    let page_id = change.page_id;
    let title = change.page_title.clone();
    Some(
        row()
            .key(format!("nb-link-{update_id}-{page_id}"))
            .role("button")
            .label(format!("Open Notebook note {title}"))
            .help(format!("Open \"{title}\""))
            .focusable(true)
            .px(7.0)
            .py(1.5)
            .radius(5.0)
            .gap(5.0)
            .center_y()
            .border(1.0, p.hairline)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.open_notebook_page_from_anywhere(page_id))
            .child(text("In Notebook · Open").font(style::META).color(p.text_2)),
    )
}

fn entry(s: &State, (p, k, md): (&Palette, &Kit, &MarkdownStyle), u: &FeedUpdate, orb: &Orb, latest: bool, last: bool) -> El {
    let accent = orb::accent(orb);
    let selected = s.feed_selected == Some(u.id);
    let dot_size = if latest { 11.0 } else { 7.0 };
    let dot = el().size(dot_size, dot_size).radius(dot_size / 2.0).fill(if latest { accent } else { p.text_3.alpha(0.6) });
    let rail = col().w(24.0).items_center().pt(if latest { 21.0 } else { 23.0 }).gap(8.0).child(dot).child(if last {
        el().grow(1.0)
    } else {
        el().w(1.0).grow(1.0).fill(p.hairline)
    });

    let who = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(agents::mark(&u.agent, p, 16.0, s.agent_icon(&u.agent)))
        .child(text(u.agent.clone()).font(style::AGENT).color(p.text))
        .child(rune::spacer())
        .child(card_actions(s, p, u))
        .child(text("·").font(style::META).color(p.text_3))
        .child(text(ago(u.created_at, s.now)).font(style::META).color(p.text_3));

    let mut tags = row().w_full().flex_wrap().gap(6.0).center_y();
    let mut has_tags = false;
    if let Some(pill) = branch_pill_clickable(s, p, u.branch.as_deref()) {
        tags = tags.child(pill);
        has_tags = true;
    }
    if let Some(diff) = diff_badge(p, u.diff_stat.as_deref()) {
        tags = tags.child(diff);
        has_tags = true;
    }
    for badge in card_attention_tags(s, p, u) {
        tags = tags.child(badge);
        has_tags = true;
    }
    if let Some(flag) = flag_pill(s, p, u) {
        tags = tags.child(flag);
        has_tags = true;
    }

    let border_color = if selected {
        accent.alpha(0.75)
    } else if latest {
        accent.alpha(0.35)
    } else {
        p.hairline
    };
    let mut card = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(12.0)
        .padding(20.0)
        .radius(14.0)
        .fill(p.card)
        .border(if selected { 1.5 } else { 1.0 }, border_color)
        .child(who);
    if has_tags {
        card = card.child(tags);
    }
    card = card.child(rune::rule(1.0, p.hairline)).child(edit_or_body(s, p, k, md, u, latest));
    if let Some(media) = media_view(s, p, u, COLUMN - 24.0 - 12.0 - 40.0) {
        card = card.child(media);
    }
    if let Some(link) = notebook_link(s, p, u.id) {
        card = card.child(row().w_full().child(link));
    }
    if let Some(replies) = replies_section(s, p, k, u) {
        card = card.child(replies);
    }

    row().w_full().gap(12.0).pb(20.0).items_stretch().child(rail).child(card)
}

pub fn edit_or_body(s: &State, p: &Palette, k: &Kit, md: &MarkdownStyle, u: &FeedUpdate, strong: bool) -> El {
    let uid = u.id;
    if s.editing_update == Some(uid) {
        let field = text_input(&s.edit_update_body, style::BODY)
            .key(format!("edit-body-{uid}"))
            .label("Edit update")
            .grow(1.0)
            .min_w(0.0)
            .color(p.text)
            .caret(p.text, p.selection)
            .on_edit(|s: &mut State| &mut s.edit_update_body)
            .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
                if matches!(e.key, Key::Escape) {
                    s.editing_update = None;
                    return true;
                }
                if matches!(e.key, Key::Enter) && e.mods.cmd {
                    s.save_update_edit();
                    return true;
                }
                false
            });
        return col()
            .w_full()
            .gap(10.0)
            .child(el().w_full().padding(10.0).radius(8.0).fill(p.bg).border(1.0, p.hairline).child(auto_grow(field, 3, 10).w_full()))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(text("⌘Enter to save").font(style::META).color(p.text_3))
                    .child(rune::spacer())
                    .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.editing_update = None).build(k))
                    .child(
                        Button::new("Save")
                            .variant(ButtonVariant::Submit)
                            .disabled(s.edit_update_body.text.trim().is_empty())
                            .on_press(|s: &mut State| s.save_update_edit())
                            .build(k),
                    ),
            );
    }
    memo(format!("f-body-{uid}"), (p.dark, strong, &u.body), || body(p, md, u, strong))
}

/// An update's text: each paragraph under its label, as Markdown.
pub fn body(p: &Palette, md: &MarkdownStyle, u: &FeedUpdate, strong: bool) -> El {
    let mut body = col().w_full().gap(11.0);
    for (i, section) in prose::sections(&u.body).into_iter().enumerate() {
        let mut block = col().key(format!("s{i}")).w_full().gap(4.0);
        if let Some(label) = section.label {
            block = block.child(text(label.to_uppercase()).font(style::LABEL).color(p.text_3));
        }
        let rendered = Markdown::new(section.markdown)
            .selectable(true)
            .on_link_tx(|s: &mut State, url: &str, tx: &mut Tx| {
                if !s.open_feeder_url(url) {
                    let _ = tx.open_url(url);
                }
            })
            .render(md);
        block = block.child(if strong { rendered } else { rendered.opacity(0.88) });
        body = body.child(block);
    }
    body
}

pub fn replies_section(s: &State, p: &Palette, k: &Kit, u: &FeedUpdate) -> Option<El> {
    let uid = u.id;
    let replies = s.replies.get(&uid).map(Vec::as_slice).unwrap_or(&[]);
    let replying = s.replying_to == Some(uid);
    if replies.is_empty() && !replying {
        return None;
    }

    let mut box_el = col().w_full().gap(8.0).pt(6.0).child(rune::rule(1.0, p.hairline));
    for r in replies {
        let rid = r.id;
        let reply_body = r.body.clone();
        let mut meta = row().w_full().gap(6.0).center_y().child(text(r.author.clone()).font(style::AGENT).color(if r.from_human {
            p.text
        } else {
            p.text_2
        }));
        if r.from_human && r.read_at.is_none() {
            meta = meta.child(
                el().px(5.0).py(1.0).radius(4.0).fill(p.amber.alpha(0.14)).child(text("Waiting on agent").font(style::META).color(p.amber)),
            );
        }
        meta = meta.child(rune::spacer());
        if r.from_human {
            let copy_label = if s.is_copied(CopyTarget::Reply(rid)) { "Copied" } else { "Copy" };
            meta = meta.child(
                row()
                    .key(format!("copy-r-{rid}"))
                    .role("button")
                    .label("Copy reply")
                    .focusable(true)
                    .h(20.0)
                    .px(6.0)
                    .center_y()
                    .radius(5.0)
                    .hover(p.bg)
                    .cursor(Cursor::Pointer)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        tx.copy(reply_body.clone());
                        s.mark_copied(CopyTarget::Reply(rid));
                    })
                    .child(text(copy_label).font(style::META).color(p.text_3)),
            );
        }
        meta = meta.child(text(ago(r.created_at, s.now)).font(style::META).color(p.text_3));
        box_el = box_el.child(
            col()
                .w_full()
                .gap(3.0)
                .px(10.0)
                .py(8.0)
                .radius(8.0)
                .fill(p.hover)
                .child(meta)
                .child(text(r.body.clone()).font(style::META).color(p.text).wrap().selectable(true)),
        );
    }

    if replying {
        let input = text_input(&s.reply_draft, style::META)
            .key(format!("reply-input-{uid}"))
            .label("Write a reply")
            .single_line()
            .grow(1.0)
            .min_w(0.0)
            .color(p.text)
            .caret(p.text, p.selection)
            .placeholder("Reply or decide for the agent…", p.text_3)
            .on_edit(|s: &mut State| &mut s.reply_draft)
            .on_submit(move |s: &mut State, _tx: &mut Tx| s.submit_reply(uid))
            .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
                if matches!(e.key, Key::Escape) {
                    s.replying_to = None;
                    return true;
                }
                false
            });
        box_el = box_el.child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(row().grow(1.0).min_w(0.0).h(30.0).px(10.0).center_y().radius(8.0).fill(p.bg).border(1.0, p.hairline).child(input))
                .child(
                    Button::new("Send")
                        .variant(ButtonVariant::Submit)
                        .disabled(s.reply_draft.text.trim().is_empty())
                        .on_press(move |s: &mut State| s.submit_reply(uid))
                        .build(k),
                )
                .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.replying_to = None).build(k)),
        );
    }

    Some(box_el)
}

/// An update's attached screenshot(s) or video clip(s) at `width`.
/// Single attachments render directly; multiple attachments render an interactive carousel
/// with left/right arrows, a top-right `1 / N` badge, and clickable slide dots.
pub fn media_view(s: &State, p: &Palette, u: &FeedUpdate, width: f32) -> Option<El> {
    let slides = crate::media_slides(u);
    if slides.is_empty() {
        return None;
    }
    let uid = u.id;
    let total = slides.len();
    let idx = s.card_slide_index(uid, total);
    let slide = &slides[idx];
    let (slide_el, frame_h) = render_slide(s, p, slide, width)?;

    if total == 1 {
        return Some(slide_el);
    }

    let btn_y = ((frame_h - 30.0) * 0.5).max(8.0);
    let arrow_btn = |key: String, glyph: &'static str, label: &'static str, delta: isize| {
        el().key(key)
            .role("button")
            .label(label)
            .focusable(true)
            .size(30.0, 30.0)
            .radius(15.0)
            .fill(oklch(0.12, 0.0, 0.0).alpha(0.72))
            .hover(oklch(0.12, 0.0, 0.0).alpha(0.90))
            .border(1.0, oklch(1.0, 0.0, 0.0).alpha(0.20))
            .center()
            .cursor(Cursor::Pointer)
            .press_scale(0.93)
            .on_press(move |s: &mut State| s.step_card_slide(uid, delta, total))
            .child(text(glyph).font(style::NAME).color(oklch(0.98, 0.0, 0.0)))
    };

    let counter = el()
        .key(format!("car-count-{uid}"))
        .pin(None, Some(10.0), Some(10.0), None)
        .px(8.0)
        .py(3.0)
        .radius(99.0)
        .fill(oklch(0.10, 0.0, 0.0).alpha(0.72))
        .border(1.0, oklch(1.0, 0.0, 0.0).alpha(0.18))
        .child(text(format!("{} / {total}", idx + 1)).font(style::META).color(oklch(0.96, 0.0, 0.0)));

    let frame = el()
        .key(format!("car-frame-{uid}"))
        .w_full()
        .h(frame_h)
        .child(slide_el)
        .child(counter)
        .child(arrow_btn(format!("car-prev-{uid}"), "‹", "Previous slide", -1).absolute(10.0, btn_y))
        .child(arrow_btn(format!("car-next-{uid}"), "›", "Next slide", 1).pin(None, Some(btn_y), Some(10.0), None));

    // Each dot's hit area is at least 24 x 24 pt (WCAG 2.5.8); the visible dot stays small.
    let mut dots = row().key(format!("car-dots-{uid}")).w_full().center().pt(2.0);
    for i in 0..total {
        let active = i == idx;
        dots = dots.child(
            el().key(format!("car-dot-{uid}-{i}"))
                .role("button")
                .label(format!("Show slide {}", i + 1))
                .focusable(true)
                .h(24.0)
                .min_w(24.0)
                .px(4.0)
                .center()
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| s.set_card_slide(uid, i))
                .child(
                    el().w(if active { 14.0 } else { 5.0 })
                        .h(5.0)
                        .radius(2.5)
                        .fill(if active { p.text_2 } else { p.hairline })
                        .hover(p.text_3),
                ),
        );
    }

    Some(col().key(format!("car-{uid}")).w_full().child(frame).child(dots))
}

fn render_slide(s: &State, p: &Palette, slide: &crate::MediaSlide, width: f32) -> Option<(El, f32)> {
    let mid = slide.id;
    if let Some(player) = s.videos.get(&mid) {
        let (vw, vh) = player.natural_size().unwrap_or((16, 9));
        let ratio = vw.max(1) as f32 / vh.max(1) as f32;
        let natural_h = (width / ratio).clamp(180.0, SHOT_MAX_H);
        let playing = player.is_playing();
        let node = el()
            .key(format!("vid-{mid}"))
            .w_full()
            .h(natural_h)
            .radius(10.0)
            .border(1.0, p.hairline)
            .fill(p.bg.alpha(0.5))
            .video(player, SurfaceFit::Fit)
            .child(
                row()
                    .pin(Some(10.0), Some(10.0), None, None)
                    .gap(6.0)
                    .child(
                        chrome::bar_button(&format!("vid-toggle-{mid}"), if playing { "Pause" } else { "Play" }, p)
                            .fill(p.card.alpha(0.92))
                            .border(1.0, p.hairline)
                            .on_press(move |s: &mut State| s.toggle_video(mid)),
                    )
                    .child(
                        chrome::bar_button(&format!("vid-expand-{mid}"), "Expand", p)
                            .fill(p.card.alpha(0.92))
                            .border(1.0, p.hairline)
                            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_lightbox(mid, tx)),
                    ),
            );
        return Some((node, natural_h));
    }

    let path = slide.path.clone();
    let button = el()
        .key(format!("shot-{mid}"))
        .role("button")
        .label("Expand screenshot")
        .focusable(true)
        .w_full()
        .radius(10.0)
        .border(1.0, p.hairline)
        .cursor(Cursor::Pointer)
        .press_scale(0.99)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_lightbox(mid, tx))
        .drag_out(move |_: &State| Some(DragItem::new().file(&path)));

    if let Some(img) = s.shots.get(&mid)
        && let Some([w, h]) = img.size()
    {
        let ratio = w.max(1) as f32 / h.max(1) as f32;
        let natural_h = width / ratio;
        if natural_h <= SHOT_MAX_H {
            Some((button.h(natural_h).pixels_fit(img.pixels(), SurfaceFit::Fit), natural_h))
        } else {
            let shot_w = SHOT_MAX_H * ratio;
            Some((
                button
                    .h(SHOT_MAX_H)
                    .fill(p.bg.alpha(0.5))
                    .center()
                    .child(el().w(shot_w).h(SHOT_MAX_H).pixels_fit(img.pixels(), SurfaceFit::Fit)),
                SHOT_MAX_H,
            ))
        }
    } else if slide.is_image()
        && let (Some(w), Some(h)) = (slide.width, slide.height)
        && w > 0
        && h > 0
    {
        let ratio = w as f32 / h as f32;
        let box_h = (width / ratio).min(SHOT_MAX_H);
        Some((button.h(box_h).fill(p.hover).center().child(text("Loading…").font(style::META).color(p.text_3)), box_h))
    } else {
        None
    }
}
