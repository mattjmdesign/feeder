//! Each agent tool shown by its mark: a known tool's logo (T3 Code's bundled app icon among them),
//! else the Finder icon of an installed app the agent is named after, else a monogram. Hovering a
//! mark names the agent.

use std::path::PathBuf;

use rune::tok::TextStyle;
use rune::{Color, El, Icon, Image, el, icon, oklch, text};

use crate::logos;
use crate::style::Palette;

/// The logo and colour for an agent's display name, when it's a tool we know.
fn known(agent: &str, p: &Palette) -> Option<(&'static Icon, Color)> {
    let a = agent.to_ascii_lowercase();
    let ink = p.text;
    let mark = if a.contains("claude") {
        (&logos::CLAUDE, oklch(0.68, 0.13, 42.0)) // Claude's terracotta
    } else if a.contains("codex") || a.contains("openai") || a.contains("chatgpt") {
        (&logos::OPENAI, ink)
    } else if a.contains("opencode") {
        (&logos::OPENCODE, ink)
    } else if a.contains("cursor") {
        (&logos::CURSOR, ink)
    } else if a.contains("gemini") {
        (&logos::GEMINI, oklch(0.66, 0.14, 285.0))
    } else if a.contains("copilot") {
        (&logos::COPILOT, ink)
    } else if a.contains("windsurf") {
        (&logos::WINDSURF, ink)
    } else if a.contains("antigravity") || a == "agy" {
        (&logos::ANTIGRAVITY, ink)
    } else if a.contains("zed") {
        (&logos::ZED, ink)
    } else {
        return None;
    };
    Some(mark)
}

/// The app an agent without a logo of its own is named after, where apps install: a tool Feeder
/// sets up by its known bundle names (T3 Code ships as "T3 Code (Beta)"), else "<agent>.app".
/// Only paths; the watcher checks which exist, off the UI thread.
pub fn app_candidates(agent: &str) -> Vec<PathBuf> {
    let name = agent.trim();
    if name.is_empty() || name.contains(['/', '.']) {
        return Vec::new();
    }
    let generic = format!("{name}.app");
    let names: Vec<&str> = match feeder_core::setup::Tool::parse(name) {
        Some(tool) if !tool.app_names().is_empty() => tool.app_names().to_vec(),
        _ => vec![generic.as_str()],
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    names.iter().flat_map(|n| [PathBuf::from("/Applications").join(n), home.join("Applications").join(n)]).collect()
}

/// Whether the agent's mark ships with Feeder, so no app icon is needed.
pub fn has_logo(agent: &str) -> bool {
    known(agent, &crate::style::palette(true)).is_some()
}

/// The agent's mark at `size` points, named for screen readers and on hover. `app` is its app's
/// icon, once loaded.
pub fn mark(agent: &str, p: &Palette, size: f32, app: Option<&Image>) -> El {
    let named = |e: El| e.label(agent.to_string()).help(agent.to_string());
    if let Some((logo, color)) = known(agent, p) {
        return named(icon(logo, size).color(color));
    }
    if let Some(img) = app.filter(|i| i.size().is_some()) {
        // App icons carry their own margin; a touch larger sits level with the logos.
        let s = size * 1.2;
        return named(el().size(s, s).pixels(img.pixels()));
    }
    let initial = agent.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
    named(
        el().size(size, size)
            .radius(size / 2.0)
            .border(1.0, p.text_3)
            .center()
            .child(text(initial).font(TextStyle::new(size * 0.55, crate::style::MEDIUM)).color(p.text_2)),
    )
}
