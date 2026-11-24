use rune::tok::TextStyle;
use rune::{Color, oklch};
use rune_kit::{Kit, PaletteSeed};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color,
    pub rail: Color,
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
            rail: oklch(0.105, 0.002, 260.0),
            card: oklch(0.18, 0.003, 260.0),
            hover: oklch(1.0, 0.0, 0.0).alpha(0.06),
            hairline: oklch(1.0, 0.0, 0.0).alpha(0.09),
            text: oklch(0.955, 0.0, 0.0),
            text_2: oklch(0.74, 0.004, 260.0),
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
            rail: oklch(0.940, 0.003, 260.0),
            card: oklch(1.0, 0.0, 0.0),
            hover: oklch(0.0, 0.0, 0.0).alpha(0.045),
            hairline: oklch(0.0, 0.0, 0.0).alpha(0.085),
            text: oklch(0.18, 0.004, 260.0),
            text_2: oklch(0.42, 0.005, 260.0),
            text_3: oklch(0.54, 0.005, 260.0),
            scrim: oklch(0.0, 0.0, 0.0).alpha(0.20),
            selection: oklch(0.60, 0.16, 255.0).alpha(0.24),
            amber: oklch(0.60, 0.16, 68.0),
            live: oklch(0.54, 0.16, 152.0),
        }
    }
}

pub fn kit(p: &Palette) -> Kit {
    let mut k = Kit::from_palette(PaletteSeed { bg: p.card, fg: p.text, accent: oklch(0.65, 0.14, 255.0) });
    k.c.bg_deep = p.bg;
    if !p.dark {
        k.c.knob = p.card;
        k.c.knob_on = p.card;
    }
    let t = &mut k.ty;
    t.small = TextStyle::new(12.0, t.small.weight).lh(16.0);
    t.small_medium = TextStyle::new(12.0, t.small_medium.weight).lh(16.0);
    k
}

pub const PANE_INSET: f32 = 8.0;

pub fn pane_kit(k: &Kit, p: &Palette) -> Kit {
    let mut k = k.clone();
    k.radius.lg = 12.0;
    k.c.bg_deep = p.rail;
    k.c.border_muted = p.hairline;
    k
}

pub fn open_pane_kit(k: &Kit, p: &Palette) -> Kit {
    let mut k = pane_kit(k, p);
    k.c.bg_base = if p.dark { oklch(0.148, 0.002, 260.0) } else { p.bg };
    k.c.border_muted = if p.dark { oklch(1.0, 0.0, 0.0).alpha(0.075) } else { oklch(0.0, 0.0, 0.0).alpha(0.075) };
    k
}

pub const FONT_FAMILIES: (&str, &str) = ("System Font", "ui-monospace");
const TNUM: &str = "'tnum' 1";

pub const MEDIUM: f32 = 500.0;

pub const DISPLAY: TextStyle = TextStyle::new(24.0, MEDIUM).lh(30.0);
pub const TITLE: TextStyle = DISPLAY;
pub const SECTION_TITLE: TextStyle = TextStyle::new(16.0, MEDIUM).lh(22.0);
pub const HEADING: TextStyle = SECTION_TITLE;
pub const NAME: TextStyle = TextStyle::new(14.0, MEDIUM).lh(19.0);
pub const BODY: TextStyle = TextStyle::new(14.0, 400.0).lh(22.0);
pub const PROSE: TextStyle = TextStyle::new(14.0, 400.0).lh(23.0);
pub const CONTROL: TextStyle = TextStyle::new(13.0, MEDIUM).lh(18.0);
pub const META: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0).features(TNUM);
pub const LABEL: TextStyle = TextStyle::new(12.0, MEDIUM).lh(16.0).tracking(0.3);
pub const AGENT: TextStyle = TextStyle::new(12.0, MEDIUM).lh(16.0);
pub const PATH: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0);

pub fn home_relative(path: &std::path::Path) -> String {
    let full = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => format!("~{}", &full[home.len()..]),
        _ => full,
    }
}
