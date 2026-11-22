//! The Feed tab: every project's updates in one stream, newest first, loading further back as the
//! end of the list comes near. Each post wears its project's orb; the orb or the project's name
//! opens that project's full history.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use feeder_core::{FeedUpdate, ProjectSummary, ago, day_key, day_label};
use rune::tok::motion;
use rune::{Cursor, El, Key, KeyEvent, ScrollPos, ScrollSpec, Transition, Tx, col, el, row, text, text_input};
use rune_kit::{Divider, Kit, MarkdownStyle, Menu, MenuItem, Placement, Spinner, scroll_bar};

use crate::style::{self, Palette};
use crate::watch::TIMELINE_PAGE;
use crate::{CopyTarget, FeedFilter, State, agents, chrome, feed, orb};

const COLUMN: f32 = 660.0;
/// The project orb beside each post: the post's face, like an avatar in a timeline.
const AVATAR: f32 = 32.0;
/// Space between the avatar and the post's text.
const GUTTER: f32 = 12.0;
/// Start loading the next page this far (in points) before the end.
const PRELOAD: f32 = 1200.0;

pub fn view(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let md = feed::markdown_style(p, k);
    let total: i64 = s.projects.iter().filter(|p| !p.project.archived).map(|p| p.updates).sum();
    let visible = s.filtered_timeline();
    let query = s.feed_search.text.trim();
    let unfiltered = s.feed_filter == FeedFilter::All && s.feed_agent.is_none() && s.branch_filter.is_none() && query.is_empty();
    let more = unfiltered && (visible.len() as i64) < total;

    let filter_bar = filter_row(s, p);
    let mut list = col().key("posts").w_full();
    if unfiltered && let Some(live) = working_now(s, p, k) {
        list = list.child(live);
    }
    let mut prev_day: Option<i64> = None;
    for (i, (u, summary)) in visible.iter().enumerate() {
        let dk = day_key(u.created_at);
        if prev_day != Some(dk) {
            list = list.child(feed::day_divider(day_label(u.created_at, s.now), p, i > 0));
            prev_day = Some(dk);
        } else {
            list = list.child(Divider::horizontal().build(k).pl(AVATAR + GUTTER));
        }
        list = list.child(post(s, p, k, &md, u, summary, layer));
    }
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
        text("Loading earlier updates…").font(style::META).color(p.text_3)
    } else {
        text("All caught up").font(style::META).color(p.text_3)
    };

    let watcher = s.watcher.clone();
    let asked = s.timeline_asked.clone();
    let next = s.timeline.len().max(s.timeline_limit) + TIMELINE_PAGE;
    let near_end = move |at: ScrollPos| {
        if more && at.offset[1] + PRELOAD >= at.max[1] {
            request(&asked, next, watcher.as_ref());
        }
    };

    let viewport =
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(col().w_full().items_center().px(24.0).pt(12.0).pb(72.0).child(
                col().w_full().max_w(COLUMN).gap(16.0).child(filter_bar).child(list).child(el().h(12.0)).child(footer.self_center()),
            ))
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(20.0)
            .on_scroll_offset(near_end);
    let scroller = scroll_bar(k, "timeline-scroll-area", "timeline-scroll", viewport).w_full().grow(1.0).min_h(0.0);

    col().key("timeline").w_full().grow(1.0).min_h(0.0).transition(Transition::opacity()).child(scroller)
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
        .placeholder("Search updates…  ⌘F", p.text_3)
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

    let mut row_el = row()
        .key("feed-filters")
        .w_full()
        .flex_wrap()
        .gap(6.0)
        .center_y()
        .child(search_box)
        .child(chip("f-all", "All".into(), s.feed_filter == FeedFilter::All).on_press(|s: &mut State| s.feed_filter = FeedFilter::All));

    let needs_count = s.projects.iter().filter(|p| !p.project.archived && p.needs_you).count();
    if needs_count > 0 || s.feed_filter == FeedFilter::NeedsYou {
        row_el = row_el.child(
            chip("f-needs", format!("Needs you ({needs_count})"), s.feed_filter == FeedFilter::NeedsYou)
                .on_press(|s: &mut State| s.feed_filter = FeedFilter::NeedsYou),
        );
    }

    let unseen = s.unseen_count();
    if unseen > 0 || s.feed_filter == FeedFilter::Unread {
        row_el = row_el.child(
            chip("f-unread", format!("Unread ({unseen})"), s.feed_filter == FeedFilter::Unread)
                .on_press(|s: &mut State| s.feed_filter = FeedFilter::Unread),
        );
    }
    row_el = row_el.child(
        chip("f-shots", "Media".into(), s.feed_filter == FeedFilter::Shots).on_press(|s: &mut State| s.feed_filter = FeedFilter::Shots),
    );

    let agents: Vec<String> = s.timeline.iter().map(|u| u.agent.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    if agents.len() > 1 || s.feed_agent.is_some() {
        let label = match &s.feed_agent {
            Some(a) => a.clone(),
            None => "All agents".into(),
        };
        let active = s.feed_agent.is_some();
        row_el = row_el.child(chip("f-agent", label, active).on_press(move |s: &mut State| {
            let next = match &s.feed_agent {
                None => agents.first().cloned(),
                Some(cur) => agents.iter().position(|a| a == cur).and_then(|i| agents.get(i + 1).cloned()),
            };
            s.feed_agent = next;
        }));
    }
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

/// Agents at work right now, pinned above the posts: native Spinner, who, where, and what.
fn working_now(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    let live: Vec<&ProjectSummary> = s.projects.iter().filter(|p| !p.project.archived && p.active_pulse.is_some()).collect();
    if live.is_empty() {
        return None;
    }
    let mut rows = col().key("working-now").w_full().gap(6.0).pb(12.0);
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

/// One post, laid out like a timeline: the project's orb on the left; one header line of
/// project, agent, branch and time; the update itself; then quiet actions that appear on hover
/// or keyboard focus.
fn post(s: &State, p: &Palette, k: &Kit, md: &MarkdownStyle, u: &FeedUpdate, summary: &ProjectSummary, layer: &mut Vec<El>) -> El {
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
    let avatar = open_project("avatar", orb::orb(&summary.project.orb, AVATAR)).size(AVATAR, AVATAR).press_scale(0.96);

    let mut header = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(open_project("project", text(summary.project.name.clone()).font(style::NAME).color(p.text).truncate()).shrink())
        .child(agents::mark(&u.agent, p, 14.0, s.agent_icon(&u.agent)))
        .child(text(u.agent.clone()).font(style::META).color(p.text_2).truncate().shrink());
    if let Some(branch) = feed::branch_pill_clickable(s, p, u.branch.as_deref()) {
        header = header.child(text("·").font(style::META).color(p.text_3)).child(branch);
    }
    header = header
        .child(rune::spacer())
        .child(post_actions(s, p, k, u, layer))
        .child(text(ago(u.created_at, s.now)).font(style::META).color(p.text_3));

    let mut words = col().grow(1.0).min_w(0.0).gap(8.0).child(header);
    let attention = feed::card_attention_tags(s, p, u);
    if !attention.is_empty() {
        let mut tags = row().gap(6.0).center_y();
        for tag in attention {
            tags = tags.child(tag);
        }
        words = words.child(tags);
    }
    words = words.child(if s.editing_update == Some(uid) { feed::edit_or_body(s, p, k, md, u, true) } else { post_body(p, md, u) });
    if let Some(media) = feed::media_view(s, p, u, COLUMN - AVATAR - GUTTER) {
        words = words.child(media);
    }
    if let Some(footer) = post_footer(s, p, u) {
        words = words.child(footer);
    }
    if let Some(replies) = feed::replies_section(s, p, k, u) {
        words = words.child(replies);
    }

    row()
        .key(format!("t-{uid}"))
        .w_full()
        .gap(GUTTER)
        .items_start()
        .px(12.0)
        .py(14.0)
        .mx(-12.0)
        .radius(12.0)
        .fill(if selected { p.hover } else { p.hover.alpha(0.0) })
        .hover_group()
        .transition(Transition::opacity())
        .motion(motion::DEFAULT)
        .child(avatar)
        .child(words)
}

/// The update as a post: keeps full rich-text Markdown contrast for code blocks, lists, links,
/// and bold. Only an opening "Done" label is omitted so the post reads naturally; Why, Next, and
/// any other section labels stay visible.
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

/// Reply, Copy and the ⋯ menu (Flag for Notebook, Edit, Delete) on the header line, shown on
/// hover or keyboard focus so a stream of posts stays quiet.
fn post_actions(s: &State, p: &Palette, k: &Kit, u: &FeedUpdate, layer: &mut Vec<El>) -> El {
    let uid = u.id;
    let action = |key: String, label: String, a11y: &str| {
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
            .child(text(label).font(style::META).color(p.text_3))
    };
    let reply_count = s.replies.get(&uid).map_or(0, Vec::len);
    let reply_label = if reply_count > 0 { format!("Reply · {reply_count}") } else { "Reply".to_string() };
    let reply = action(format!("reply-u-{uid}"), reply_label, "Reply to update")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_reply(uid, tx));
    let body = u.body.clone();
    let copy_label = if s.is_copied(CopyTarget::Update(uid)) { "Copied" } else { "Copy" }.to_string();
    let copy = action(format!("copy-u-{uid}"), copy_label, "Copy update").on_press_tx(move |s: &mut State, tx: &mut Tx| {
        tx.copy(body.clone());
        s.mark_copied(CopyTarget::Update(uid));
    });
    let menu_open = s.post_menu == Some(uid);
    let trigger = action(format!("more-u-{uid}"), "⋯".to_string(), "More actions")
        .on_press(move |s: &mut State| s.post_menu = if s.post_menu == Some(uid) { None } else { Some(uid) });
    let more = Menu::new(format!("post-menu-{uid}"))
        .trigger(trigger)
        .open(menu_open)
        .placement(Placement::BottomEnd)
        .min_width(176.0)
        .on_close(|s: &mut State| s.post_menu = None)
        .item(MenuItem::new("Flag for Notebook").on_select(move |s: &mut State| {
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
        .build(k, layer);
    let shown = menu_open || s.replying_to == Some(uid);
    row().gap(2.0).center_y().group_opacity(if shown { 1.0 } else { 0.0 }, 1.0).child(reply).child(copy).child(more)
}

/// The post's footer: diff stat and/or a quiet link to the Notebook note sourced from this update.
fn post_footer(s: &State, p: &Palette, u: &FeedUpdate) -> Option<El> {
    let diff = u.diff_stat.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let link = feed::notebook_link(s, p, u.id);
    if diff.is_none() && link.is_none() {
        return None;
    }
    let mut r = row().w_full().gap(8.0).center_y();
    if let Some(diff) = diff {
        r = r.child(text(diff.to_string()).font(style::PATH).color(p.text_3).truncate().max_w(220.0));
    }
    if let Some(link) = link {
        r = r.child(link);
    }
    Some(r)
}
