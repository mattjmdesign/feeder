use feeder_core::{Order, ProjectSummary, ago, extract_block};
use rune::tok::motion;
use rune::{Color, Cursor, El, ScrollSpec, Spring, Transition, Tx, col, el, row, text};
use rune_kit::shell::TOOLBAR_HEIGHT;
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, MenuItem, Spinner, Switch, context_menu, scroll_bar};

use crate::connect::{Link, ToolAction};
use crate::style::{self, Palette};
use crate::{CopyTarget, HomeFilter, OnboardingStep, State, agents, chrome, feed, orb};

pub fn view(s: &State, p: &Palette, k: &Kit, reduced_motion: bool, layer: &mut Vec<El>) -> El {
    let body = if let Some(err) = &s.error {
        message(p, "Can't open the store", err)
    } else if !s.loaded {
        el().key("loading").grow(1.0)
    } else if s.projects.is_empty() {
        empty(s, p, k)
    } else {
        shelf(s, p, k, reduced_motion, layer)
    };

    col().key("home").w_full().grow(1.0).min_h(0.0).transition(Transition::none()).child(body)
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
            let chip = |key: &str, label: String, active: bool, dot: Option<Color>| {
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
            for summary in &active {
                grid = grid.child(tile(s, summary, p, k, reduced_motion, layer, size));
            }
            content = content.child(grid);
        }

        let focus_id = s.hovered.or_else(|| s.home_selected.filter(|id| active.iter().any(|p| p.project.id == *id)));
        let preview = match focus_id.and_then(|id| s.summary(id)) {
            Some(sum) => {
                let line = if sum.needs_you { needs_you_question(sum).map(|q| format!("Needs you: {q}")) } else { None }
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
            None => el(),
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
                .child(text("ARCHIVED").font(style::LABEL).color(p.text_3))
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

    centered_scroll(s, k, "shelf-scroll-area", "shelf-scroll", 12.0, 64.0, content)
}

fn centered_scroll(s: &State, k: &Kit, area_key: &str, scroll_key: &str, pt: f32, pb: f32, content: El) -> El {
    let min_h = (s.window_size[1] - TOOLBAR_HEIGHT).max(0.0);
    let inner = col().w_full().min_h(min_h).justify_center().items_center().px(32.0).pt(pt).pb(pb).child(content);
    let viewport = col().w_full().grow(1.0).min_h(0.0).child(inner).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(16.0);
    scroll_bar(k, area_key, scroll_key, viewport).w_full().grow(1.0).min_h(0.0)
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
            .child(agents::post_mark(s, agent, summary.last_via.as_deref(), p, 13.0))
            .child(text(ago(at, s.now)).font(style::META).color(if hovered || selected { p.text_2 } else { p.text_3 })),
        _ => row().child(text("No updates yet").font(style::META).color(p.text_3)),
    };
    let mut orb_el = orb::orb(&summary.project.orb, size);
    if archived {
        orb_el = orb_el.opacity(0.68);
    }
    let orb_el = orb_el.key(format!("orb-{id}")).matched(format!("orb-{id}")).motion(Spring::new(0.42, 0.28));
    let mut sphere = el().size(size, size).scale(if hovered || selected { 1.06 } else { 1.0 }).motion(Spring::new(0.42, 0.28));
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
    sphere = sphere.child(orb_el);

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
    let status_text =
        |line: String, color| text(line).font(style::META).color(color).align(rune::Align::Center).max_lines(2).max_w(width - 4.0);
    if !archived && let Some(pulse) = &summary.active_pulse {
        words = words.child(status_text(format!("Working · {}", pulse.status), p.live));
    } else if !archived && summary.needs_you {
        words = words
            .child(el().px(6.0).py(1.0).radius(5.0).fill(p.amber.alpha(0.14)).child(text("Needs you").font(style::META).color(p.text)));
        if let Some(question) = needs_you_question(summary) {
            words = words.child(status_text(question, p.text_2));
        }
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

pub fn needs_you_question(sum: &ProjectSummary) -> Option<String> {
    sum.last_body
        .as_deref()
        .and_then(|b| extract_block(b, &["decision needed", "decision", "blocked"]))
        .map(|t| feed::first_line(&t, 96))
        .filter(|q| !q.is_empty())
        .map(|q| {
            let mut chars = q.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
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

const WELCOME_ORB: feeder_core::Orb =
    feeder_core::Orb { seed: 3, hues: [220.0, 265.0, 195.0], chroma: 0.14, angle: 0.9, pattern: feeder_core::Pattern::Pearl };

fn grouped_list(p: &Palette, rows: Vec<El>) -> El {
    let mut list = col().w_full().radius(12.0).fill(p.card).border(1.0, p.hairline).clip_rounded(12.0);
    let count = rows.len();
    for (i, item) in rows.into_iter().enumerate() {
        list = list.child(item);
        if i + 1 < count {
            list = list.child(el().w_full().h(1.0).fill(p.hairline));
        }
    }
    list
}

fn step_indicator(current: OnboardingStep, p: &Palette, k: &Kit) -> El {
    let cur = current.index();
    let mut bar = row().key("onboarding-steps").gap(8.0).center_y();
    for step in OnboardingStep::ALL {
        let idx = step.index();
        let active = idx == cur;
        let fill = if active {
            p.text
        } else if idx < cur {
            p.text_2.alpha(0.75)
        } else {
            p.text_3.alpha(0.32)
        };
        bar = bar.child(
            row()
                .key(format!("onboarding-step-tab-{}", step.key()))
                .role("tab")
                .checked(active)
                .focusable(true)
                .radius(4.0)
                .focus_ring(2.0, 1.0, k.c.border_focus)
                .h(24.0)
                .center()
                .cursor(Cursor::Pointer)
                .label(format!("Step {}: {}", idx + 1, step.label()))
                .on_press(move |s: &mut State| s.onboarding_step = step)
                .child(el().w(28.0).h(3.0).radius(1.5).fill(fill)),
        );
    }
    bar
}

fn step_header(orb_spec: &feeder_core::Orb, title: &str, subtitle: &str, p: &Palette) -> El {
    col()
        .items_center()
        .gap(8.0)
        .child(orb::orb(orb_spec, 48.0))
        .child(text(title.to_string()).font(style::DISPLAY).color(p.text).align(rune::Align::Center).mt(8.0))
        .child(text(subtitle.to_string()).font(style::BODY).color(p.text_2).wrap().max_w(440.0).align(rune::Align::Center))
}

fn onboarding_tool_row(s: &State, p: &Palette, k: &Kit, tool: feeder_core::setup::Tool, link: &Link) -> El {
    let action = |label: &str, connect: bool| {
        Button::new(label.to_string())
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut State| {
                let wake = s.wake.clone();
                if connect {
                    s.agents.connect(tool, wake.as_ref());
                } else {
                    s.agents.disconnect(tool, wake.as_ref());
                }
            })
            .build(k)
            .key(format!("home-agent-action-{}", tool.id()))
    };
    let busy = |label: &str| {
        row()
            .gap(6.0)
            .center_y()
            .child(Spinner::new().size(12.0).color(p.text_3).build(k))
            .child(text(label.to_string()).font(style::META).color(p.text_3))
    };
    let control = match link {
        Link::Connected | Link::Missing => {
            let connected = *link == Link::Connected;
            Switch::new(connected)
                .label(format!("Connect {}", tool.name()))
                .hide_label()
                .on_toggle(move |s: &mut State| {
                    let wake = s.wake.clone();
                    if connected {
                        s.agents.disconnect(tool, wake.as_ref());
                    } else {
                        s.agents.connect(tool, wake.as_ref());
                    }
                })
                .build(k)
                .key(format!("home-agent-toggle-{}", tool.id()))
        }
        Link::Outdated => action("Update", true),
        Link::Failed { action: failed_action, .. } => action("Try Again", *failed_action == ToolAction::Connect),
        Link::Connecting => busy("Connecting…"),
        Link::Disconnecting => busy("Disconnecting…"),
    };
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(text(tool.name()).font(style::NAME).color(p.text).truncate());
    match link {
        Link::Outdated => words = words.child(text("Set up for another copy of Feeder").font(style::META).color(p.text_3).truncate()),
        Link::Failed { reason, .. } => {
            words = words
                .child(text(format!("Couldn’t connect: {reason}")).font(style::META).color(k.c.danger.fg).truncate().help(reason.clone()));
        }
        _ => {}
    }
    row()
        .key(format!("home-agent-{}", tool.id()))
        .w_full()
        .min_h(44.0)
        .px(16.0)
        .py(8.0)
        .gap(12.0)
        .center_y()
        .child(el().size(20.0, 20.0).center().child(agents::mark(tool.name(), p, 18.0, s.agent_icon(tool.name()))))
        .child(words)
        .child(control)
}

fn connect_step(s: &State, p: &Palette, k: &Kit) -> El {
    let rows = match &s.agents.rows {
        None => vec![
            row()
                .w_full()
                .px(16.0)
                .py(16.0)
                .gap(8.0)
                .center()
                .child(Spinner::new().size(13.0).color(p.text_3).build(k))
                .child(text("Looking for coding tools on this Mac…").font(style::META).color(p.text_3)),
        ],
        Some(found) if found.is_empty() => vec![
            text("No supported tools were detected on this Mac. Copy the MCP config below to connect any tool.")
                .font(style::META)
                .color(p.text_3)
                .wrap()
                .px(16.0)
                .py(16.0),
        ],
        Some(found) => found.iter().map(|(tool, link)| onboarding_tool_row(s, p, k, *tool, link)).collect(),
    };

    let mcp_label = if s.is_copied(CopyTarget::McpConfig) { "Copied" } else { "Copy MCP Config" };
    let scanning = s.agents.is_scanning();
    let utility = row()
        .w_full()
        .justify_center()
        .center_y()
        .gap(16.0)
        .child(
            Button::new(mcp_label)
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    tx.copy(feeder_core::setup::mcp_json(&feeder_core::setup::feeder_binary()));
                    s.mark_copied(CopyTarget::McpConfig);
                })
                .build(k)
                .key("home-copy-mcp"),
        )
        .child(
            Button::new(if scanning { "Scanning…" } else { "Scan Again" })
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .disabled(scanning)
                .on_press(|s: &mut State| s.rescan_clis())
                .build(k)
                .key("home-rescan"),
        );

    let footer = row()
        .w_full()
        .center_y()
        .child(
            Button::new("Skip")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut State| s.set_onboarding_skipped(true))
                .build(k)
                .key("home-onboarding-skip"),
        )
        .child(rune::spacer())
        .child(
            Button::new("Continue")
                .variant(ButtonVariant::Contrast)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut State| s.onboarding_step = OnboardingStep::FirstUpdate)
                .build(k)
                .key("home-onboarding-next"),
        );

    col()
        .w_full()
        .items_center()
        .gap(24.0)
        .child(step_header(
            &WELCOME_ORB,
            "Connect your coding tools",
            "Connect adds Feeder to your tool’s settings and gives it access to your Feed and Library.",
            p,
        ))
        .child(col().w_full().gap(8.0).child(grouped_list(p, rows)).child(utility))
        .child(footer)
}

pub fn starter_prompt(s: &State, p: &Palette, k: &Kit, fill: Color, key: &'static str) -> El {
    let prompt_label = if s.is_copied(CopyTarget::StarterPrompt) { "Copied" } else { "Copy Prompt" };

    row()
        .w_full()
        .px(16.0)
        .py(12.0)
        .gap(12.0)
        .center_y()
        .radius(12.0)
        .fill(fill)
        .border(1.0, p.hairline)
        .child(text(crate::STARTER_PROMPT).font(style::BODY).color(p.text).selectable(true).grow(1.0).min_w(0.0))
        .child(
            Button::new(prompt_label)
                .variant(ButtonVariant::Neutral)
                .control_size(ControlSize::Small)
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    tx.copy(crate::STARTER_PROMPT.to_string());
                    s.mark_copied(CopyTarget::StarterPrompt);
                })
                .build(k)
                .key(key),
        )
}

fn first_update_step(s: &State, p: &Palette, k: &Kit) -> El {
    let waiting = row()
        .gap(8.0)
        .center_y()
        .child(el().size(6.0, 6.0).radius(3.0).fill(p.live.alpha(0.75)))
        .child(text("Waiting for an update. Projects appear here automatically.").font(style::META).color(p.text_3));

    let footer = row()
        .w_full()
        .center_y()
        .child(
            Button::new("Back")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut State| s.onboarding_step = OnboardingStep::Connect)
                .build(k)
                .key("home-onboarding-back"),
        )
        .child(rune::spacer())
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    Button::new("Skip")
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Regular)
                        .on_press(|s: &mut State| s.set_onboarding_skipped(true))
                        .build(k)
                        .key("home-onboarding-skip"),
                )
                .child(
                    Button::new("Continue")
                        .variant(ButtonVariant::Contrast)
                        .control_size(ControlSize::Regular)
                        .on_press(|s: &mut State| s.onboarding_step = OnboardingStep::Curator)
                        .build(k)
                        .key("home-onboarding-next"),
                ),
        );

    col()
        .w_full()
        .items_center()
        .gap(24.0)
        .child(step_header(&WELCOME_ORB, "Ask for your first update", "Restart connected tools, then send this prompt in any project.", p))
        .child(col().w_full().items_center().gap(12.0).child(starter_prompt(s, p, k, p.card, "home-copy-prompt")).child(waiting))
        .child(footer)
}

fn curator_step(s: &State, p: &Palette, k: &Kit) -> El {
    let backends = &s.notebook.gardener.backends;
    let rows: Vec<El> = if backends.is_empty() {
        vec![
            text("No supported CLIs detected yet. Install Claude Code, Codex, OpenCode, or Antigravity, then scan again.")
                .font(style::META)
                .color(p.text_3)
                .wrap()
                .px(16.0)
                .py(16.0),
        ]
    } else {
        backends
            .iter()
            .map(|&backend| {
                let on = s.curator.prefs.is_enabled(backend);
                let usable = s.curator_sign_in.usable(backend);
                let (status, action) = crate::curator_sign_in::cells(s, backend, p, k);
                let toggle = Switch::new(on)
                    .label(if !on && !usable {
                        format!("Use {} for Curator, sign in first", s.curator.prefs.name(backend))
                    } else {
                        format!("Use {} for Curator", s.curator.prefs.name(backend))
                    })
                    .hide_label()
                    .disabled(!on && !usable)
                    .on_toggle(move |s: &mut State| {
                        let next = !s.curator.prefs.is_enabled(backend);
                        let wake = s.wake.clone();
                        s.curator.set_enabled(backend, next, wake.as_ref());
                    })
                    .build(k)
                    .key(format!("home-curator-toggle-{}", backend.as_str()));
                let words = col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(2.0)
                    .child(text(s.curator.prefs.name(backend)).font(style::NAME).color(p.text).truncate())
                    .child(status);
                row()
                    .key(format!("home-curator-{}", backend.as_str()))
                    .w_full()
                    .min_h(52.0)
                    .px(16.0)
                    .py(12.0)
                    .gap(12.0)
                    .center_y()
                    .child(el().size(20.0, 20.0).center().child(agents::mark(backend.label(), p, 18.0, s.agent_icon(backend.label()))))
                    .child(words)
                    .child(row().gap(8.0).center_y().child(action).child(toggle))
            })
            .collect()
    };

    let scanning = s.curator_sign_in.is_scanning();
    let utility = row()
        .w_full()
        .justify_center()
        .center_y()
        .gap(16.0)
        .child(
            Button::new("Curator Settings…")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(crate::curator_settings::open_settings)
                .build(k)
                .key("home-curator-setup"),
        )
        .child(
            Button::new(if scanning { "Scanning…" } else { "Scan Again" })
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .disabled(scanning)
                .on_press(|s: &mut State| s.rescan_clis())
                .build(k)
                .key("home-curator-rescan"),
        );

    let done_label = if crate::curator_settings::selected(s).is_some() { "Done" } else { "Skip for Now" };
    let footer = row()
        .w_full()
        .center_y()
        .child(
            Button::new("Back")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut State| s.onboarding_step = OnboardingStep::FirstUpdate)
                .build(k)
                .key("home-onboarding-back"),
        )
        .child(rune::spacer())
        .child(
            Button::new(done_label)
                .variant(ButtonVariant::Contrast)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut State| s.set_onboarding_skipped(true))
                .build(k)
                .key("home-onboarding-done"),
        );

    col()
        .w_full()
        .items_center()
        .gap(24.0)
        .child(step_header(
            &chrome::CURATOR_ORB,
            "Turn on Curator",
            "Optional. Curator turns flagged updates into Library notes using a subscription you already have.",
            p,
        ))
        .child(col().w_full().gap(8.0).child(grouped_list(p, rows)).child(utility))
        .child(footer)
}

fn skipped_empty(s: &State, p: &Palette, k: &Kit) -> El {
    let prompt_label = if s.is_copied(CopyTarget::StarterPrompt) { "Copied" } else { "Copy Prompt" };
    let copy_btn = |variant| {
        Button::new(prompt_label)
            .variant(variant)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                tx.copy(crate::STARTER_PROMPT.to_string());
                s.mark_copied(CopyTarget::StarterPrompt);
            })
            .build(k)
            .key("home-skipped-copy-prompt")
    };
    let setup_btn = |variant| {
        Button::new("Set Up Tools")
            .variant(variant)
            .control_size(ControlSize::Small)
            .on_press(|s: &mut State| s.set_onboarding_skipped(false))
            .build(k)
            .key("home-skipped-setup")
    };
    let actions = if s.agents.has_connected() {
        row().gap(8.0).pt(8.0).center_y().child(copy_btn(ButtonVariant::Neutral)).child(setup_btn(ButtonVariant::GhostMuted))
    } else {
        row().gap(8.0).pt(8.0).center_y().child(setup_btn(ButtonVariant::Contrast)).child(copy_btn(ButtonVariant::GhostMuted))
    };
    col()
        .key("empty-skipped")
        .w_full()
        .max_w(400.0)
        .items_center()
        .gap(12.0)
        .child(orb::orb(&WELCOME_ORB, 56.0).opacity(0.88))
        .child(text("No projects yet").font(style::SECTION_TITLE).color(p.text))
        .child(
            text("Ask a connected coding agent to post a Feeder update. Projects appear here automatically.")
                .font(style::BODY)
                .color(p.text_2)
                .wrap()
                .align(rune::Align::Center),
        )
        .child(actions)
}

fn empty(s: &State, p: &Palette, k: &Kit) -> El {
    let (scroll_key, stage) = if s.settings.onboarding_skipped {
        ("home-empty-skipped".to_string(), skipped_empty(s, p, k))
    } else {
        let stage = col()
            .key(format!("onboarding-{}", s.onboarding_step.key()))
            .w_full()
            .max_w(440.0)
            .items_center()
            .gap(24.0)
            .child(step_indicator(s.onboarding_step, p, k))
            .child(match s.onboarding_step {
                OnboardingStep::Connect => connect_step(s, p, k),
                OnboardingStep::FirstUpdate => first_update_step(s, p, k),
                OnboardingStep::Curator => curator_step(s, p, k),
            });
        (format!("home-empty-{}", s.onboarding_step.key()), stage)
    };
    centered_scroll(s, k, "empty-scroll-area", &scroll_key, 32.0, 64.0, stage)
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
