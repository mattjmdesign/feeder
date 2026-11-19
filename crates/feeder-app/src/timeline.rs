//! The Feed tab: every project's updates in one stream, newest first, loading further back as the
//! end of the list comes near. Each post wears its project's orb; the orb or the project's name
//! opens that project's full history.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use feeder_core::{FeedUpdate, ProjectSummary, ago, day_key, day_label};
use rune::tok::motion;
use rune::{Cursor, El, Key, KeyEvent, ScrollPos, ScrollSpec, Transition, Tx, col, el, row, text, text_input};
use rune_kit::{Kit, MarkdownStyle, scroll_bar};

use crate::style::{self, Palette};
use crate::watch::TIMELINE_PAGE;
use crate::{FeedFilter, State, agents, chrome, feed, orb};

const COLUMN: f32 = 660.0;
const AVATAR: f32 = 26.0;
/// Start loading the next page this far (in points) before the end.
const PRELOAD: f32 = 1200.0;

pub fn view(s: &State, p: &Palette, k: &Kit) -> El {
    let md = feed::markdown_style(p, k);
    let total: i64 = s.projects.iter().filter(|p| !p.project.archived).map(|p| p.updates).sum();
    let visible = s.filtered_timeline();
    let query = s.feed_search.text.trim();
    let unfiltered = s.feed_filter == FeedFilter::All && s.feed_agent.is_none() && s.branch_filter.is_none() && query.is_empty();
    let more = unfiltered && (visible.len() as i64) < total;

    let filter_bar = filter_row(s, p);
    let mut list = col().key("posts").w_full().gap(16.0);
    let mut prev_day: Option<i64> = None;
    for (i, (u, summary)) in visible.iter().enumerate() {
        let dk = day_key(u.created_at);
        if prev_day != Some(dk) {
            list = list.child(feed::day_divider(day_label(u.created_at, s.now), p, i > 0));
            prev_day = Some(dk);
        }
        list = list.child(post(s, p, k, &md, u, summary));
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

fn post(s: &State, p: &Palette, k: &Kit, md: &MarkdownStyle, u: &FeedUpdate, summary: &ProjectSummary) -> El {
    let id = summary.project.id;
    let selected = s.timeline_selected == Some(u.id);
    let accent = orb::accent(&summary.project.orb);
    let avatar = el().key("avatar").size(AVATAR, AVATAR).child(orb::orb(&summary.project.orb, AVATAR));
    let name = text(summary.project.name.clone()).key("project").font(style::NAME).color(p.text).truncate();
    let project_link = row()
        .role("button")
        .label(format!("Open {}", summary.project.name))
        .focusable(true)
        .gap(8.0)
        .px(6.0)
        .py(3.0)
        .ml(-6.0)
        .radius(8.0)
        .center_y()
        .hover(p.hover)
        .cursor(Cursor::Pointer)
        .press_scale(0.98)
        .on_press(move |s: &mut State| s.open_project(id))
        .child(avatar)
        .child(name.shrink());
    let header = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(project_link.shrink())
        .child(text("·").font(style::META).color(p.text_3))
        .child(agents::mark(&u.agent, p, 14.0, s.agent_icon(&u.agent)))
        .child(text(u.agent.clone()).font(style::META).color(p.text_2).truncate().shrink())
        .child(rune::spacer())
        .child(feed::card_actions(s, p, u))
        .child(text("·").font(style::META).color(p.text_3))
        .child(text(ago(u.created_at, s.now)).font(style::META).color(p.text_3));

    let mut tags = row().w_full().flex_wrap().gap(6.0).center_y();
    let mut has_tags = false;
    if let Some(pill) = feed::branch_pill_clickable(s, p, u.branch.as_deref()) {
        tags = tags.child(pill);
        has_tags = true;
    }
    if let Some(diff) = feed::diff_badge(p, u.diff_stat.as_deref()) {
        tags = tags.child(diff);
        has_tags = true;
    }
    for badge in feed::card_attention_tags(s, p, u) {
        tags = tags.child(badge);
        has_tags = true;
    }

    let mut card = col()
        .key(format!("t-{}", u.id))
        .w_full()
        .gap(12.0)
        .padding(20.0)
        .radius(14.0)
        .fill(p.card)
        .border(if selected { 1.5 } else { 1.0 }, if selected { accent.alpha(0.75) } else { p.hairline })
        .transition(Transition::opacity())
        .motion(motion::DEFAULT)
        .child(header);
    if has_tags {
        card = card.child(tags);
    }
    card = card.child(rune::rule(1.0, p.hairline)).child(feed::edit_or_body(s, p, k, md, u, true));
    if let Some(media) = feed::media_view(s, p, u, COLUMN - 40.0) {
        card = card.child(media);
    }
    if let Some(replies) = feed::replies_section(s, p, k, u) {
        card = card.child(replies);
    }
    card
}
