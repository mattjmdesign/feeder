//! One project's feed: the orb & brief up top, then every update newest first along a
//! timeline (or contact-sheet Gallery), plus inline edit, two-way replies, and video playback.

use feeder_core::{AttentionKind, FeedUpdate, Orb, ProjectSummary, ago, day_key, day_label, parse_attention};
use rune::tok::motion;
use rune::{
    Cursor, DragItem, El, Key, KeyEvent, ScrollSpec, Spring, SurfaceFit, Transition, Tx, auto_grow, col, el, row, text, text_input,
};
use rune_kit::search::Fold;
use rune_kit::{Button, ButtonVariant, Checkbox, Kit, Markdown, MarkdownStyle, scroll_bar};

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
        .filter(|u| {
            query.is_empty()
                || fold.contains(&u.body, query)
                || fold.contains(&u.agent, query)
                || u.branch.as_deref().is_some_and(|b| fold.contains(b, query))
        })
        .collect()
}

pub fn bar_chrome(s: &State, id: i64, p: &Palette) -> (El, Option<El>) {
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
    let project_todos = s.project_todos(id);
    let open_todo_count = if s.todos_for == Some(id) {
        project_todos.iter().filter(|t| !t.done).count()
    } else {
        usize::try_from(summary.open_todos.max(0)).unwrap_or(0)
    };
    let todo_label = if open_todo_count > 0 { format!("To-Do · {open_todo_count}") } else { "To-Do".to_string() };

    // Show in Finder and Pin are on the project's orb menu on Home, so the header doesn't repeat
    // them; Copy Context covers what the old resume prompt did.
    let mut actions = row().gap(4.0).center_y();
    actions = actions.child(
        chrome::bar_button("project-todo-btn", &todo_label, p)
            .label("Open project to-do list")
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_todo_editor(id, tx)),
    );
    if s.feed_for == Some(id) && !s.feed.is_empty() {
        let context_label = if s.is_copied(CopyTarget::ProjectContext) { "Copied" } else { "Copy Context" };
        let payload = handoff_markdown(summary, &s.feed, s.now);
        actions = actions.child(chrome::bar_button("copy-context", context_label, p).on_press_tx(move |s: &mut State, tx: &mut Tx| {
            tx.copy(payload.clone());
            s.mark_copied(CopyTarget::ProjectContext);
        }));
    }
    actions = actions
        .child(
            chrome::bar_button("rename-button", if s.editing.is_some() { "Done" } else { "Rename" }, p)
                .on_press_tx(|s: &mut State, tx: &mut Tx| if s.editing.is_some() { s.commit_rename() } else { s.start_rename(tx) }),
        )
        .child(
            chrome::bar_button("archive-button", if archived { "Unarchive" } else { "Archive" }, p)
                .on_press(move |s: &mut State| s.set_archived(id, !archived)),
        );
    (back, Some(actions))
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
        let has_media = s.feed.iter().any(|u| u.screenshot.is_some());
        let search_active = s.project_search_open || !s.project_search.text.is_empty();
        if has_media || s.branch_filter.is_some() || search_active {
            timeline = timeline.child(project_toolbar(s, p, has_media));
        }

        let visible = visible_feed(s);
        if s.project_gallery {
            timeline = timeline.child(gallery_grid(s, p, &visible));
        } else {
            let n = visible.len();
            let mut prev_day: Option<i64> = None;
            for (i, u) in visible.iter().enumerate() {
                let dk = day_key(u.created_at);
                if prev_day != Some(dk) {
                    timeline = timeline.child(day_divider(day_label(u.created_at, s.now), p, i > 0));
                    prev_day = Some(dk);
                }
                let same_next = visible.get(i + 1).is_some_and(|next| day_key(next.created_at) == dk);
                timeline = timeline.child(entry(s, (p, k, &md), u, &summary.project.orb, i == 0, !same_next));
            }
            if n == 0 {
                let empty_msg = if !s.project_search.text.trim().is_empty() {
                    "No updates match this filter."
                } else if s.branch_filter.is_some() {
                    "No updates on this branch."
                } else {
                    "No updates yet. Ask an agent in this project to post a Feeder update."
                };
                timeline = timeline.child(text(empty_msg).font(style::META).color(p.text_3).self_center().py(16.0));
            }
            if s.branch_filter.is_none()
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
    let scroller = scroll_bar(k, "feed-scroll-area", "feed-scroll", viewport).w_full().grow(1.0).min_h(0.0);

    col().key("feed").w_full().grow(1.0).min_h(0.0).transition(Transition::opacity()).child(scroller)
}

fn intro_extras(s: &State, p: &Palette, k: &Kit, summary: &ProjectSummary, mut intro: El) -> El {
    let id = summary.project.id;
    let has_brief = s.brief_editing == Some(id) || summary.project.brief.as_deref().map(str::trim).is_some_and(|b| !b.is_empty());
    let todos = s.project_todos(id);
    let show_todo = s.todo_open == Some(id) || !todos.is_empty();

    // The header's To-Do button adds to-dos, so this row only offers the brief.
    if !has_brief && !show_todo {
        return intro.child(
            row().key("intro-actions").mt(4.0).center_y().child(
                chrome::bar_button("add-brief", "+ Add Project Brief", p)
                    .border(1.0, p.hairline)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_brief_edit(id, tx)),
            ),
        );
    }

    intro = intro.child(brief_card(s, p, k, summary));
    if show_todo {
        intro = intro.child(todo_card(s, p, k, id, todos));
    }
    intro
}

fn todo_card(s: &State, p: &Palette, k: &Kit, id: i64, todos: &[feeder_core::TodoItem]) -> El {
    let open_count = todos.iter().filter(|t| !t.done).count();
    let done_count = todos.len().saturating_sub(open_count);
    let status_label = if todos.is_empty() {
        None
    } else if open_count == 0 {
        Some("All done".to_string())
    } else {
        Some(format!("{open_count} open"))
    };

    let mut header = row().w_full().center_y().gap(8.0).child(text("TO-DO").font(style::LABEL).color(p.text_3));
    if let Some(status) = status_label {
        header = header.child(text(status).font(style::META).color(p.text_3));
    }
    header = header.child(rune::spacer());

    if s.todo_clear_confirm == Some(id) {
        header = header
            .child(text("Clear list?").font(style::META).color(p.text_2))
            .child(
                chrome::bar_button("todo-clear-cancel", "Cancel", p).h(22.0).px(8.0).on_press(|s: &mut State| s.todo_clear_confirm = None),
            )
            .child(
                chrome::bar_button("todo-clear-confirm", "Clear All", p)
                    .h(22.0)
                    .px(8.0)
                    .border(1.0, p.amber.alpha(0.5))
                    .on_press(move |s: &mut State| s.clear_project_todos(id)),
            );
    } else {
        if done_count > 0 {
            header = header.child(
                chrome::bar_button("todo-clear-done", "Clear Done", p)
                    .h(22.0)
                    .px(8.0)
                    .on_press(move |s: &mut State| s.clear_completed_todos(id)),
            );
        }
        if !todos.is_empty() {
            header = header.child(
                chrome::bar_button("todo-clear-all", "Clear All", p)
                    .h(22.0)
                    .px(8.0)
                    .on_press(move |s: &mut State| s.todo_clear_confirm = Some(id)),
            );
        } else {
            header = header.child(
                chrome::bar_button("todo-close-empty", "×", p)
                    .label("Close empty to-do list")
                    .h(22.0)
                    .px(8.0)
                    .on_press(|s: &mut State| s.close_empty_todo()),
            );
        }
    }

    let mut items_col = col().w_full().gap(4.0);
    for item in todos {
        let tid = item.id;
        let check = Checkbox::new(item.done)
            .hide_label()
            .label(format!("Toggle {}", item.title))
            .on_toggle(move |s: &mut State| s.toggle_todo(tid))
            .build(k);

        let middle = if s.todo_editing == Some(tid) {
            let edit_field = text_input(&s.todo_edit_draft, style::BODY)
                .key(format!("todo-edit-{tid}"))
                .label("Edit to-do item")
                .single_line()
                .grow(1.0)
                .min_w(0.0)
                .color(p.text)
                .caret(p.text, p.selection)
                .on_edit(|s: &mut State| &mut s.todo_edit_draft)
                .on_submit(|s: &mut State, _tx: &mut Tx| s.save_todo_edit())
                .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
                    if matches!(e.key, Key::Escape) {
                        s.todo_editing = None;
                        return true;
                    }
                    false
                });
            row().grow(1.0).min_w(0.0).px(8.0).py(4.0).radius(6.0).fill(p.bg).border(1.0, p.hairline).child(edit_field)
        } else {
            let mut label_el = text(item.title.clone()).font(style::BODY).color(if item.done { p.text_3 } else { p.text }).wrap();
            if item.done {
                label_el = label_el.strike(0, item.title.len(), Some(p.text_3));
            }
            el().grow(1.0)
                .min_w(0.0)
                .py(2.0)
                .cursor(Cursor::Text)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_edit_todo(tid, tx))
                .child(label_el)
        };

        let del = chrome::bar_button(&format!("todo-del-{tid}"), "×", p)
            .label(format!("Delete {}", item.title))
            .h(22.0)
            .px(6.0)
            .on_press(move |s: &mut State| s.delete_todo(tid));

        items_col =
            items_col.child(row().key(format!("todo-item-{tid}")).w_full().gap(8.0).center_y().child(check).child(middle).child(del));
    }

    let add_field = text_input(&s.todo_draft, style::BODY)
        .key("todo-add-input")
        .label("Add a to-do item")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(p.text)
        .caret(p.text, p.selection)
        .placeholder("Add a to-do…", p.text_3)
        .on_edit(|s: &mut State| &mut s.todo_draft)
        .on_submit(move |s: &mut State, tx: &mut Tx| s.submit_todo(id, tx))
        .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
            if matches!(e.key, Key::Escape) {
                s.close_empty_todo();
                return true;
            }
            false
        });

    let mut add_row = row().w_full().gap(8.0).px(10.0).py(6.0).center_y().radius(8.0).fill(p.bg).border(1.0, p.hairline).child(add_field);
    if !s.todo_draft.text.trim().is_empty() {
        add_row = add_row.child(
            chrome::bar_button("todo-add-submit", "Add", p)
                .h(22.0)
                .px(8.0)
                .border(1.0, p.hairline)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.submit_todo(id, tx)),
        );
    }

    let mut card = col()
        .key("todo-card")
        .w_full()
        .max_w(COLUMN)
        .mt(8.0)
        .padding(14.0)
        .gap(10.0)
        .radius(12.0)
        .fill(p.card)
        .border(1.0, p.hairline)
        .child(header);
    if !todos.is_empty() {
        card = card.child(items_col);
    }
    card.child(add_row)
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
            .child(chip("pv-all", "Updates", !s.project_gallery).on_press(|s: &mut State| s.project_gallery = false))
            .child(chip("pv-gallery", "Gallery", s.project_gallery).on_press(|s: &mut State| s.project_gallery = true));
    }
    if let Some(branch) = &s.branch_filter {
        bar = bar.child(chip("pv-branch-clear", &format!("Branch: {branch} ×"), true).on_press(|s: &mut State| s.branch_filter = None));
    }
    bar
}

fn gallery_grid(s: &State, p: &Palette, visible: &[&FeedUpdate]) -> El {
    let items: Vec<&FeedUpdate> = visible.iter().copied().filter(|u| u.screenshot.is_some()).collect();
    if items.is_empty() {
        return text("No media attached yet.").font(style::META).color(p.text_3).self_center().py(24.0);
    }
    let tile_w = (COLUMN - 14.0) / 2.0;
    let mut grid = row().key("gallery-grid").w_full().flex_wrap().gap(14.0);
    for u in items {
        let uid = u.id;
        let thumb_h = 196.0;
        let media = if let Some(img) = s.shots.get(&uid) {
            el().w_full().h(thumb_h).pixels_fit(img.pixels(), SurfaceFit::Fit)
        } else if let Some(player) = s.videos.get(&uid) {
            el().w_full().h(thumb_h).video(player, SurfaceFit::Fit).child(
                el().pin(None, Some(8.0), Some(8.0), None)
                    .px(7.0)
                    .py(2.0)
                    .radius(5.0)
                    .fill(p.bg.alpha(0.8))
                    .child(text("Video").font(style::META).color(p.text)),
            )
        } else {
            el().w_full().h(thumb_h).fill(p.hover).center().child(text("Loading…").font(style::META).color(p.text_3))
        };

        let mut footer = row()
            .w_full()
            .px(10.0)
            .py(8.0)
            .gap(6.0)
            .center_y()
            .child(agents::mark(&u.agent, p, 12.0, s.agent_icon(&u.agent)))
            .child(text(u.agent.clone()).font(style::META).color(p.text_2).truncate().shrink());
        if let Some(b) = &u.branch {
            footer = footer.child(text(b.clone()).font(style::PATH).color(p.text_3).truncate().max_w(110.0));
        }
        footer = footer.child(rune::spacer()).child(text(ago(u.created_at, s.now)).font(style::META).color(p.text_3));

        grid = grid.child(
            col()
                .key(format!("gal-{uid}"))
                .role("button")
                .label("Open media")
                .focusable(true)
                .w(tile_w)
                .radius(12.0)
                .fill(p.card)
                .border(1.0, p.hairline)
                .cursor(Cursor::Pointer)
                .press_scale(0.985)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_lightbox(uid, tx))
                .child(media)
                .child(rune::rule(1.0, p.hairline))
                .child(footer),
        );
    }
    grid
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
        .child(text(label).font(style::META).color(p.amber))
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
    let delete_btn =
        small_btn(format!("del-u-{uid}"), "Delete", "Delete update").on_press(move |s: &mut State| s.delete_confirm = Some(uid));

    row().gap(2.0).center_y().child(copy_btn).child(edit_btn).child(reply_btn).child(delete_btn)
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
    if let Some(replies) = replies_section(s, p, k, u) {
        card = card.child(replies);
    }

    row()
        .key(format!("u-{}", u.id))
        .w_full()
        .gap(12.0)
        .pb(20.0)
        .items_stretch()
        .transition(Transition::opacity())
        .motion(motion::DEFAULT)
        .child(rail)
        .child(card)
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
                    .child(Button::new("Save").variant(ButtonVariant::Submit).on_press(|s: &mut State| s.save_update_edit()).build(k)),
            );
    }
    body(p, md, u, strong)
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
                .child(Button::new("Send").variant(ButtonVariant::Submit).on_press(move |s: &mut State| s.submit_reply(uid)).build(k))
                .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.replying_to = None).build(k)),
        );
    }

    Some(box_el)
}

/// An update's attached screenshot or video clip at `width`.
pub fn media_view(s: &State, p: &Palette, u: &FeedUpdate, width: f32) -> Option<El> {
    let uid = u.id;
    if let Some(player) = s.videos.get(&uid) {
        let (vw, vh) = player.natural_size().unwrap_or((16, 9));
        let ratio = vw.max(1) as f32 / vh.max(1) as f32;
        let natural_h = (width / ratio).clamp(180.0, SHOT_MAX_H);
        let playing = player.is_playing();
        return Some(
            el().key(format!("vid-{uid}"))
                .w_full()
                .h(natural_h)
                .radius(10.0)
                .border(1.0, p.hairline)
                .fill(p.bg.alpha(0.5))
                .video(player, SurfaceFit::Fit)
                .child(
                    row()
                        .pin(None, Some(10.0), Some(10.0), None)
                        .gap(6.0)
                        .child(
                            chrome::bar_button(&format!("vid-toggle-{uid}"), if playing { "Pause" } else { "Play" }, p)
                                .fill(p.card.alpha(0.92))
                                .border(1.0, p.hairline)
                                .on_press(move |s: &mut State| s.toggle_video(uid)),
                        )
                        .child(
                            chrome::bar_button(&format!("vid-expand-{uid}"), "Expand", p)
                                .fill(p.card.alpha(0.92))
                                .border(1.0, p.hairline)
                                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_lightbox(uid, tx)),
                        ),
                ),
        );
    }
    screenshot(s, p, u, width)
}

/// An update's screenshot at `width`: fits the card's full width while keeping aspect ratio,
/// capping unusually tall portrait images inside a centered full-width frame. While decoding
/// is in flight, reserves the exact final height using persisted `(media_w, media_h)`.
pub fn screenshot(s: &State, p: &Palette, u: &FeedUpdate, width: f32) -> Option<El> {
    let uid = u.id;
    let mut button = el()
        .key(format!("shot-{uid}"))
        .role("button")
        .label("Expand screenshot")
        .focusable(true)
        .w_full()
        .radius(10.0)
        .border(1.0, p.hairline)
        .cursor(Cursor::Pointer)
        .press_scale(0.99)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_lightbox(uid, tx));
    if let Some(path) = u.screenshot.clone() {
        button = button.drag_out(move |_: &State| Some(DragItem::new().file(&path)));
    }

    if let Some(img) = s.shots.get(&uid)
        && let Some([w, h]) = img.size()
    {
        let ratio = w.max(1) as f32 / h.max(1) as f32;
        let natural_h = width / ratio;
        if natural_h <= SHOT_MAX_H {
            Some(button.h(natural_h).pixels_fit(img.pixels(), SurfaceFit::Fit))
        } else {
            let shot_w = SHOT_MAX_H * ratio;
            Some(
                button
                    .h(SHOT_MAX_H)
                    .fill(p.bg.alpha(0.5))
                    .center()
                    .child(el().w(shot_w).h(SHOT_MAX_H).pixels_fit(img.pixels(), SurfaceFit::Fit)),
            )
        }
    } else if u.has_image()
        && let (Some(w), Some(h)) = (u.media_w, u.media_h)
        && w > 0
        && h > 0
    {
        let ratio = w as f32 / h as f32;
        let box_h = (width / ratio).min(SHOT_MAX_H);
        Some(button.h(box_h).fill(p.hover).center().child(text("Loading…").font(style::META).color(p.text_3)))
    } else {
        None
    }
}
