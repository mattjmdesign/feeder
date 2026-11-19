//! The shelf: every project as an orb, nothing else in the way.

use feeder_core::{Order, ProjectSummary, ago, extract_block};
use rune::tok::motion;
use rune::{Cursor, El, ScrollSpec, Spring, Transition, Tx, col, el, row, text};
use rune_kit::{Kit, MenuItem, context_menu};

use crate::style::{self, Palette};
use crate::{HomeFilter, State, agents, chrome, feed, orb};

pub fn view(s: &State, p: &Palette, k: &Kit, reduced_motion: bool, layer: &mut Vec<El>) -> El {
    let body = if let Some(err) = &s.error {
        message(p, "Can't open the store", err)
    } else if !s.loaded {
        el().key("loading").grow(1.0)
    } else if s.projects.is_empty() {
        empty(p, s.settings.orb_size.points())
    } else {
        shelf(s, p, k, reduced_motion, layer)
    };

    col().key("home").w_full().grow(1.0).min_h(0.0).transition(Transition::opacity()).child(body)
}

pub fn ordered_active(s: &State) -> Vec<&ProjectSummary> {
    let mut active: Vec<&ProjectSummary> = s
        .projects
        .iter()
        .filter(|p| !p.project.archived)
        .filter(|p| match s.home_filter {
            HomeFilter::All => true,
            HomeFilter::NeedsYou => p.needs_you,
            HomeFilter::Working => p.active_pulse.is_some(),
        })
        .collect();
    if s.settings.order == Order::Name {
        active.sort_by_key(|p| (!p.project.pinned, p.project.name.to_lowercase()));
    }
    active
}

fn shelf(s: &State, p: &Palette, k: &Kit, reduced_motion: bool, layer: &mut Vec<El>) -> El {
    let size = s.settings.orb_size.points();
    let (_, gap) = s.settings.orb_size.column();
    let row_w = s.settings.orb_size.row_width();

    let active_all_count = s.projects.iter().filter(|p| !p.project.archived).count();
    let needs_count = s.projects.iter().filter(|p| !p.project.archived && p.needs_you).count();
    let working_count = s.projects.iter().filter(|p| !p.project.archived && p.active_pulse.is_some()).count();

    let active = ordered_active(s);
    let mut archived: Vec<&ProjectSummary> = s.projects.iter().filter(|p| p.project.archived).collect();
    if s.settings.order == Order::Name {
        archived.sort_by_key(|p| (!p.project.pinned, p.project.name.to_lowercase()));
    }

    let mut content = col().w_full().items_center().gap(24.0);

    if active_all_count == 0 && !archived.is_empty() && !s.show_archived {
        content = content.child(all_archived(p, size, archived.len()));
    } else {
        let mut filter_slot = row().key("home-filters-slot").h(28.0).center();
        if needs_count > 0 || working_count > 0 || s.home_filter != HomeFilter::All {
            let chip = |key: &str, label: String, active: bool, dot: Option<rune::Color>| {
                let mut r = row()
                    .key(key.to_string())
                    .role("button")
                    .label(label.clone())
                    .focusable(true)
                    .h(26.0)
                    .px(10.0)
                    .gap(6.0)
                    .center_y()
                    .radius(8.0)
                    .fill(if active { p.hover } else { p.card.alpha(0.0) })
                    .border(1.0, if active { p.text_3.alpha(0.45) } else { p.hairline })
                    .hover(p.hover)
                    .cursor(Cursor::Pointer);
                if let Some(c) = dot {
                    r = r.child(el().size(6.0, 6.0).radius(3.0).fill(c));
                }
                r.child(text(label).font(style::META).color(if active { p.text } else { p.text_3 }))
            };
            let mut pills = row().key("home-filters").gap(6.0).center_y();
            pills = pills.child(
                chip("hf-all", "All".into(), s.home_filter == HomeFilter::All, None)
                    .on_press(|s: &mut State| s.home_filter = HomeFilter::All),
            );
            if needs_count > 0 || s.home_filter == HomeFilter::NeedsYou {
                pills = pills.child(
                    chip("hf-needs", format!("Needs you ({needs_count})"), s.home_filter == HomeFilter::NeedsYou, Some(p.amber))
                        .on_press(|s: &mut State| s.home_filter = HomeFilter::NeedsYou),
                );
            }
            if working_count > 0 || s.home_filter == HomeFilter::Working {
                pills = pills.child(
                    chip("hf-working", format!("Working ({working_count})"), s.home_filter == HomeFilter::Working, Some(p.live))
                        .on_press(|s: &mut State| s.home_filter = HomeFilter::Working),
                );
            }
            filter_slot = filter_slot.child(pills);
        }
        content = content.child(filter_slot);

        if active.is_empty() {
            content = content.child(text("No projects match this filter").font(style::META).color(p.text_3).py(32.0));
        } else {
            let mut grid = row().key("shelf").w_full().max_w(row_w).flex_wrap().justify_center().gap(gap);
            for summary in active {
                grid = grid.child(tile(s, summary, p, k, reduced_motion, layer, size));
            }
            content = content.child(grid);
        }

        let focus_id = s.hovered.or(s.home_selected);
        let preview = match focus_id.and_then(|id| s.summary(id)) {
            Some(sum) => {
                let line = if sum.needs_you {
                    sum.last_body
                        .as_deref()
                        .and_then(|b| extract_block(b, &["decision needed", "decision", "blocked"]))
                        .map(|t| format!("Needs you: {}", feed::first_line(&t, 76)))
                } else {
                    None
                }
                .or_else(|| sum.active_pulse.as_ref().map(|pu| format!("{}: {}", pu.agent, pu.status)))
                .unwrap_or_else(|| {
                    sum.last_body
                        .as_deref()
                        .map(|b| feed::first_line(b, 84))
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| "No updates yet".into())
                });
                text(format!("{} · {line}", sum.project.name)).font(style::META).color(p.text_2).truncate()
            }
            None => text("⌘K to jump · j/k to select · Enter to open").font(style::META).color(p.text_3.alpha(0.7)),
        };
        content = content.child(row().key("hover-preview").h(20.0).max_w(640.0).center().child(preview));

        let unseen = s.unseen_count();
        let mut footer_pills = row().gap(8.0).center_y();
        let mut has_footer_pills = false;
        if unseen > 0 {
            has_footer_pills = true;
            footer_pills = footer_pills.child(
                chrome::bar_button("mark-all-read", &format!("Mark All Read ({unseen})"), p)
                    .border(1.0, p.hairline)
                    .on_press(|s: &mut State| s.mark_all_seen()),
            );
        }

        if !archived.is_empty() && s.show_archived {
            if has_footer_pills {
                content = content.child(row().key("home-footer-slot").h(28.0).center().child(footer_pills));
            }
            let mut archived_grid = row().key("archived-shelf").w_full().max_w(row_w).flex_wrap().justify_center().gap(gap);
            for summary in &archived {
                archived_grid = archived_grid.child(tile(s, summary, p, k, reduced_motion, layer, size));
            }
            let heading = row()
                .w_full()
                .max_w(row_w)
                .center_y()
                .gap(12.0)
                .child(rune::rule(1.0, p.hairline).grow(1.0))
                .child(text(format!("ARCHIVED · {}", archived.len())).font(style::LABEL).color(p.text_3))
                .child(
                    chrome::bar_button("hide-archived", "Hide", p)
                        .border(1.0, p.hairline)
                        .on_press(|s: &mut State| s.show_archived = false),
                )
                .child(rune::rule(1.0, p.hairline).grow(1.0));
            content = content.child(heading).child(archived_grid);
        } else {
            if !archived.is_empty() {
                has_footer_pills = true;
                footer_pills = footer_pills.child(
                    chrome::bar_button("show-archived", &format!("Archived ({})", archived.len()), p)
                        .border(1.0, p.hairline)
                        .on_press(|s: &mut State| s.show_archived = true),
                );
            }
            let mut footer_slot = row().key("home-footer-slot").h(28.0).center();
            if has_footer_pills {
                footer_slot = footer_slot.child(footer_pills);
            }
            content = content.child(footer_slot);
        }
    }

    col().key("shelf-scroll").w_full().grow(1.0).min_h(0.0).child(
        col()
            .w_full()
            .min_h(0.0)
            .grow(1.0)
            .items_center()
            .justify_center()
            .px(32.0)
            .pt(12.0)
            .pb(64.0)
            .child(content)
            .scroll(ScrollSpec { stick_bottom: false }),
    )
}

fn tile(s: &State, summary: &ProjectSummary, p: &Palette, k: &Kit, reduced_motion: bool, layer: &mut Vec<El>, size: f32) -> El {
    let id = summary.project.id;
    let archived = summary.project.archived;
    let pinned = summary.project.pinned;
    let unseen = summary.unseen();
    let hovered = s.hovered == Some(id);
    let selected = s.home_selected == Some(id);
    let width = s.settings.orb_size.column().0;
    let label = match (summary.last_at, &summary.last_agent) {
        (Some(at), Some(agent)) => format!("{}, {} by {agent}", summary.project.name, ago(at, s.now)),
        _ => format!("{}, no updates yet", summary.project.name),
    };
    let meta = match (summary.last_at, &summary.last_agent) {
        (Some(at), Some(agent)) => row()
            .gap(6.0)
            .center_y()
            .child(agents::mark(agent, p, 13.0, s.agent_icon(agent)))
            .child(text(ago(at, s.now)).font(style::META).color(if hovered || selected { p.text_2 } else { p.text_3 })),
        _ => row().child(text("No updates yet").font(style::META).color(p.text_3)),
    };
    let orb_el = orb::orb(&summary.project.orb, size);
    let mut sphere = el()
        .key(format!("orb-{id}"))
        .matched(format!("orb-{id}"))
        .size(size, size)
        .scale(if hovered || selected { 1.06 } else { 1.0 })
        .motion(Spring::new(0.42, 0.28));
    if summary.active_pulse.is_some() && !archived {
        let phase = if reduced_motion { 0.65 } else { ((s.clock * 1.4).sin() as f32) * 0.5 + 0.5 };
        sphere = sphere.child(orb::pulse_halo(&summary.project.orb, size, phase));
    }
    if selected {
        sphere = sphere.child(
            el().size(size + 8.0, size + 8.0)
                .absolute(-4.0, -4.0)
                .radius((size + 8.0) / 2.0)
                .border(1.5, orb::accent(&summary.project.orb).alpha(0.65)),
        );
    }
    sphere = sphere.child(if archived { orb_el.opacity(0.68) } else { orb_el });

    let mut name_row = row().center_y().gap(6.0).max_w(width - 4.0);
    if summary.needs_you && !archived {
        name_row = name_row.child(el().size(6.0, 6.0).radius(3.0).fill(p.amber));
    } else if unseen {
        name_row = name_row.child(el().size(6.0, 6.0).radius(3.0).fill(orb::accent(&summary.project.orb)));
    }
    name_row = name_row
        .child(text(summary.project.name.clone()).font(style::NAME).color(if archived { p.text_2 } else { p.text }).truncate().shrink());
    if pinned && !archived {
        name_row = name_row.child(text("★").font(style::META).color(p.text_3));
    }

    let mut words = col().items_center().gap(5.0).child(name_row).child(meta);
    if !archived && let Some(pulse) = &summary.active_pulse {
        words = words.child(
            row()
                .gap(4.0)
                .center_y()
                .max_w(width - 4.0)
                .child(el().size(5.0, 5.0).radius(2.5).fill(p.live))
                .child(text(pulse.status.clone()).font(style::META).color(p.live).truncate()),
        );
    } else if !archived && summary.needs_you {
        words = words
            .child(el().px(6.0).py(1.0).radius(5.0).fill(p.amber.alpha(0.14)).child(text("Needs you").font(style::META).color(p.amber)));
    }
    if s.settings.show_folders
        && summary.project.renamed()
        && let Some(folder) = summary.project.folder()
    {
        words = words.child(text(folder).font(style::PATH).color(p.text_3.alpha(0.85)).truncate().max_w(width - 4.0));
    }

    let card = col()
        .key(format!("tile-{id}"))
        .role("button")
        .label(label)
        .focusable(true)
        .w(width)
        .items_center()
        .gap(16.0)
        .py(12.0)
        .cursor(Cursor::Pointer)
        .press_scale(0.97)
        .motion(motion::DEFAULT)
        .on_hover_change(move |s: &mut State, on: bool, _tx: &mut Tx| {
            if on {
                s.hovered = Some(id);
            } else if s.hovered == Some(id) {
                s.hovered = None;
            }
        })
        .on_press(move |s: &mut State| s.open_project(id))
        .on_secondary_press(move |s: &mut State, ptr, _| s.orb_menu = Some((id, ptr)))
        .child(sphere)
        .child(words);

    let point = s.orb_menu.and_then(|(mid, ptr)| (mid == id).then_some(ptr));
    let mut ctx =
        context_menu(format!("orb-ctx-{id}"), card, point).item(MenuItem::new("Open").on_select(move |s: &mut State| s.open_project(id)));
    if unseen {
        ctx = ctx.item(MenuItem::new("Mark as Read").on_select(move |s: &mut State| s.mark_seen(id)));
    }
    ctx = ctx.item(MenuItem::new(if pinned { "Unpin" } else { "Pin to Front" }).on_select(move |s: &mut State| s.set_pinned(id, !pinned)));
    if let Some(path) = summary.project.path.clone() {
        ctx = ctx.item(MenuItem::new("Show in Finder").on_select(move |s: &mut State| {
            s.orb_menu = None;
            chrome::reveal_path(&path);
        }));
    }
    ctx.separator()
        .item(MenuItem::new(if archived { "Unarchive" } else { "Archive" }).on_select(move |s: &mut State| s.set_archived(id, !archived)))
        .on_close(|s: &mut State| s.orb_menu = None)
        .build(k, layer)
}

fn all_archived(p: &Palette, size: f32, count: usize) -> El {
    let ghost = el().size(size, size).radius(size / 2.0).border(1.0, p.hairline).radial_gradient(
        [0.35, 0.25],
        size * 0.5,
        [(0.0, p.hairline), (1.0, p.bg.alpha(0.0))],
    );
    col()
        .key("all-archived")
        .items_center()
        .gap(16.0)
        .child(ghost)
        .child(text("All projects archived").font(style::NAME).color(p.text))
        .child(
            text("Archived projects keep their updates and screenshots but stay off Home and Feed.")
                .font(style::META)
                .color(p.text_3)
                .wrap()
                .max_w(380.0),
        )
        .child(
            chrome::bar_button("show-archived", &format!("Show Archived ({count})"), p)
                .border(1.0, p.hairline)
                .on_press(|s: &mut State| s.show_archived = true),
        )
}

fn empty(p: &Palette, size: f32) -> El {
    let ghost = el().size(size, size).radius(size / 2.0).border(1.0, p.hairline).radial_gradient(
        [0.35, 0.25],
        size * 0.5,
        [(0.0, p.hairline), (1.0, p.bg.alpha(0.0))],
    );
    col()
        .key("empty")
        .grow(1.0)
        .w_full()
        .center()
        .gap(18.0)
        .pb(52.0)
        .child(ghost)
        .child(text("No projects yet").font(style::NAME).color(p.text))
        .child(
            text("Connect the coding tools you use, then ask an agent in any project to post a Feeder update.")
                .font(style::META)
                .color(p.text_3)
                .wrap()
                .max_w(380.0),
        )
        .child(chrome::bar_button("connect-tools", "Connect Your Tools", p).border(1.0, p.hairline).on_press_tx(chrome::open_agent_setup))
}

fn message(p: &Palette, title: &str, detail: &str) -> El {
    col()
        .key("message")
        .grow(1.0)
        .w_full()
        .center()
        .gap(8.0)
        .child(text(title.to_string()).font(style::NAME).color(p.text))
        .child(text(detail.to_string()).font(style::META).color(p.text_3).wrap().max_w(420.0))
}
