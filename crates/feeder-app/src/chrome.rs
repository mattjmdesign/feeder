//! The window's top bar, its menu, lightbox (zoom/pan, video, before/after compare),
//! Quick Jump palette, toasts, and Settings sheet.

use std::path::Path;

use feeder_core::setup::{self, Tool};
use feeder_core::{OrbSize, Order, Theme, ago};
use rune::{
    ClipboardContent, ClipboardImage, Cursor, Drag, El, Icon, Key, KeyEvent, ScrollSpec, SurfaceFit, Transition, Tx, col, el, oklch, row,
    text, text_input,
};
use rune_kit::search::Fold;
use rune_kit::{
    Button, ButtonVariant, ControlSize, Kit, Menu, MenuItem, Placement, Preview, SegmentedControl, Switch, Toast, ToastRegion, Toolbar,
};

use crate::connect::Link;
use crate::style::{self, Palette};
use crate::{CopyTarget, State, Tab, ToastItem, ToastKind, agents, feed, orb};

static PLUS_ICON: Icon = Icon {
    name: "feeder-plus",
    view: 20.0,
    body: r#"<rect x="5" y="9.4" width="10" height="1.2" rx="0.6" fill="currentColor"/><rect x="9.4" y="5" width="1.2" height="10" rx="0.6" fill="currentColor"/>"#,
};

/// The bar over Home and a project's feed: rune-kit's toolbar band, as the Notebook's
/// `SplitView` draws it, so the view tabs and the menu hold their place across every view.
/// `leading` starts past the traffic lights; `trailing` sits before the menu.
pub fn bar(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>, inset: f32, leading: El, trailing: Option<El>) -> El {
    let mut toolbar = Toolbar::new().leading(row().pl(inset.max(0.0)).center_y().child(leading));
    if let Some(t) = trailing {
        toolbar = toolbar.trailing(t);
    }
    toolbar.trailing(menu(s, p, k, layer)).build(k).key("bar")
}

/// Home | Feed | Notebook, top right.
pub fn tabs(s: &State, k: &Kit) -> El {
    let labels: Vec<&str> = Tab::ALL.iter().map(|t| t.label()).collect();
    SegmentedControl::new(&labels, Tab::ALL.iter().position(|t| *t == s.tab))
        .label("View")
        .fit()
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            s.tab = Tab::ALL.get(i).copied().unwrap_or_default();
            s.sync_search();
        })
        .build(k)
}

/// A quiet text button for the bar.
pub fn bar_button(key: &str, label: &str, p: &Palette) -> El {
    row()
        .key(key.to_string())
        .role("button")
        .label(label.to_string())
        .focusable(true)
        .h(28.0)
        .px(10.0)
        .center_y()
        .radius(8.0)
        .hover(p.hover)
        .cursor(Cursor::Pointer)
        .child(text(label.to_string()).font(style::META).color(p.text_2))
}

pub fn reveal_path(path: &Path) {
    let is_plain_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
        && !matches!(
            path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
            Some("app" | "bundle" | "pkg" | "mpkg" | "prefpane" | "appex" | "plugin" | "workflow" | "action")
        );
    let mut cmd = std::process::Command::new("/usr/bin/open");
    if is_plain_dir {
        cmd.args(["-a", "Finder", "--"]).arg(path);
    } else {
        cmd.args(["-R", "--"]).arg(path);
    }
    let _ = cmd.spawn();
}

fn copy_image_to_clipboard(path: &Path, tx: &mut Tx) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read image: {e}"))?;
    if let Some(img) = ClipboardImage::from_png(bytes) {
        tx.copy_rich(ClipboardContent::new("").with_image(img));
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        static NEXT_CLIP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT_CLIP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("feeder-clip-{}-{n}.png", std::process::id()));
        let status = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png"])
            .arg(path)
            .arg("--out")
            .arg(&tmp)
            .output()
            .map_err(|e| format!("Couldn't convert image: {e}"))?;
        let png = std::fs::read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        if !status.status.success() {
            return Err("Couldn't convert image to PNG".into());
        }
        let png = png.map_err(|e| format!("Couldn't read converted PNG: {e}"))?;
        let img = ClipboardImage::from_png(png).ok_or_else(|| "Image is too large or not valid PNG".to_string())?;
        tx.copy_rich(ClipboardContent::new("").with_image(img));
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Only PNG images can be copied on this platform".into())
    }
}

pub fn menu(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let dot = || el().size(3.5, 3.5).radius(1.75).fill(p.text_2);
    let trigger = row()
        .key("menu-button")
        .role("button")
        .label("Menu")
        .focusable(true)
        .size(30.0, 28.0)
        .gap(3.0)
        .center()
        .radius(8.0)
        .hover(p.hover)
        .fill(if s.menu_open { p.hover } else { p.hover.alpha(0.0) })
        .cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.menu_open = !s.menu_open)
        .child(dot())
        .child(dot())
        .child(dot());
    let archived_count = s.projects.iter().filter(|p| p.project.archived).count();
    let unseen_count = s.unseen_count();
    let mut m = Menu::new("main-menu")
        .trigger(trigger)
        .open(s.menu_open)
        .placement(Placement::BottomEnd)
        .min_width(236.0)
        .on_close(|s: &mut State| s.menu_open = false)
        .item(MenuItem::new("Quick Jump…").shortcut("⌘K").on_select_tx(|s: &mut State, tx: &mut Tx| s.open_palette(tx)))
        .item(MenuItem::new("Settings…").shortcut("⌘,").on_select(|s: &mut State| {
            s.menu_open = false;
            s.settings_open = true;
        }));
    if unseen_count > 0 {
        m = m.item(MenuItem::new(format!("Mark All Read ({unseen_count})")).on_select(|s: &mut State| {
            s.menu_open = false;
            s.mark_all_seen();
        }));
    }
    if archived_count > 0 {
        let label = if s.show_archived { "Hide Archived Projects".to_string() } else { format!("Show Archived ({archived_count})") };
        m = m.item(MenuItem::new(label).on_select(|s: &mut State| {
            s.menu_open = false;
            s.open = None;
            s.tab = Tab::Home;
            s.show_archived = !s.show_archived;
        }));
    }
    m.item(MenuItem::new("Connect Agents…").on_select_tx(|s: &mut State, tx: &mut Tx| {
        s.menu_open = false;
        open_agent_setup(s, tx);
    }))
    .item(MenuItem::new("Show Data in Finder").on_select(|s: &mut State| {
        s.menu_open = false;
        reveal_path(&feeder_core::data_dir());
    }))
    .separator()
    .item(MenuItem::new("Quit Feeder").shortcut("⌘Q").on_select_tx(|_: &mut State, tx: &mut Tx| tx.quit()))
    .build(k, layer)
}

fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let mins = total / 60;
    let rem = total % 60;
    format!("{mins}:{rem:02}")
}

/// Interactive media preview: zoom/pan images, Before/After swipe compare, and video playback.
pub fn lightbox(s: &State, inset: f32, p: &Palette, k: &Kit, reduced_motion: bool) -> Option<El> {
    let id = s.lightbox?;
    let update = s.find_update(id);
    let media_path = update.and_then(|u| u.screenshot.clone());
    let caption = update
        .map(|u| {
            let proj = s.summary(u.project_id).map(|sum| sum.project.name.as_str()).unwrap_or("Project");
            let branch = u.branch.as_deref().map(|b| format!(" · {b}")).unwrap_or_default();
            format!("{proj} · {} · {}{branch}", u.agent, ago(u.created_at, s.now))
        })
        .unwrap_or_default();

    let ids = s.lightbox_ids();
    let pos = ids.iter().position(|&x| x == id).unwrap_or(0);
    let total = ids.len();

    let backdrop = if p.dark { oklch(0.0, 0.0, 0.0).alpha(0.88) } else { oklch(0.0, 0.0, 0.0).alpha(0.72) };

    if let Some(player) = s.videos.get(&id) {
        let (vw, vh) = player.natural_size().unwrap_or((1280, 720));
        let max_w = (s.window_size[0] - 120.0).max(280.0);
        let max_h = (s.window_size[1] - 190.0).max(220.0);
        let scale = (max_w / vw.max(1) as f32).min(max_h / vh.max(1) as f32).min(1.0);
        let box_w = (vw.max(1) as f32 * scale).max(240.0);
        let box_h = (vh.max(1) as f32 * scale).max(160.0);
        let playing = player.is_playing();
        let dur = player.duration().filter(|d| d.is_finite() && *d > 0.0);
        let cur = player.time().max(0.0);
        let frac = dur.map(|d| (cur / d).clamp(0.0, 1.0) as f32).unwrap_or(0.0);
        let timecode = match dur {
            Some(d) => format!("{} / {}", format_duration(cur.min(d)), format_duration(d)),
            None => format_duration(cur),
        };
        let vid_el = el()
            .key(format!("lightbox-vid-{id}"))
            .w(box_w)
            .h(box_h)
            .radius(12.0)
            .border(1.0, p.hairline)
            .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.65 } else { 0.30 }))
            .video(player, SurfaceFit::Fit)
            .on_press(move |s: &mut State| s.toggle_video(id));

        let mut track = row().key("lightbox-vid-track").w_full().h(16.0).center_y();
        if frac > 0.001 {
            track = track.child(el().grow(frac).h(4.0).radius(2.0).fill(p.text));
        }
        track = track.child(el().size(10.0, 10.0).radius(5.0).fill(p.text).border(1.5, p.card));
        if 1.0 - frac > 0.001 {
            track = track.child(el().grow(1.0 - frac).h(4.0).radius(2.0).fill(p.hairline));
        }

        let scrub = row()
            .key("lightbox-vid-scrub")
            .grow(1.0)
            .min_w(0.0)
            .h(24.0)
            .center_y()
            .cursor(if dur.is_some() { Cursor::Pointer } else { Cursor::Default })
            .on_drag(move |s: &mut State, d: &mut Drag, _| {
                if d.w > 0.0
                    && let Some(player) = s.videos.get(&id)
                    && let Some(d_sec) = player.duration().filter(|v| v.is_finite() && *v > 0.0)
                {
                    let target = (d.x / d.w).clamp(0.0, 1.0) as f64 * d_sec;
                    player.seek(target);
                }
            })
            .child(track);

        let controls = row()
            .key("lightbox-vid-controls")
            .w(box_w)
            .h(38.0)
            .px(10.0)
            .gap(10.0)
            .center_y()
            .radius(10.0)
            .fill(p.card.alpha(0.94))
            .border(1.0, p.hairline)
            .child(
                bar_button("lightbox-vid-play", if playing { "Pause" } else { "Play" }, p)
                    .on_press(move |s: &mut State| s.toggle_video(id)),
            )
            .child(scrub)
            .child(text(timecode).font(style::META).color(p.text_2));

        let mut top = lightbox_top_chrome(s, inset, p, &caption, pos, total, false, None);
        if let Some(path) = media_path {
            top = top.child(
                bar_button("lightbox-open", "Open File", p)
                    .fill(p.card)
                    .border(1.0, p.hairline)
                    .on_press(move |_: &mut State| reveal_path(&path)),
            );
        }
        top = top.child(
            bar_button("lightbox-close", "×", p)
                .fill(p.card)
                .border(1.0, p.hairline)
                .label("Close preview")
                .on_press(|s: &mut State| s.close_lightbox()),
        );

        return Some(
            el().key("lightbox-scrim")
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .fill(backdrop)
                .center()
                .transition(Transition::opacity())
                .on_press(|s: &mut State| s.close_lightbox())
                .child(col().gap(10.0).items_center().on_press(|_: &mut State| {}).child(vid_el).child(controls))
                .child(top),
        );
    }

    let img = s.shots.get(&id)?;
    let [w, h] = img.size()?;

    let compare_target = s.lightbox_compare.and_then(|cid| (cid != id && s.shots.contains_key(&cid)).then_some(cid));

    let img_ids: Vec<i64> = ids.iter().copied().filter(|x| s.shots.contains_key(x)).collect();
    let can_compare = img_ids.len() >= 2;

    let mut top = lightbox_top_chrome(s, inset, p, &caption, pos, total, can_compare, compare_target);
    if let Some(path) = media_path {
        let copy_label = if s.is_copied(CopyTarget::LightboxImage) { "Copied" } else { "Copy Image" };
        top = top.child(
            bar_button("lightbox-copy-image", copy_label, p)
                .fill(p.card)
                .border(1.0, p.hairline)
                .label("Copy screenshot image")
                .on_press_tx(move |s: &mut State, tx: &mut Tx| match copy_image_to_clipboard(&path, tx) {
                    Ok(()) => {
                        s.mark_copied(CopyTarget::LightboxImage);
                        s.push_toast(
                            "copy-image",
                            ToastItem { title: "Copied screenshot to clipboard".into(), description: None, kind: ToastKind::Info },
                            Some(2.5),
                        );
                    }
                    Err(err) => {
                        s.push_toast(
                            "copy-image-err",
                            ToastItem { title: "Couldn't copy image".into(), description: Some(err), kind: ToastKind::Error },
                            Some(4.0),
                        );
                    }
                }),
        );
    }

    if let Some(before_id) = compare_target
        && let Some(before_img) = s.shots.get(&before_id)
    {
        let max_w = (s.window_size[0] - 120.0).max(280.0);
        let max_h = (s.window_size[1] - 140.0).max(220.0);
        let scale = (max_w / w.max(1) as f32).min(max_h / h.max(1) as f32).min(1.0);
        let box_w = (w.max(1) as f32 * scale).max(240.0);
        let box_h = (h.max(1) as f32 * scale).max(160.0);
        let split = s.lightbox_split.clamp(0.02, 0.98);
        let left_w = box_w * split;

        let pill = |label: &str| {
            el().px(8.0)
                .py(3.0)
                .radius(6.0)
                .fill(oklch(0.10, 0.0, 0.0).alpha(0.75))
                .border(1.0, oklch(1.0, 0.0, 0.0).alpha(0.18))
                .child(text(label.to_string()).font(style::META).color(oklch(0.96, 0.0, 0.0)))
        };

        let compare_box = el()
            .key("lightbox-compare-box")
            .w(box_w)
            .h(box_h)
            .radius(12.0)
            .border(1.0, p.hairline)
            .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.65 } else { 0.30 }))
            .cursor(Cursor::Grab)
            .on_drag(|s: &mut State, d: &mut Drag, _| {
                if d.w > 0.0 {
                    s.lightbox_split = (d.x / d.w).clamp(0.02, 0.98);
                }
            })
            .on_press(|_: &mut State| {})
            .child(el().w(box_w).h(box_h).pixels_fit(before_img.pixels(), SurfaceFit::Fit))
            .child(
                el().absolute(0.0, 0.0)
                    .w(left_w)
                    .h(box_h)
                    .clip_rounded(0.0)
                    .child(el().w(box_w).h(box_h).pixels_fit(img.pixels(), SurfaceFit::Fit)),
            )
            .child(el().absolute(left_w - 1.0, 0.0).w(2.0).h(box_h).fill(oklch(1.0, 0.0, 0.0).alpha(0.92)))
            .child(
                el().absolute(left_w - 13.0, box_h * 0.5 - 13.0)
                    .size(26.0, 26.0)
                    .radius(13.0)
                    .fill(oklch(0.14, 0.0, 0.0).alpha(0.92))
                    .border(1.5, oklch(1.0, 0.0, 0.0).alpha(0.9))
                    .center()
                    .child(text("↔").font(style::META).color(oklch(0.98, 0.0, 0.0))),
            )
            .child(el().absolute(12.0, 12.0).child(pill("After")))
            .child(el().pin(None, Some(12.0), Some(12.0), None).child(pill("Before")));

        top = top.child(
            bar_button("lightbox-close", "×", p)
                .fill(p.card)
                .border(1.0, p.hairline)
                .label("Close preview")
                .on_press(|s: &mut State| s.close_lightbox()),
        );

        return Some(
            el().key("lightbox-scrim")
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .fill(backdrop)
                .center()
                .transition(Transition::opacity())
                .on_press(|s: &mut State| s.close_lightbox())
                .child(compare_box)
                .child(top),
        );
    }

    let img_el = el()
        .key(format!("lightbox-img-{id}"))
        .border(1.0, p.hairline)
        .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.65 } else { 0.30 }))
        .pixels_fit(img.pixels(), SurfaceFit::Fit);

    let preview = Preview::new(|s: &mut State| &mut s.lightbox_zoom, [w.max(1) as f32, h.max(1) as f32], img_el)
        .icons(k.glyph.dash, &PLUS_ICON)
        .on_close(|s: &mut State, _| s.close_lightbox())
        .build(&s.lightbox_zoom, k, s.window_size, reduced_motion);

    Some(
        el().key("lightbox-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(backdrop)
            .transition(Transition::opacity())
            .child(preview)
            .child(top),
    )
}

#[expect(clippy::too_many_arguments, reason = "one call site; each argument is a separate piece of the lightbox bar")]
fn lightbox_top_chrome(
    _s: &State,
    inset: f32,
    p: &Palette,
    caption: &str,
    pos: usize,
    total: usize,
    can_compare: bool,
    compare_target: Option<i64>,
) -> El {
    let mut top = row().key("lightbox-top").pin(Some(inset.max(0.0) + 16.0), Some(12.0), Some(16.0), None).center_y().gap(8.0);
    if !caption.is_empty() {
        top = top.child(
            el().px(10.0)
                .py(5.0)
                .radius(8.0)
                .fill(p.card.alpha(0.94))
                .border(1.0, p.hairline)
                .child(text(caption.to_string()).font(style::META).color(p.text_2).truncate().max_w(420.0)),
        );
    }
    top = top.child(rune::spacer());
    if can_compare {
        let active = compare_target.is_some();
        top = top.child(
            bar_button("lightbox-compare", if active { "Exit Compare" } else { "Compare" }, p)
                .fill(p.card)
                .border(1.0, if active { p.text_2 } else { p.hairline })
                .label("Compare before and after screenshots")
                .on_press(|s: &mut State| s.toggle_lightbox_compare()),
        );
    }
    if total > 1 {
        top = top
            .child(
                bar_button("lightbox-prev", "‹", p)
                    .fill(p.card)
                    .border(1.0, p.hairline)
                    .label("Previous media")
                    .on_press(|s: &mut State| s.step_lightbox(-1)),
            )
            .child(
                el().px(8.0)
                    .py(5.0)
                    .radius(8.0)
                    .fill(p.card.alpha(0.94))
                    .border(1.0, p.hairline)
                    .child(text(format!("{} / {total}", pos + 1)).font(style::META).color(p.text_2)),
            )
            .child(
                bar_button("lightbox-next", "›", p)
                    .fill(p.card)
                    .border(1.0, p.hairline)
                    .label("Next media")
                    .on_press(|s: &mut State| s.step_lightbox(1)),
            );
    }
    top
}

#[derive(Clone)]
pub enum PaletteAction {
    OpenProject(i64),
    GoHome,
    GoFeed,
    GoNotebook,
    OpenPage(i64),
    MarkAllRead,
    ToggleArchived,
    OpenSettings,
}

pub struct PaletteItem {
    pub action: PaletteAction,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
    pub project_id: Option<i64>,
    pub unseen: bool,
}

pub fn palette_items(s: &State) -> Vec<PaletteItem> {
    let fold = Fold::default();
    let q = s.palette_query.text.trim();
    let mut items = Vec::new();
    for sum in &s.projects {
        let name = &sum.project.name;
        let folder = sum.project.folder().unwrap_or_default();
        if !q.is_empty() && !fold.contains(name, q) && !fold.contains(&folder, q) {
            continue;
        }
        let subtitle = match (sum.last_at, &sum.last_agent) {
            (Some(at), Some(agent)) => format!("{agent} · {}", ago(at, s.now)),
            _ => "No updates yet".into(),
        };
        let badge = if sum.needs_you {
            Some("Needs you".into())
        } else if sum.active_pulse.is_some() {
            Some("Working".into())
        } else if sum.project.archived {
            Some("Archived".into())
        } else if sum.project.pinned {
            Some("Pinned".into())
        } else {
            None
        };
        items.push(PaletteItem {
            action: PaletteAction::OpenProject(sum.project.id),
            title: name.clone(),
            subtitle,
            badge,
            project_id: Some(sum.project.id),
            unseen: sum.unseen(),
        });
    }
    let commands = [
        (PaletteAction::GoHome, "Go to Home", "Project orb shelf · ⌘1"),
        (PaletteAction::GoFeed, "Go to Feed", "All active updates · ⌘2"),
        (PaletteAction::GoNotebook, "Go to Notebook", "Lessons learned across projects · ⌘3"),
        (PaletteAction::MarkAllRead, "Mark All Read", "Clear unseen project dots"),
        (
            PaletteAction::ToggleArchived,
            if s.show_archived { "Hide Archived Projects" } else { "Show Archived Projects" },
            "Toggle archived shelf on Home",
        ),
        (PaletteAction::OpenSettings, "Open Settings", "Appearance, order, agent setup · ⌘,"),
    ];
    for (action, title, subtitle) in commands {
        if matches!(action, PaletteAction::MarkAllRead) && s.unseen_count() == 0 {
            continue;
        }
        if matches!(action, PaletteAction::ToggleArchived) && !s.projects.iter().any(|p| p.project.archived) {
            continue;
        }
        if !q.is_empty() && !fold.contains(title, q) && !fold.contains(subtitle, q) {
            continue;
        }
        items.push(PaletteItem { action, title: title.into(), subtitle: subtitle.into(), badge: None, project_id: None, unseen: false });
    }
    if !q.is_empty() {
        // Notebook pages whose title, tags or rule mention the words, so a lesson is one ⌘K away.
        let pages = s.notebook.pages.iter().filter(|pg| {
            !pg.is_archived()
                && (fold.contains(&pg.title, q)
                    || fold.contains(&pg.rule, q)
                    || fold.contains(&pg.topic, q)
                    || fold.contains(&feeder_core::topic_label(&pg.topic), q)
                    || pg.tags.iter().any(|t| fold.contains(t, q)))
        });
        for pg in pages.take(4) {
            items.push(PaletteItem {
                action: PaletteAction::OpenPage(pg.id),
                title: pg.title.clone(),
                subtitle: feed::first_line(&pg.rule, 60),
                badge: Some(pg.kind.label().into()),
                project_id: pg.project_id,
                unseen: false,
            });
        }
    }
    if !q.is_empty() && s.search_snapshot_query == q {
        for u in s.search_hits.iter().take(5) {
            let proj = s.summary(u.project_id).map(|sum| sum.project.name.as_str()).unwrap_or("Project");
            let preview = feed::first_line(&u.body, 52);
            let subtitle = format!("{proj} · {} · {}", u.agent, ago(u.created_at, s.now));
            items.push(PaletteItem {
                action: PaletteAction::OpenProject(u.project_id),
                title: if preview.is_empty() { format!("Update #{}", u.id) } else { preview },
                subtitle,
                badge: u.branch.clone().or_else(|| Some("Update".into())),
                project_id: Some(u.project_id),
                unseen: false,
            });
        }
    }
    items
}

pub fn move_palette(s: &mut State, delta: isize) {
    let len = palette_items(s).len();
    if len == 0 {
        s.palette_index = 0;
        return;
    }
    s.palette_index = (s.palette_index.min(len - 1) as isize + delta).rem_euclid(len as isize) as usize;
}

pub fn run_palette(s: &mut State) {
    let items = palette_items(s);
    if items.is_empty() {
        return;
    }
    let idx = s.palette_index.min(items.len() - 1);
    execute_palette_action(s, items[idx].action.clone());
}

fn execute_palette_action(s: &mut State, action: PaletteAction) {
    s.close_palette();
    match action {
        PaletteAction::OpenProject(id) => s.open_project(id),
        PaletteAction::GoHome => {
            s.open = None;
            s.tab = Tab::Home;
            s.sync_search();
        }
        PaletteAction::GoFeed => {
            s.open = None;
            s.tab = Tab::Feed;
            s.sync_search();
        }
        PaletteAction::GoNotebook => {
            s.open = None;
            s.tab = Tab::Notebook;
            s.sync_search();
        }
        PaletteAction::OpenPage(id) => {
            s.open = None;
            s.tab = Tab::Notebook;
            s.notebook.editor = None;
            s.notebook.open = Some(id);
            s.sync_search();
        }
        PaletteAction::MarkAllRead => s.mark_all_seen(),
        PaletteAction::ToggleArchived => {
            s.open = None;
            s.tab = Tab::Home;
            s.show_archived = !s.show_archived;
            s.sync_search();
        }
        PaletteAction::OpenSettings => s.settings_open = true,
    }
}

/// Quick Jump (`⌘K`) command, project & full-history update switcher overlay.
pub fn palette(s: &State, p: &Palette, _k: &Kit) -> Option<El> {
    if !s.palette_open {
        return None;
    }
    let items = palette_items(s);
    let selected = if items.is_empty() { 0 } else { s.palette_index.min(items.len() - 1) };

    let input = text_input(&s.palette_query, style::BODY)
        .key("palette-input")
        .label("Quick jump search")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(p.text)
        .caret(p.text, p.selection)
        .placeholder("Jump to a project, action, or past update…", p.text_3)
        .on_edit(|s: &mut State| {
            s.palette_index = 0;
            &mut s.palette_query
        })
        .on_submit(|s: &mut State, _tx: &mut Tx| run_palette(s))
        .on_input_key(|s: &mut State, e: &KeyEvent, _tx: &mut Tx| match e.key {
            Key::Escape => {
                s.close_palette();
                true
            }
            Key::Up => {
                move_palette(s, -1);
                true
            }
            Key::Down => {
                move_palette(s, 1);
                true
            }
            _ => false,
        });

    let item_count = items.len();
    let mut list = col().w_full().gap(2.0).padding(8.0);
    if items.is_empty() {
        list = list.child(text("No matching projects, actions, or updates").font(style::META).color(p.text_3).padding(12.0));
    } else {
        for (i, item) in items.into_iter().enumerate() {
            let active = i == selected;
            let action = item.action.clone();
            let mut row_el = row()
                .key(format!("pal-{i}"))
                .role("button")
                .label(item.title.clone())
                .w_full()
                .px(10.0)
                .py(8.0)
                .gap(10.0)
                .center_y()
                .radius(8.0)
                .fill(if active { p.hover } else { p.card.alpha(0.0) })
                .hover(p.hover)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| execute_palette_action(s, action.clone()));

            if let Some(pid) = item.project_id
                && let Some(sum) = s.summary(pid)
            {
                let sphere = orb::orb(&sum.project.orb, 20.0);
                row_el = row_el.child(if sum.project.archived { sphere.opacity(0.6) } else { sphere });
            } else {
                row_el = row_el.child(el().size(20.0, 20.0).center().child(el().size(5.0, 5.0).radius(2.5).fill(p.text_3)));
            }

            if item.unseen
                && let Some(pid) = item.project_id
                && let Some(sum) = s.summary(pid)
            {
                row_el = row_el.child(el().size(6.0, 6.0).radius(3.0).fill(orb::accent(&sum.project.orb)));
            }

            row_el = row_el.child(text(item.title).font(style::NAME).color(p.text).truncate().shrink()).child(rune::spacer());

            if let Some(badge) = item.badge {
                let badge_color = match badge.as_str() {
                    "Needs you" => p.amber,
                    "Working" => p.live,
                    _ => p.text_3,
                };
                row_el = row_el.child(
                    el().px(6.0)
                        .py(1.5)
                        .radius(5.0)
                        .border(1.0, if badge == "Needs you" || badge == "Working" { badge_color.alpha(0.4) } else { p.hairline })
                        .child(text(badge).font(style::META).color(badge_color).truncate().max_w(110.0)),
                );
            }
            row_el = row_el.child(text(item.subtitle).font(style::META).color(p.text_3).truncate());
            list = list.child(row_el);
        }
    }

    let list_h = if item_count == 0 { 56.0 } else { (14.0 + item_count as f32 * 38.0).min(380.0) };

    let panel = col()
        .key("palette-panel")
        .role("dialog")
        .label("Quick Jump")
        .w(540.0)
        .radius(14.0)
        .fill(p.card)
        .border(1.0, p.hairline)
        .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.55 } else { 0.20 }))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .modal(true, |s: &mut State| s.close_palette())
        .child(row().w_full().px(14.0).py(12.0).gap(10.0).center_y().child(input).child(text("⌘K").font(style::META).color(p.text_3)))
        .child(rune::rule(1.0, p.hairline))
        .child(col().w_full().h(list_h).child(list).scroll(ScrollSpec { stick_bottom: false }));

    Some(
        col()
            .key("palette-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pt(96.0)
            .items_center()
            .fill(p.scrim)
            .transition(Transition::opacity())
            .child(panel),
    )
}

/// Bottom-right toast stack for archive undo, clipboard image confirmation, and store errors.
pub fn toasts(s: &State, k: &Kit) -> Option<El> {
    let entries = s.toasts.entries();
    if entries.is_empty() {
        return None;
    }
    let mut region = ToastRegion::new();
    for entry in entries.iter().rev().take(3).rev() {
        let tid = entry.id;
        let mut t = Toast::new(format!("toast-{tid}"), entry.options.title.clone());
        if let Some(desc) = &entry.options.description {
            t = t.description(desc.clone());
        }
        match entry.options.kind {
            ToastKind::UndoArchive { project_id, was_archived } => {
                t = t.icon(k, k.glyph.check).action(k, "Undo", true, move |s: &mut State, _tx: &mut Tx| {
                    s.dismiss_toast(tid);
                    s.set_archived_inner(project_id, was_archived, false);
                });
            }
            ToastKind::UndoDismiss { update_id } => {
                t = t.icon(k, k.glyph.check).action(k, "Undo", true, move |s: &mut State, _tx: &mut Tx| {
                    s.dismiss_toast(tid);
                    s.undismiss_attention(update_id);
                });
            }
            ToastKind::Error => {
                t = t.icon(k, k.glyph.circle_x);
            }
            ToastKind::Info => {
                t = t.icon(k, k.glyph.check);
            }
        }
        t = t.on_close(move |s: &mut State| s.dismiss_toast(tid));
        region = region.toast(t.build(k));
    }
    Some(region.build(k))
}

/// Confirmation dialog before deleting a single update card.
pub fn delete_confirm(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    let uid = s.delete_confirm?;
    let panel = col()
        .key("delete-confirm")
        .role("dialog")
        .label("Delete update")
        .w(380.0)
        .padding(22.0)
        .gap(14.0)
        .radius(14.0)
        .fill(p.card)
        .border(1.0, p.hairline)
        .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.5 } else { 0.18 }))
        .transition(Transition::scale(0.96).combined(Transition::opacity()))
        .modal(true, |s: &mut State| s.delete_confirm = None)
        .child(text("Delete this update?").font(style::HEADING).color(p.text))
        .child(text("This deletes the update and its attached media. You can't undo it.").font(style::META).color(p.text_3).wrap())
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(rune::spacer())
                .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.delete_confirm = None).build(k))
                .child(Button::new("Delete").variant(ButtonVariant::Danger).on_press(move |s: &mut State| s.delete_update(uid)).build(k)),
        );

    Some(
        el().key("delete-scrim").absolute(0.0, 0.0).w_full().h_full().fill(p.scrim).center().transition(Transition::opacity()).child(panel),
    )
}

/// The Settings sheet over a dimmed window, or nothing while it's closed.
pub fn settings(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    if !s.settings_open {
        return None;
    }
    let st = s.settings;
    let theme = SegmentedControl::new(&["System", "Light", "Dark"], Some(st.theme as usize))
        .label("Theme")
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            s.settings.theme = [Theme::System, Theme::Light, Theme::Dark][i];
            s.save_settings();
        })
        .build(k);
    let size = SegmentedControl::new(&["Small", "Medium", "Large"], Some(st.orb_size as usize))
        .label("Orb size")
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            s.settings.orb_size = [OrbSize::Small, OrbSize::Medium, OrbSize::Large][i];
            s.save_settings();
        })
        .build(k);
    let order = SegmentedControl::new(&["Recent", "Name"], Some(st.order as usize))
        .label("Order")
        .on_pick(|s: &mut State, i: usize, _: &mut Tx| {
            s.settings.order = [Order::Recent, Order::Name][i];
            s.save_settings();
        })
        .build(k);
    let folders = Switch::new(st.show_folders)
        .label("Show folder names")
        .hide_label()
        .on_toggle(|s: &mut State| {
            s.settings.show_folders = !s.settings.show_folders;
            s.save_settings();
        })
        .build(k);
    let notify = Switch::new(st.notify_updates)
        .label("Notify on new updates")
        .hide_label()
        .on_toggle(|s: &mut State| {
            s.settings.notify_updates = !s.settings.notify_updates;
            s.save_settings();
        })
        .build(k);
    let menubar = Switch::new(st.menubar)
        .label("Menu bar item")
        .hide_label()
        .on_toggle(|s: &mut State| {
            s.settings.menubar = !s.settings.menubar;
            s.save_settings();
            s.sync_menubar();
        })
        .build(k);

    let store = style::home_relative(&feeder_core::data_dir());

    let storage = row()
        .w_full()
        .gap(16.0)
        .center_y()
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(text(store).font(style::PATH).color(p.text_2).truncate().selectable(true))
                .child(text("Database, screenshots, and settings").font(style::META).color(p.text_3)),
        )
        .child(
            bar_button("reveal", "Show in Finder", p)
                .border(1.0, p.hairline)
                .on_press(|_: &mut State| reveal_path(&feeder_core::data_dir())),
        );

    let body = col()
        .w_full()
        .px(24.0)
        .pt(4.0)
        .pb(24.0)
        .gap(24.0)
        .child(section(p, "Appearance", vec![setting(p, "Theme", None, theme), setting(p, "Orb size", None, size)]))
        .child(section(
            p,
            "Projects and updates",
            vec![
                setting(p, "Order", None, order),
                setting(p, "Show folder names", Some("Under projects you renamed"), folders),
                setting(p, "New update banners", Some("A macOS notification when an agent posts"), notify),
                setting(p, "Menu bar item", Some("Shows working agents and projects that need you"), menubar),
            ],
        ))
        .child(agents_section(s, p, k))
        .child(section(p, "Storage", vec![storage]))
        .child(uninstall_section(s, p, k))
        .child(text(format!("Feeder {}", env!("CARGO_PKG_VERSION"))).font(style::META).color(p.text_3).self_center());

    // In a window shorter than the sheet, the sheet shrinks to the scrim's padding and the
    // sections scroll under a fixed title row.
    let panel = col()
        .key("settings")
        .role("dialog")
        .label("Settings")
        .w(480.0)
        .max_h(720.0)
        .shrink()
        .min_h(0.0)
        .radius(16.0)
        .fill(p.card)
        .border(1.0, p.hairline)
        .clip_rounded(16.0)
        .shadow(18.0, 60.0, oklch(0.0, 0.0, 0.0).alpha(if p.dark { 0.5 } else { 0.18 }))
        .transition(Transition::scale(0.96).combined(Transition::opacity()))
        .modal(true, |s: &mut State| s.settings_open = false)
        .child(
            row()
                .w_full()
                .px(24.0)
                .pt(20.0)
                .pb(16.0)
                .center_y()
                .child(text("Settings").font(style::HEADING).color(p.text))
                .child(rune::spacer())
                .child(Button::new("Done").variant(ButtonVariant::Ghost).on_press(|s: &mut State| s.settings_open = false).build(k)),
        )
        .child(
            col()
                .key("settings-body")
                .w_full()
                .shrink()
                .min_h(0.0)
                .child(body)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(12.0),
        );

    Some(
        col()
            .key("settings-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(p.scrim)
            .center()
            .py(24.0)
            .transition(Transition::opacity())
            .child(panel),
    )
}

/// Opens Settings scrolled to Agents, for the places that are about connecting a tool: the empty
/// Home and "Connect Agents…". Agents sits below Appearance and Projects, out of first view.
pub fn open_agent_setup(s: &mut State, tx: &mut Tx) {
    s.settings_open = true;
    tx.scroll_key_to(AGENTS_SECTION_KEY, 0.0);
}

const AGENTS_SECTION_KEY: &str = "settings-agents";

/// Settings → Agents: a row per installed tool with its own connect control, then a config any
/// other MCP tool can read. Nothing connects until it's pressed.
fn agents_section(s: &State, p: &Palette, k: &Kit) -> El {
    let note = |t: &str| text(t.to_string()).font(style::META).color(p.text_3).wrap();
    let mut rows = vec![note(
        "Connecting a tool adds Feeder's MCP server and two skills to its settings. Agents in that tool can then post updates and use the notebook.",
    )];
    match &s.agents.rows {
        None => rows.push(note("Looking for tools on this Mac…")),
        Some(found) if found.is_empty() => {
            rows.push(note("None of the tools Feeder sets up are installed. Paste the MCP config below into your tool instead."))
        }
        Some(found) => rows.extend(found.iter().map(|(tool, link)| agent_row(s, p, k, *tool, link))),
    }
    let copy_label = if s.is_copied(CopyTarget::McpConfig) { "Copied" } else { "Copy Config" };
    rows.push(
        row()
            .w_full()
            .gap(12.0)
            .center_y()
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(2.0)
                    .child(text("Another tool").font(style::NAME).color(p.text))
                    .child(text("Paste this MCP config into its settings").font(style::META).color(p.text_3).truncate()),
            )
            .child(bar_button("copy-mcp", copy_label, p).label("Copy MCP config").border(1.0, p.hairline).on_press_tx(
                |s: &mut State, tx: &mut Tx| {
                    tx.copy(setup::mcp_json(&setup::feeder_binary()));
                    s.mark_copied(CopyTarget::McpConfig);
                },
            )),
    );
    rows.push(note("Restart a tool after connecting or disconnecting it. Then ask an agent in any project to post a Feeder update."));
    section(p, "Agents", rows).key(AGENTS_SECTION_KEY)
}

/// One installed tool: its mark and name, where it stands under the name, and the one action
/// that changes that on the right.
fn agent_row(s: &State, p: &Palette, k: &Kit, tool: Tool, link: &Link) -> El {
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
            .key(format!("agent-action-{}", tool.id()))
    };
    let busy = |t: &str| text(t.to_string()).font(style::META).color(p.text_3);
    let control = match link {
        Link::Connected => action("Disconnect", ButtonVariant::GhostMuted, false),
        Link::Missing => action("Connect", ButtonVariant::Outline, true),
        Link::Outdated => action("Update", ButtonVariant::Outline, true),
        Link::Failed(_) => action("Try again", ButtonVariant::Outline, true),
        Link::Connecting => busy("Connecting…"),
        Link::Disconnecting => busy("Disconnecting…"),
    };
    let status = |t: String, color| text(t).font(style::META).color(color).truncate();
    let state = match link {
        Link::Connected => {
            Some(row().gap(6.0).center_y().child(el().size(6.0, 6.0).radius(3.0).fill(p.live)).child(status("Connected".into(), p.text_2)))
        }
        Link::Outdated => Some(row().child(status("Set up for another copy of Feeder".into(), p.text_3))),
        Link::Failed(why) => Some(row().min_w(0.0).child(status(format!("Couldn't change it: {why}"), k.c.danger.fg).help(why.clone()))),
        _ => None,
    };
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(text(tool.name()).font(style::NAME).color(p.text).truncate());
    if let Some(state) = state {
        words = words.child(state);
    }
    row()
        .key(format!("agent-{}", tool.id()))
        .w_full()
        .min_h(36.0)
        .gap(12.0)
        .center_y()
        // One box for every mark, so logos and app icons leave the names on one edge.
        .child(el().size(20.0, 20.0).center().child(agents::mark(tool.name(), p, 18.0, s.agent_icon(tool.name()))))
        .child(words)
        .child(control)
}

/// Settings → Uninstall: undoes every change Feeder made outside its own folder, so moving it to
/// the Trash leaves nothing behind but the data the person chooses to keep.
fn uninstall_section(s: &State, p: &Palette, k: &Kit) -> El {
    let note = |t: &str| text(t.to_string()).font(style::META).color(p.text_3).wrap();
    let any_connected = s.agents.rows.iter().flatten().any(|(_, l)| matches!(l, Link::Connected | Link::Outdated | Link::Disconnecting));
    let mut rows = vec![note(
        "Removing Feeder from your tools deletes its MCP server and skills from their settings. Each file keeps a .feeder-backup copy.",
    )];
    if s.agents.uninstalled && !any_connected {
        rows.push(note("Feeder is out of your tools. Quit Feeder and move it to the Trash. To delete your projects and notebook too, delete the data folder above."));
    } else {
        rows.push(
            Button::new("Remove Feeder from All Tools")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(|s: &mut State| {
                    let wake = s.wake.clone();
                    s.agents.disconnect_all(wake.as_ref());
                })
                .build(k)
                .key("uninstall-tools")
                .self_start(),
        );
    }
    section(p, "Uninstall", rows)
}

fn section(p: &Palette, title: &str, rows: Vec<El>) -> El {
    let mut c = col().w_full().gap(12.0).child(text(title.to_uppercase()).font(style::LABEL).color(p.text_3));
    for r in rows {
        c = c.child(r);
    }
    c
}

fn setting(p: &Palette, label: &str, hint: Option<&str>, control: El) -> El {
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(text(label.to_string()).font(style::NAME).color(p.text).truncate());
    if let Some(h) = hint {
        words = words.child(text(h.to_string()).font(style::META).color(p.text_3));
    }
    row().w_full().gap(16.0).center_y().child(words).child(control)
}
