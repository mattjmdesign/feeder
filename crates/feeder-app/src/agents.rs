use std::path::PathBuf;

use rune::tok::TextStyle;
use rune::{Color, El, Icon, Image, el, icon, oklch, text};

use crate::logos;
use crate::style::Palette;

fn known(agent: &str, p: &Palette) -> Option<(&'static Icon, Color)> {
    let a = agent.to_ascii_lowercase();
    let ink = p.text;
    let mark = if a.contains("claude") {
        (&logos::CLAUDE, oklch(0.68, 0.13, 42.0))
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

pub fn post_mark(s: &crate::State, agent: &str, via: Option<&str>, p: &Palette, size: f32) -> El {
    let own = has_logo(agent) || s.agent_icon(agent).is_some_and(|i| i.size().is_some());
    match via.filter(|_| !own) {
        Some(host) => {
            let who = format!("{agent} via {host}");
            mark(host, p, size, s.agent_icon(host)).label(who.clone()).help(who)
        }
        None => mark(agent, p, size, s.agent_icon(agent)),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ModelService<'a> {
    Branded(&'static str),
    Unbranded(&'a str),
}

fn model_service(model: &str) -> Option<ModelService<'_>> {
    use ModelService::{Branded, Unbranded};

    if let Some((provider, name)) = model.split_once('/') {
        if provider.is_empty() || name.is_empty() {
            return None;
        }
        return Some(match provider.to_ascii_lowercase().as_str() {
            "openai" => Branded("openai"),
            "anthropic" => Branded("claude"),
            "google" | "gemini" | "vertex" | "google-vertex" => Branded("gemini"),
            "github-copilot" | "copilot" => Branded("copilot"),
            "opencode" | "opencode-go" | "opencode-zen" => Branded("opencode"),
            _ => Unbranded(provider),
        });
    }

    let family = model.split('-').next()?.to_ascii_lowercase();
    let brand = match family.as_str() {
        "claude" | "opus" | "sonnet" | "haiku" | "fable" => "claude",
        "gpt" | "codex" | "o1" | "o3" | "o4" => "openai",
        "gemini" => "gemini",
        _ => return None,
    };
    Some(Branded(brand))
}

pub fn model_mark(model: &str, subscription: &str, p: &Palette, size: f32, app: Option<&Image>) -> El {
    match model_service(model) {
        Some(ModelService::Branded(service)) => mark(service, p, size, None),
        Some(ModelService::Unbranded(provider)) => monogram(provider, p, size),
        None => mark(subscription, p, size, app),
    }
}

pub fn has_logo(agent: &str) -> bool {
    known(agent, &crate::style::palette(true)).is_some()
}

pub fn mark(agent: &str, p: &Palette, size: f32, app: Option<&Image>) -> El {
    let named = |e: El| e.label(agent.to_string()).help(agent.to_string());
    if let Some((logo, color)) = known(agent, p) {
        return named(icon(logo, size).color(color));
    }
    if let Some(img) = app.filter(|i| i.size().is_some()) {
        let s = size * 1.2;
        return named(el().size(s, s).pixels(img.pixels()));
    }
    monogram(agent, p, size)
}

fn monogram(name: &str, p: &Palette, size: f32) -> El {
    let initial = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
    el().size(size, size)
        .radius(size / 2.0)
        .border(1.0, p.text_3)
        .center()
        .child(text(initial).font(TextStyle::new(size * 0.55, crate::style::MEDIUM)).color(p.text_2))
        .label(name.to_string())
        .help(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::ModelService::{Branded, Unbranded};
    use super::model_service;

    #[test]
    fn models_map_to_the_service_they_come_from() {
        assert_eq!(model_service("openai/gpt-5.6-luna"), Some(Branded("openai")));
        assert_eq!(model_service("OpenCode-Go/deepseek-v4.1-flash"), Some(Branded("opencode")));
        assert_eq!(model_service("anthropic/claude-opus-5"), Some(Branded("claude")));
        assert_eq!(model_service("google/gemini-3-pro"), Some(Branded("gemini")));
        assert_eq!(model_service("sonnet"), Some(Branded("claude")));
        assert_eq!(model_service("gpt-6-sol"), Some(Branded("openai")));
        assert_eq!(model_service("o3-mini"), Some(Branded("openai")));
    }

    #[test]
    fn explicit_unknown_services_do_not_borrow_a_model_makers_mark() {
        assert_eq!(model_service("openrouter/anthropic/claude-opus-5"), Some(Unbranded("openrouter")));
        assert_eq!(model_service("azure/gpt-6-sol"), Some(Unbranded("azure")));
        assert_eq!(model_service("opencode-compatible/gpt-6-sol"), Some(Unbranded("opencode-compatible")));
        assert_eq!(model_service("opaque-model"), None);
        assert_eq!(model_service("openai/"), None);
    }
}
