use feeder_core::{Order, ProjectSummary, ago, extract_block};
use rune::tok::motion;
use rune::{Color, Cursor, El, ScrollSpec, Spring, Transition, Tx, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, MenuItem, Spinner, context_menu};

use crate::connect::{Link, ToolAction};
use crate::style::{self, Palette};
use crate::{CopyTarget, HomeFilter, State, agents, chrome, feed, orb};

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

fn step_badge(num: &str, done: bool, p: &Palette) -> El {
    el().size(22.0, 22.0)
        .radius(11.0)
        .center()
        .fill(if done { p.live.alpha(0.16) } else { p.hover })
        .child(text(if done { "✓".to_string() } else { num.to_string() }).font(style::META).color(if done { p.live } else { p.text_2 }))
}

fn onboarding_card(key: &str, p: &Palette, child: El) -> El {
    col().key(key.to_string()).w_full().padding(16.0).gap(12.0).radius(14.0).fill(p.card).border(1.0, p.hairline).child(child)
}

fn onboarding_tool_row(s: &State, p: &Palette, k: &Kit, tool: feeder_core::setup::Tool, link: &Link) -> El {
    let action = |label: &str, variant: ButtonVariant, connect: bool| {
        Button::new(label.to_string())
            .variant(variant)
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
    let busy = |t: &str| text(t.to_string()).font(style::META).color(p.text_3);
    let control = match link {
        Link::Connected => row()
            .gap(6.0)
            .center_y()
            .child(el().size(6.0, 6.0).radius(3.0).fill(p.live))
            .child(text("Connected").font(style::META).color(p.text_2)),
        Link::Missing => action("Connect", ButtonVariant::Outline, true),
        Link::Outdated => action("Update", ButtonVariant::Outline, true),
        Link::Failed { action: failed_action, .. } => action("Try Again", ButtonVariant::Outline, *failed_action == ToolAction::Connect),
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
        .min_h(28.0)
        .gap(10.0)
        .center_y()
        .child(el().size(18.0, 18.0).center().child(agents::mark(tool.name(), p, 16.0, s.agent_icon(tool.name()))))
        .child(words)
        .child(control)
}

fn welcome_hero(p: &Palette) -> El {
    col()
        .items_center()
        .gap(10.0)
        .pb(4.0)
        .child(orb::orb(&WELCOME_ORB, 56.0))
        .child(text("Welcome to Feeder").font(style::DISPLAY).color(p.text))
        .child(
            text("Connect the coding tools you use, then ask an agent to post an update.")
                .font(style::BODY)
                .color(p.text_2)
                .wrap()
                .max_w(440.0)
                .align(rune::Align::Center),
        )
}

fn connect_step(s: &State, p: &Palette, k: &Kit) -> El {
    let scanning = s.agents.is_scanning();
    let rescan_btn = Button::new(if scanning { "Scanning…" } else { "Scan Again" })
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .disabled(scanning)
        .on_press(|s: &mut State| s.rescan_clis())
        .build(k)
        .key("home-rescan");

    let step1_head = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(step_badge("1", s.agents.has_connected(), p))
        .child(text("Connect your coding tools").font(style::NAME).color(p.text))
        .child(rune::spacer())
        .child(rescan_btn);

    let step1_body = match &s.agents.rows {
        None => row()
            .gap(8.0)
            .center_y()
            .py(4.0)
            .child(Spinner::new().size(13.0).color(p.text_3).build(k))
            .child(text("Looking for coding tools on this Mac…").font(style::META).color(p.text_3)),
        Some(found) if found.is_empty() => {
            text("No coding tools were detected automatically. Copy the MCP config below to connect any tool.")
                .font(style::META)
                .color(p.text_3)
                .wrap()
        }
        Some(found) => {
            let mut list = col().w_full().gap(8.0);
            for (tool, link) in found {
                list = list.child(onboarding_tool_row(s, p, k, *tool, link));
            }
            list
        }
    };

    let mcp_label = if s.is_copied(CopyTarget::McpConfig) { "Copied" } else { "Copy MCP Config" };
    let step1_footer = row()
        .w_full()
        .center_y()
        .gap(8.0)
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
        .child(rune::spacer())
        .child(
            Button::new("All Tool Settings…")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press_tx(chrome::open_agent_setup)
                .build(k)
                .key("home-all-tools"),
        );

    onboarding_card(
        "home-step-1",
        p,
        col().w_full().gap(12.0).child(step1_head).child(step1_body).child(rune::rule(1.0, p.hairline)).child(step1_footer),
    )
}

pub fn starter_prompt(s: &State, p: &Palette, k: &Kit, fill: Color, key: &'static str) -> El {
    let prompt_label = if s.is_copied(CopyTarget::StarterPrompt) { "Copied" } else { "Copy Prompt" };

    row()
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .fill(fill)
        .border(1.0, p.hairline)
        .child(text(crate::STARTER_PROMPT).font(style::BODY).color(p.text).selectable(true).grow(1.0).min_w(0.0))
        .child(
            Button::new(prompt_label)
                .variant(ButtonVariant::Outline)
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
    onboarding_card(
        "home-step-2",
        p,
        col()
            .w_full()
            .gap(10.0)
            .child(
                row()
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .child(step_badge("2", false, p))
                    .child(text("Ask an agent for your first update").font(style::NAME).color(p.text)),
            )
            .child(starter_prompt(s, p, k, p.bg, "home-copy-prompt"))
            .child(
                text("Restart a tool after connecting it, then send that prompt in any project. Projects appear here automatically.")
                    .font(style::META)
                    .color(p.text_3)
                    .wrap(),
            ),
    )
}

fn curator_step(s: &State, p: &Palette, k: &Kit) -> El {
    let curator_ready = crate::curator_settings::selected(s);
    let backends = &s.notebook.gardener.backends;
    let step3_control = if let Some(b) = curator_ready {
        row()
            .gap(6.0)
            .center_y()
            .child(el().size(6.0, 6.0).radius(3.0).fill(p.live))
            .child(text(format!("Using {}", s.curator.prefs.name(b))).font(style::META).color(p.text_2))
    } else {
        let label = if backends.is_empty() { "Curator Settings…" } else { "Choose Subscription…" };
        Button::new(label)
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .on_press(crate::curator_settings::open_settings)
            .build(k)
            .key("home-curator-setup")
    };
    let step3_note = if curator_ready.is_some() {
        "Optional. Ready to turn flagged lessons and Feed updates into Library notes.".to_string()
    } else if !backends.is_empty() {
        let names: Vec<&str> = backends.iter().map(|b| b.label()).collect();
        format!("Optional. Found {} on this Mac. Turn on a subscription to keep your Library updated.", crate::gardener::join_names(&names))
    } else {
        "Optional. Curator keeps your Library updated using Claude Code, Codex, OpenCode, or Antigravity.".to_string()
    };

    onboarding_card(
        "home-step-3",
        p,
        col()
            .w_full()
            .gap(6.0)
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(step_badge("3", curator_ready.is_some(), p))
                    .child(text("Turn on Curator").font(style::NAME).color(p.text))
                    .child(rune::spacer())
                    .child(step3_control),
            )
            .child(text(step3_note).font(style::META).color(p.text_3).wrap().pl(30.0)),
    )
}

fn empty(s: &State, p: &Palette, k: &Kit) -> El {
    let stack = col()
        .key("empty")
        .w_full()
        .max_w(540.0)
        .items_center()
        .gap(14.0)
        .child(welcome_hero(p))
        .child(connect_step(s, p, k))
        .child(first_update_step(s, p, k))
        .child(curator_step(s, p, k));
    col().key("empty-scroll").w_full().grow(1.0).min_h(0.0).child(
        col()
            .w_full()
            .min_h(0.0)
            .grow(1.0)
            .items_center()
            .px(32.0)
            .pt(40.0)
            .pb(64.0)
            .child(stack)
            .scroll(ScrollSpec { stick_bottom: false }),
    )
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
