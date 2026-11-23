pub fn agent_from_env() -> Option<String> {
    agent_from(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

fn agent_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(a) = get("FEEDER_AGENT") {
        return Some(agent_display_name(&a));
    }
    if let Some(host) = host_from(&get) {
        return Some(host.into());
    }
    let found = if get("OPENCODE").is_some() || get("OPENCODE_SESSION").is_some() {
        "OpenCode"
    } else if get("CLAUDECODE").is_some() {
        "Claude Code"
    } else if get("CODEX_SANDBOX").is_some() || get("CODEX_THREAD_ID").is_some() || get("CODEX_MANAGED_BY_NPM").is_some() {
        "Codex"
    } else if get("GEMINI_CLI").is_some() {
        "Gemini CLI"
    } else {
        return None;
    };
    Some(found.into())
}

pub(crate) fn host_from_env() -> Option<&'static str> {
    host_from(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
}

fn host_from(get: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    let bundle = get("__CFBundleIdentifier").unwrap_or_default();
    let found = if bundle == "app.openbot.desktop" || get("OPENBOT_AGENT_PROCESS").is_some() {
        "OpenBot"
    } else if get("T3CODE_THREAD_ID").is_some() || bundle == "com.t3tools.t3code" {
        "T3 Code"
    } else if bundle.contains("antigravity") || get("ANTIGRAVITY_AGENT").is_some() {
        "Antigravity"
    } else if bundle == "com.todesktop.230313mzl4w4u92" || get("CURSOR_TRACE_ID").is_some() || get("CURSOR_AGENT").is_some() {
        "Cursor"
    } else {
        return None;
    };
    Some(found)
}

pub fn agent_display_name(raw: &str) -> String {
    let id = raw.trim();
    let lower = id.to_ascii_lowercase();
    let known = [
        ("claude-code", "Claude Code"),
        ("claude code", "Claude Code"),
        ("claude-ai", "Claude"),
        ("claude", "Claude"),
        ("codex-mcp-client", "Codex"),
        ("codex", "Codex"),
        ("cursor-vscode", "Cursor"),
        ("cursor", "Cursor"),
        ("opencode", "OpenCode"),
        ("gemini-cli-mcp-client", "Gemini CLI"),
        ("gemini-cli", "Gemini CLI"),
        ("gemini", "Gemini"),
        ("windsurf", "Windsurf"),
        ("zed", "Zed"),
        ("amp", "Amp"),
        ("t3-code", "T3 Code"),
        ("t3code", "T3 Code"),
        ("antigravity", "Antigravity"),
    ];
    if let Some((_, name)) = known.iter().find(|(k, _)| *k == lower) {
        return (*name).to_string();
    }
    if id.is_empty() {
        return "Unknown agent".to_string();
    }
    if id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return id
            .split(['-', '_'])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_and_unknown_kebab_ids_read_well() {
        assert_eq!(agent_display_name("claude-code"), "Claude Code");
        assert_eq!(agent_display_name("codex-mcp-client"), "Codex");
        assert_eq!(agent_display_name("my-agent"), "My Agent");
        assert_eq!(agent_display_name("GPT Engineer"), "GPT Engineer");
    }

    #[test]
    fn a_host_app_wins_over_the_harness_inside_it() {
        let env =
            |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(agent_from(env(&[("CLAUDECODE", "1"), ("T3CODE_THREAD_ID", "x")])).as_deref(), Some("T3 Code"));
        assert_eq!(agent_from(env(&[("CLAUDECODE", "1")])).as_deref(), Some("Claude Code"));
        assert_eq!(agent_from(env(&[("CLAUDECODE", "1"), ("OPENBOT_AGENT_PROCESS", "1")])).as_deref(), Some("OpenBot"));
        assert_eq!(agent_from(env(&[("FEEDER_AGENT", "codex"), ("CLAUDECODE", "1")])).as_deref(), Some("Codex"));
        let named_agent = &[("FEEDER_AGENT", "Rust Dev"), ("OPENBOT_AGENT_PROCESS", "1"), ("CLAUDECODE", "1")];
        assert_eq!(agent_from(env(named_agent)).as_deref(), Some("Rust Dev"));
        assert_eq!(host_from(env(named_agent)), Some("OpenBot"));
        assert_eq!(host_from(env(&[("FEEDER_AGENT", "codex")])), None);
        assert_eq!(agent_from(env(&[])), None);
    }
}
