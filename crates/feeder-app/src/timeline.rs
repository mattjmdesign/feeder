use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use feeder_core::{FeedUpdate, ProjectSummary, ago, day_key, day_label};
use rune::{Cursor, El, GrowthSpec, Key, KeyEvent, ScrollPos, ScrollSpec, Transition, Tx, col, el, memo, row, text, text_input};
use rune_kit::{Divider, Kit, MarkdownStyle, Menu, MenuItem, Placement, Spinner, scroll_bar};

use crate::style::{self, Palette};
use crate::watch::TIMELINE_PAGE;
use crate::{CopyTarget, State, agents, chrome, feed, orb};

const COLUMN: f32 = 660.0;
const AVATAR: f32 = 32.0;
const GUTTER: f32 = 12.0;
const PRELOAD: f32 = 1200.0;

pub fn view(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let total: i64 = s.projects.iter().filter(|p| !p.project.archived).map(|p| p.updates).sum();
    let visible = s.filtered_timeline();
    let query = s.feed_search.text.trim();
    let unfiltered = s.branch_filter.is_none() && query.is_empty();
    let more = unfiltered && (visible.len() as i64) < total;

    let filter_bar = filter_row(s, p);
    let waiting = unfiltered.then(|| needs_you_now(s, p)).flatten();
    let live = unfiltered.then(|| working_now(s, p, k)).flatten();
    let watcher = s.watcher.clone();
    let asked = s.timeline_asked.clone();
    let next = s.timeline.len().max(s.timeline_limit) + TIMELINE_PAGE;
    let footer = if !s.loaded {
        el()
    } else if visible.is_empty() {
        let msg =
            if unfiltered { "No updates yet. Updates from active projects will appear here." } else { "No updates match these filters." };
        text(msg).font(style::META).color(p.text_3)
    } else if !query.is_empty() && s.search_snapshot_query == query {
        let n = visible.len();
        text(format!("{n} match{} across full history", if n == 1 { "" } else { "es" })).font(style::META).color(p.text_3)
    } else if more {
        let asked_click = asked.clone();
        chrome::bar_button("timeline-load-more", "Load Earlier Updates", p).border(1.0, p.hairline).on_press(move |s: &mut State| {
            request(&asked_click, next, s.watcher.as_ref());
        })
    } else {
        text("All caught up").font(style::META).color(p.text_3)
    };

    let near_end = move |at: ScrollPos| {
        if more && at.offset[1] + PRELOAD >= at.max[1] {
            request(&asked, next, watcher.as_ref());
        }
    };

    let mut content = col().w_full().max_w(COLUMN).gap(16.0).child(filter_bar);
    if let Some(waiting) = waiting {
        content = content.child(waiting);
    }
    if let Some(live) = live {
        content = content.child(live);
    }
    if !visible.is_empty() {
        content = content.child(posts(s, p, k, &visible, layer));
    }
    content = content.child(el().h(12.0)).child(footer.self_center());

    let viewport = col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(col().w_full().items_center().px(24.0).pt(12.0).pb(72.0).child(content))
        .scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(20.0)
        .on_scroll_offset(near_end);
    let scroller = scroll_bar(k, "timeline-scroll-area", "timeline-scroll", viewport).w_full().grow(1.0).min_h(0.0);

    col().key("timeline").w_full().grow(1.0).min_h(0.0).transition(Transition::none()).child(scroller)
}

struct PostRow {
    id: i64,
    key: String,
    starts_day: bool,
}

fn posts(s: &State, p: &Palette, k: &Kit, visible: &[(&FeedUpdate, &ProjectSummary)], layer: &mut Vec<El>) -> El {
    let mut previous_day = None;
    let rows: Rc<[PostRow]> = visible
        .iter()
        .map(|(update, _)| {
            let day = day_key(update.created_at);
            PostRow { id: update.id, key: format!("t-{}", update.id), starts_day: previous_day.replace(day) != Some(day) }
        })
        .collect();
    let keys = rows.clone();
    let menu = s.post_menu.filter(|id| rows.iter().any(|r| r.id == *id)).map(|id| (id, post_menu(id, p, k, layer)));
    let (p, k) = (*p, k.clone());
    let md = feed::markdown_style(&p, &k);
    col()
        .key("posts")
        .w_full()
        .growth(GrowthSpec::default())
        .virtual_rows(
            rows.len(),
            240.0,
            move |_: &State, i| keys[i].key.clone().into(),
            move |s: &State, i| {
                let PostRow { id, starts_day, .. } = rows[i];
                let (_, source) = s.timeline_source();
                let Some(u) = source.iter().find(|u| u.id == id) else { return el() };
                let Some(summary) = s.summary(u.project_id) else { return el() };
                let divider = if starts_day {
                    feed::day_divider(day_label(u.created_at, s.now), &p, i > 0)
                } else {
                    Divider::horizontal().build(&k).pl(AVATAR + GUTTER)
                };
                let menu_trigger = menu.as_ref().filter(|(menu_id, _)| *menu_id == id).map(|(_, trigger)| trigger.clone());
                col().w_full().child(divider).child(post(s, (&p, &k, &md), u, summary, menu_trigger))
            },
        )
        .on_visible_rows(|s: &mut State, range| {
            s.feed_visible_rows = Some(range);
            crate::media::sync(s);
            false
        })
}

fn filter_row(s: &State, p: &Palette) -> El {
    let search = text_input(&s.feed_search, style::META)
        .key("feed-search")
        .label("Search updates")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(p.text)
        .caret(p.text, p.selection)
        .placeholder("Search updates", p.text_3)
        .help("Search updates (Command-F)")
        .on_edit(|s: &mut State| &mut s.feed_search)
        .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| {
            if matches!(e.key, Key::Escape) && !s.feed_search.text.is_empty() {
                s.feed_search.text.clear();
                return true;
            }
            false
        });

    let mut search_box =
        row().grow(1.0).min_w(140.0).h(28.0).px(10.0).gap(6.0).center_y().radius(8.0).fill(p.card).border(1.0, p.hairline).child(search);
    if !s.feed_search.text.is_empty() {
        search_box = search_box.child(
            row()
                .key("clear-feed-search")
                .role("button")
                .label("Clear search")
                .size(18.0, 18.0)
                .center()
                .radius(5.0)
                .hover(p.hover)
                .cursor(Cursor::Pointer)
                .on_press(|s: &mut State| s.feed_search.text.clear())
                .child(text("×").font(style::META).color(p.text_3)),
        );
    }

    let chip = |key: &str, label: String, active: bool| {
        row()
            .key(key.to_string())
            .role("button")
            .label(label.clone())
            .focusable(true)
            .h(28.0)
            .px(10.0)
            .center_y()
            .radius(8.0)
            .fill(if active { p.hover } else { p.card.alpha(0.0) })
            .border(1.0, if active { p.text_3.alpha(0.45) } else { p.hairline })
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .child(text(label).font(style::META).color(if active { p.text } else { p.text_3 }))
    };

    let mut row_el = row().key("feed-filters").w_full().flex_wrap().gap(6.0).center_y().child(search_box);

    let unseen = s.unseen_count();
    if let Some(branch) = &s.branch_filter {
        row_el = row_el.child(chip("f-branch", format!("Branch: {branch} ×"), true).on_press(|s: &mut State| s.branch_filter = None));
    }
    if unseen > 0 {
        row_el = row_el.child(
            chrome::bar_button("timeline-mark-read", "Mark Read", p).border(1.0, p.hairline).on_press(|s: &mut State| s.mark_all_seen()),
        );
    }
    row_el
}

fn request(asked: &Arc<AtomicUsize>, n: usize, watcher: Option<&crate::watch::Watcher>) {
    if asked.fetch_max(n, Ordering::Relaxed) < n
        && let Some(w) = watcher
    {
        w.timeline(n);
    }
}

fn strip_label(label: &str, p: &Palette) -> El {
    text(label.to_uppercase()).font(style::LABEL).color(p.text_3).pb(2.0)
}

fn needs_you_now(s: &State, p: &Palette) -> Option<El> {
    let waiting: Vec<&ProjectSummary> = s.projects.iter().filter(|p| !p.project.archived && p.needs_you).collect();
    if waiting.is_empty() {
        return None;
    }
    let action = |key: String, label: &str, a11y: String| {
        row()
            .key(key)
            .role("button")
            .label(a11y)
            .focusable(true)
            .h(24.0)
            .px(8.0)
            .center_y()
            .radius(6.0)
            .border(1.0, p.hairline)
            .hover(p.hover)
            .cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(style::META).color(p.text_2))
    };
    let mut rows = col().key("needs-you-now").w_full().gap(6.0).pb(4.0).child(strip_label("Needs you", p));
    for summary in waiting {
        let id = summary.project.id;
        let name = summary.project.name.clone();
        let question = crate::home::needs_you_question(summary).unwrap_or_else(|| "Waiting for your reply".into());
        let mut actions = row().gap(4.0).center_y();
        if let Some(uid) = summary.last_update_id {
            actions = actions
                .child(action(format!("needs-reply-{id}"), "Reply", format!("Reply to {name}")).on_press_tx(
                    move |s: &mut State, tx: &mut Tx| {
                        s.open_project(id);
                        s.pending_scroll = Some(format!("u-{uid}"));
                        s.start_reply(uid, tx);
                    },
                ))
                .child(
                    action(format!("needs-dismiss-{id}"), "Dismiss", format!("Dismiss Needs you for {name}"))
                        .on_press(move |s: &mut State| s.dismiss_attention(uid)),
                );
        }
        actions = actions
            .child(action(format!("needs-open-{id}"), "Open", format!("Open {name}")).on_press(move |s: &mut State| s.open_project(id)));
        rows = rows.child(
            row()
                .key(format!("needs-{id}"))
                .w_full()
                .gap(10.0)
                .center_y()
                .px(12.0)
                .py(8.0)
                .radius(10.0)
                .fill(p.amber.alpha(0.08))
                .border(1.0, p.amber.alpha(0.35))
                .child(el().size(6.0, 6.0).radius(3.0).fill(p.amber))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(2.0)
                        .child(text(name.clone()).font(style::AGENT).color(p.text).truncate())
                        .child(text(question).font(style::META).color(p.text_2).max_lines(2)),
                )
                .child(actions),
        );
    }
    Some(rows)
}

fn working_now(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    let live: Vec<&ProjectSummary> = s.projects.iter().filter(|p| !p.project.archived && p.active_pulse.is_some()).collect();
    if live.is_empty() {
        return None;
    }
    let mut rows = col().key("working-now").w_full().gap(6.0).pb(12.0).child(strip_label("Working now", p));
    for summary in live {
        let Some(pulse) = &summary.active_pulse else { continue };
        let id = summary.project.id;
        rows = rows.child(
            row()
                .key(format!("working-{id}"))
                .w_full()
                .gap(8.0)
                .center_y()
                .px(12.0)
                .py(8.0)
                .radius(10.0)
                .fill(p.card)
                .border(1.0, p.hairline)
                .role("button")
                .label(format!("Open {}", summary.project.name))
                .focusable(true)
                .hover(p.hover)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| s.open_project(id))
                .child(Spinner::new().size(14.0).color(p.live).build(k))
                .child(text(format!("{} · {}", pulse.agent, summary.project.name)).font(style::AGENT).color(p.text).truncate())
                .child(text(pulse.status.clone()).font(style::META).color(p.text_2).truncate().shrink().min_w(0.0)),
        );
    }
    Some(rows)
}

fn post(s: &State, (p, k, md): (&Palette, &Kit, &MarkdownStyle), u: &FeedUpdate, summary: &ProjectSummary, menu_trigger: Option<El>) -> El {
    let id = summary.project.id;
    let uid = u.id;
    let selected = s.timeline_selected == Some(uid);
    let open_project = |key: &str, child: El| {
        row()
            .key(key.to_string())
            .role("button")
            .label(format!("Open {}", summary.project.name))
            .focusable(true)
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.open_project(id))
            .child(child)
    };
    let avatar = open_project("avatar", memo(format!("t-orb-{uid}"), id, || orb::orb(&summary.project.orb, AVATAR)))
        .size(AVATAR, AVATAR)
        .press_scale(0.96);

    let mut header = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(open_project("project", text(summary.project.name.clone()).font(style::NAME).color(p.text).truncate()).shrink())
        .child(agents::post_mark(s, &u.agent, u.via.as_deref(), p, 14.0))
        .child(text(u.agent.clone()).font(style::META).color(p.text_2).truncate().shrink())
        .child(action_dot(p))
        .child(text(ago(u.created_at, s.now)).font(style::META).color(p.text_3));
    header = header.child(rune::spacer()).child(post_actions(s, p, u, menu_trigger));

    let mut words = col().grow(1.0).min_w(0.0).gap(8.0).child(header);
    let mut attention = feed::card_attention_tags(s, p, u);
    attention.extend(feed::flag_pill(s, p, u));
    if !attention.is_empty() {
        let mut tags = row().gap(6.0).center_y();
        for tag in attention {
            tags = tags.child(tag);
        }
        words = words.child(tags);
    }
    words = words.child(if s.editing_update == Some(uid) {
        feed::edit_or_body(s, p, k, md, u, true)
    } else {
        memo(format!("t-body-{uid}"), (p.dark, &u.body), || post_body(p, md, u))
    });
    if let Some(media) = feed::media_view(s, p, u, COLUMN - AVATAR - GUTTER) {
        words = words.child(media);
    }
    if let Some(footer) = post_footer(s, p, u) {
        words = words.child(footer);
    }

    row()
        .w_full()
        .gap(GUTTER)
        .items_start()
        .px(12.0)
        .py(14.0)
        .mx(-12.0)
        .radius(12.0)
        .fill(if selected { p.hover } else { p.hover.alpha(0.0) })
        .hover_group()
        .child(avatar)
        .child(words)
}

fn post_body(p: &Palette, md: &MarkdownStyle, u: &FeedUpdate) -> El {
    let mut body = col().w_full().gap(10.0);
    for (i, section) in crate::prose::sections(&u.body).into_iter().enumerate() {
        let rendered = rune_kit::Markdown::new(section.markdown)
            .selectable(true)
            .on_link_tx(|s: &mut State, url: &str, tx: &mut Tx| {
                if !s.open_feeder_url(url) {
                    let _ = tx.open_url(url);
                }
            })
            .render(md);
        let mut block = col().key(format!("s{i}")).w_full().gap(3.0);
        if let Some(label) = section.label
            && !(i == 0 && label.eq_ignore_ascii_case("done"))
        {
            block = block.child(text(sentence_case(&label)).font(style::META).color(p.text_3));
        }
        body = body.child(block.child(rendered));
    }
    body
}

fn sentence_case(label: &str) -> String {
    let lower = label.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn post_action(p: &Palette, key: String, a11y: &str, content: El) -> El {
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
        .child(content)
}

fn action_dot(p: &Palette) -> El {
    el().size(2.0, 2.0).radius(1.0).fill(p.text_3)
}

fn more_action(uid: i64, p: &Palette) -> El {
    let dots = row().gap(2.0).center_y().children((0..3).map(|_| action_dot(p))).a11y_hidden();
    post_action(p, format!("more-u-{uid}"), "More actions", dots)
        .on_press(move |s: &mut State| s.post_menu = if s.post_menu == Some(uid) { None } else { Some(uid) })
}

fn post_actions(s: &State, p: &Palette, u: &FeedUpdate, menu_trigger: Option<El>) -> El {
    let uid = u.id;
    let body = u.body.clone();
    let copy_label = if s.is_copied(CopyTarget::Update(uid)) { "Copied" } else { "Copy" };
    let copy = post_action(p, format!("copy-u-{uid}"), "Copy update", text(copy_label).font(style::META).color(p.text_3)).on_press_tx(
        move |s: &mut State, tx: &mut Tx| {
            tx.copy(body.clone());
            s.mark_copied(CopyTarget::Update(uid));
        },
    );
    let shown = menu_trigger.is_some();
    let more = menu_trigger.unwrap_or_else(|| more_action(uid, p));
    row().gap(2.0).center_y().group_opacity(if shown { 1.0 } else { 0.0 }, 1.0).child(copy).child(more)
}

fn post_menu(uid: i64, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    Menu::new(format!("post-menu-{uid}"))
        .trigger(more_action(uid, p))
        .open(true)
        .placement(Placement::BottomEnd)
        .min_width(176.0)
        .on_close(|s: &mut State| s.post_menu = None)
        .item(MenuItem::new("Flag for Library").on_select(move |s: &mut State| {
            s.post_menu = None;
            s.flag_update_for_notebook(uid);
        }))
        .item(MenuItem::new("Edit").on_select_tx(move |s: &mut State, tx: &mut Tx| {
            s.post_menu = None;
            s.start_edit_update(uid, tx);
        }))
        .separator()
        .item(MenuItem::new("Delete…").danger(true).on_select(move |s: &mut State| {
            s.post_menu = None;
            s.delete_confirm = Some(uid);
        }))
        .build(k, layer)
}

fn post_footer(s: &State, p: &Palette, u: &FeedUpdate) -> Option<El> {
    let diff = u.diff_stat.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let link = feed::notebook_link(s, p, u.id);
    if diff.is_none() && link.is_none() {
        return None;
    }
    let mut r = row().w_full().gap(8.0).center_y();
    if let Some(diff) = diff {
        r = r.child(text(diff.to_string()).font(style::META).color(p.text_3).truncate().max_w(220.0));
    }
    if let Some(link) = link {
        if diff.is_some() {
            r = r.child(action_dot(p));
        }
        r = r.child(link);
    }
    Some(r)
}
