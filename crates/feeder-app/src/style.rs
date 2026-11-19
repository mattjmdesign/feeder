//! Feeder's two appearances and its type. The orbs carry the colour; everything else stays quiet.

use rune::tok::TextStyle;
use rune::{Color, oklch};
use rune_kit::{Kit, PaletteSeed};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color,
    pub card: Color,
    pub hover: Color,
    pub hairline: Color,
    pub text: Color,
    pub text_2: Color,
    pub text_3: Color,
    pub scrim: Color,
    pub selection: Color,
    pub amber: Color,
    pub live: Color,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            bg: oklch(0.135, 0.002, 260.0),
            card: oklch(0.18, 0.003, 260.0),
            hover: oklch(1.0, 0.0, 0.0).alpha(0.06),
            hairline: oklch(1.0, 0.0, 0.0).alpha(0.09),
            text: oklch(0.955, 0.0, 0.0),
            text_2: oklch(0.74, 0.004, 260.0),
            // 0.60 holds 4.5:1 for 12 pt metadata on both the card (4.76) and the frame (5.07).
            text_3: oklch(0.60, 0.005, 260.0),
            scrim: oklch(0.0, 0.0, 0.0).alpha(0.50),
            selection: oklch(0.65, 0.14, 255.0).alpha(0.32),
            amber: oklch(0.78, 0.14, 72.0),
            live: oklch(0.74, 0.15, 152.0),
        }
    } else {
        Palette {
            dark,
            bg: oklch(0.968, 0.002, 260.0),
            card: oklch(1.0, 0.0, 0.0),
            hover: oklch(0.0, 0.0, 0.0).alpha(0.045),
            hairline: oklch(0.0, 0.0, 0.0).alpha(0.085),
            text: oklch(0.18, 0.004, 260.0),
            text_2: oklch(0.42, 0.005, 260.0),
            // 0.54 holds 4.5:1 for 12 pt metadata on both the card (5.06) and the frame (4.61).
            text_3: oklch(0.54, 0.005, 260.0),
            scrim: oklch(0.0, 0.0, 0.0).alpha(0.20),
            selection: oklch(0.60, 0.16, 255.0).alpha(0.24),
            amber: oklch(0.60, 0.16, 68.0),
            live: oklch(0.54, 0.16, 152.0),
        }
    }
}

/// rune-kit's controls (menus, switches, segmented controls, Markdown) in this palette.
pub fn kit(p: &Palette) -> Kit {
    let mut k = Kit::from_palette(PaletteSeed { bg: p.card, fg: p.text, accent: oklch(0.65, 0.14, 255.0) });
    k.c.bg_deep = p.bg;
    // rune-kit's 11 pt captions join the app's 12 pt step, so its controls stay on the scale.
    let t = &mut k.ty;
    t.small = TextStyle::new(12.0, t.small.weight).lh(16.0);
    t.small_medium = TextStyle::new(12.0, t.small_medium.weight).lh(16.0);
    k
}

/// How far the Notebook's main pane sits inside the window's edge.
pub const PANE_INSET: f32 = 8.0;

/// The kit for a pane inset [`PANE_INSET`] from the window, with its corner radius set so the
/// pane's bottom corners run parallel to the window's. macOS draws a 16 pt *continuous* window
/// corner, which starts curving earlier than a circular arc; the arithmetic 16 − 8 = 8 pt looks
/// pinched next to it, so the pane takes a slightly wider circular radius.
pub fn pane_kit(k: &Kit) -> Kit {
    let mut k = k.clone();
    k.radius.lg = 12.0;
    k
}

// Type scale. One family, the macOS system font (SF Pro, behind the sans slot in `main`), five
// sizes (24, 16, 14, 13, 12) and nothing heavier than medium (500): hierarchy comes from size
// and colour, not weight. Each role fixes size, line height and weight together so a role is
// one decision. SF's own `trak` table tracks every size as AppKit does, so only uppercase
// overlines add any; figures that change (times, counts) are tabular so they don't shift.

/// SF Pro, behind `Family::Sans`, and SF Mono, behind `Family::Mono`.
pub const FONT_FAMILIES: (&str, &str) = ("System Font", "ui-monospace");
const TNUM: &str = "'tnum' 1";

/// The heaviest weight anywhere in the app: SF Pro Medium.
pub const MEDIUM: f32 = 500.0;

/// 24: page-level headlines and a project's or a page's own title.
pub const DISPLAY: TextStyle = TextStyle::new(24.0, MEDIUM).lh(30.0);
pub const TITLE: TextStyle = DISPLAY;
/// 16: titles inside a list (notebook entries), dialog and panel headings, Markdown headings.
pub const SECTION_TITLE: TextStyle = TextStyle::new(16.0, MEDIUM).lh(22.0);
pub const HEADING: TextStyle = SECTION_TITLE;
/// 14: names (projects, people, row titles) and all reading copy.
pub const NAME: TextStyle = TextStyle::new(14.0, MEDIUM).lh(19.0);
pub const BODY: TextStyle = TextStyle::new(14.0, 400.0).lh(22.0);
/// Long-form reading (notebook pages, briefs): the body size, looser.
pub const PROSE: TextStyle = TextStyle::new(14.0, 400.0).lh(23.0);
/// 13: controls and navigation: buttons, rail rows, segmented labels.
pub const CONTROL: TextStyle = TextStyle::new(13.0, MEDIUM).lh(18.0);
/// 12: secondary detail: times, counts, bylines. Tabular so changing figures hold their width.
pub const META: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0).features(TNUM);
/// Uppercase overlines and section labels.
pub const LABEL: TextStyle = TextStyle::new(12.0, MEDIUM).lh(16.0).tracking(0.3);
pub const AGENT: TextStyle = TextStyle::new(12.0, MEDIUM).lh(16.0);
/// Folders and branches, in the interface face.
pub const PATH: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0);

/// `/Users/me/code/x` as `~/code/x`.
pub fn home_relative(path: &std::path::Path) -> String {
    let full = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => format!("~{}", &full[home.len()..]),
        _ => full,
    }
}
