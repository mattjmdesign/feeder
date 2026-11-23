use std::ffi::{CStr, c_char};
use std::sync::OnceLock;

use rune::{El, Icon, ScrollSpec, Waker, col, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Markdown, Progress, ProgressCircle, popover::raised, scrim,
};

use crate::{State, ToastItem, ToastKind, feed, style};

static WAKE: OnceLock<Waker> = OnceLock::new();

#[repr(C)]
struct NativeSnapshot {
    phase: u32,
    progress: f64,
    version: *const c_char,
    notes: *const c_char,
    message: *const c_char,
    reveal: bool,
}

unsafe extern "C" {
    fn feeder_start_updater(wake: extern "C" fn());
    fn feeder_update_snapshot() -> NativeSnapshot;
    fn feeder_update_advance();
}

pub fn bind(wake: Waker) {
    let _ = WAKE.set(wake);
}

pub fn start() {
    unsafe { feeder_start_updater(wake) }
}

extern "C" fn wake() {
    let _ = std::panic::catch_unwind(|| {
        if let Some(waker) = WAKE.get() {
            waker();
        }
    });
}

#[repr(u32)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle = 0,
    Checking = 1,
    Available = 2,
    Downloading = 3,
    Preparing = 4,
    Ready = 5,
    Installing = 6,
    Current = 7,
    Problem = 8,
    Informational = 9,
}

impl TryFrom<u32> for Phase {
    type Error = u32;

    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        match raw {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Checking),
            2 => Ok(Self::Available),
            3 => Ok(Self::Downloading),
            4 => Ok(Self::Preparing),
            5 => Ok(Self::Ready),
            6 => Ok(Self::Installing),
            7 => Ok(Self::Current),
            8 => Ok(Self::Problem),
            9 => Ok(Self::Informational),
            other => Err(other),
        }
    }
}

impl Phase {
    fn status(self) -> Option<&'static str> {
        match self {
            Self::Available => Some("Feeder update available"),
            Self::Downloading => Some("Downloading update…"),
            Self::Preparing => Some("Preparing update…"),
            Self::Ready => Some("Update ready to install"),
            Self::Installing => Some("Installing update…"),
            _ => None,
        }
    }
}

#[derive(Default, PartialEq)]
struct Snapshot {
    phase: Phase,
    progress: Option<f32>,
    version: String,
    notes: String,
    message: String,
}

#[derive(Default)]
pub struct Updates {
    snapshot: Snapshot,
    requested: bool,
    pub open: bool,
}

pub fn poll(s: &mut State) -> bool {
    let (native, [version, notes, message]) = unsafe {
        let native = feeder_update_snapshot();
        let copy = |s: *const c_char| if s.is_null() { String::new() } else { CStr::from_ptr(s).to_string_lossy().into_owned() };
        let strings = [copy(native.version), copy(native.notes), copy(native.message)];
        (native, strings)
    };
    let phase = Phase::try_from(native.phase).unwrap_or(Phase::Idle);
    let snapshot =
        Snapshot { phase, progress: (0.0..=1.0).contains(&native.progress).then_some(native.progress as f32), version, notes, message };
    let changed = snapshot != s.updates.snapshot || native.reveal;
    let was_active = s.updates.snapshot.phase.status().is_some();
    s.updates.requested |= native.reveal;
    if phase.status().is_some() {
        s.updates.open |= std::mem::take(&mut s.updates.requested);
    } else {
        s.updates.open = false;
        if changed
            && matches!(phase, Phase::Current | Phase::Problem | Phase::Informational)
            && (std::mem::take(&mut s.updates.requested) || was_active)
        {
            let (title, kind) = match phase {
                Phase::Problem => ("Couldn't finish updating Feeder", ToastKind::Error),
                Phase::Informational => ("A new Feeder release is out", ToastKind::Info),
                _ => ("Feeder is up to date", ToastKind::Info),
            };
            let title = if phase == Phase::Current && !snapshot.message.is_empty() { snapshot.message.clone() } else { title.to_owned() };
            let description = (phase != Phase::Current && !snapshot.message.is_empty()).then(|| snapshot.message.clone());
            s.push_toast("app-update", ToastItem { title, description, kind }, Some(5.0));
        }
    }
    s.updates.snapshot = snapshot;
    changed
}

static UPDATE_ICON: Icon = Icon {
    name: "feeder-upgrade",
    view: 20.0,
    body: r#"<circle cx="10" cy="10" r="7.1" fill="none" stroke="currentColor" stroke-width="1.35"/><path fill="none" stroke="currentColor" stroke-width="1.45" stroke-linecap="round" stroke-linejoin="round" d="M10 13.5v-7m-2.5 2.5L10 6.5l2.5 2.5"/>"#,
};

pub fn view(s: &State, p: &style::Palette, k: &Kit, layer: &mut Vec<El>) {
    let update = &s.updates.snapshot;
    let Some(status) = update.phase.status() else { return };
    let mut accent = k.clone();
    accent.c.text_base = k.c.icon_accent;
    accent.c.icon_muted = k.c.icon_accent;
    accent.c.bg_button_neutral = k.c.bg_accent.alpha(0.14);
    accent.c.border_active = k.c.icon_accent;
    let toggle = |s: &mut State| s.updates.open = !s.updates.open;
    let trigger = if let Some(progress) = update.progress
        && matches!(update.phase, Phase::Downloading | Phase::Preparing)
    {
        let kit = accent.clone();
        Button::new("Feeder Updates")
            .control_size(ControlSize::Large)
            .help(format!("{status} {}%", (progress * 100.0).round()))
            .label_with(move |_, _| ProgressCircle::new(progress).build(&kit).a11y_hidden())
            .on_press(toggle)
            .build(&accent)
            .size(28.0, 28.0)
            .px(0.0)
    } else {
        let icon = if update.phase == Phase::Ready { k.glyph.check } else { &UPDATE_ICON };
        IconButton::new(icon)
            .variant(IconButtonVariant::Neutral)
            .control_size(ControlSize::Large)
            .label("Feeder Updates")
            .help(status)
            .on_press(toggle)
            .build(&accent)
    }
    .key("feeder-update-button")
    .expanded(s.updates.open);
    let dock = col().key("feeder-updates").pin(None, None, Some(18.0), Some(16.0)).items_end().gap(8.0);
    if !s.updates.open {
        layer.push(dock.child(trigger));
        return;
    }

    let title = if update.version.is_empty() { "Feeder updates".to_owned() } else { format!("Feeder {}", update.version) };
    let close = IconButton::new(k.glyph.close)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("Close update details")
        .on_press_tx(|s: &mut State, tx| {
            s.updates.open = false;
            tx.focus_when_shown("feeder-update-button", false);
        })
        .build(k);
    let header =
        row().w_full().center_y().justify_between().gap(8.0).child(text(title.clone()).font(style::HEADING).color(p.text)).child(close);
    let mut content = col()
        .key("feeder-upgrade-content")
        .role("dialog")
        .label(title)
        .focus_return("feeder-update-button")
        .w(304.0)
        .padding(12.0)
        .gap(10.0)
        .child(header);
    if !update.notes.is_empty() && matches!(update.phase, Phase::Available | Phase::Ready) {
        content = content.child(
            col()
                .key("update-notes")
                .w_full()
                .max_h(164.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(Markdown::new(update.notes.clone()).render(&feed::markdown_style(p, k).body_font(style::CONTROL))),
        );
    }
    if !update.message.is_empty() {
        content = content.child(text(update.message.clone()).font(style::CONTROL).color(k.c.std_text));
    }
    if matches!(update.phase, Phase::Downloading | Phase::Preparing | Phase::Installing) {
        let progress = if matches!(update.phase, Phase::Downloading | Phase::Preparing) { update.progress } else { None };
        content = content.child(Progress::new(progress).label(status).show_value().build(&accent));
    }
    let action = match update.phase {
        Phase::Available => Some("Download"),
        Phase::Ready => Some("Restart to Update"),
        _ => None,
    };
    if let Some(label) = action {
        content = content.child(
            row().w_full().justify_end().child(
                Button::new(label)
                    .variant(ButtonVariant::Contrast)
                    .on_press(|_: &mut State| {
                        unsafe { feeder_update_advance() };
                    })
                    .build(k),
            ),
        );
    }
    layer.push(scrim("feeder-update-dismiss", |s: &mut State| s.updates.open = false));
    layer.push(dock.child(raised(k, content, k.radius.lg)).child(trigger));
}
