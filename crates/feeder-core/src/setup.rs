//! Connects Feeder's MCP server to the agent tools on this Mac and installs its skills where
//! tools load skills. `feeder setup` and the app's Settings both run this.
//!
//! Every entry it writes is a local stdio server (`command` + `args`), never a URL: the tool starts
//! `feeder mcp` as a child process and talks to it over stdin and stdout. Nothing listens on a port.
//! JSON configs are edited in place, keeping their other keys and order; a file that isn't plain
//! JSON (comments) is left alone and the snippet is printed instead. Each file is copied to
//! `<file>.feeder-backup` before its first change.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cursor,
    Antigravity,
    T3Code,
    OpenCode,
}

impl Tool {
    pub const ALL: [Tool; 7] =
        [Tool::ClaudeCode, Tool::ClaudeDesktop, Tool::Codex, Tool::Cursor, Tool::Antigravity, Tool::T3Code, Tool::OpenCode];

    pub fn id(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
            Tool::T3Code => "t3-code",
            Tool::Codex => "codex",
            Tool::ClaudeDesktop => "claude-desktop",
            Tool::Cursor => "cursor",
            Tool::Antigravity => "antigravity",
            Tool::OpenCode => "opencode",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "Claude Code",
            Tool::T3Code => "T3 Code",
            Tool::Codex => "Codex",
            Tool::ClaudeDesktop => "Claude Desktop",
            Tool::Cursor => "Cursor",
            Tool::Antigravity => "Antigravity",
            Tool::OpenCode => "OpenCode",
        }
    }

    pub fn parse(s: &str) -> Option<Tool> {
        let s = s.trim().to_ascii_lowercase().replace([' ', '_'], "-");
        let alias = match s.as_str() {
            "claude" => "claude-code",
            "t3" | "t3code" => "t3-code",
            "claude-app" => "claude-desktop",
            other => other,
        };
        Tool::ALL.into_iter().find(|t| t.id() == alias)
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Where each tool keeps its MCP servers, and the shape of the entry.
enum Config {
    /// Registered through the tool's own CLI (`claude mcp add`), optionally against another
    /// config dir.
    ClaudeCli {
        config_dir: Option<PathBuf>,
    },
    Json {
        path: PathBuf,
        key: &'static str,
        opencode: bool,
    },
    CodexToml {
        path: PathBuf,
    },
}

/// T3 Code runs Claude Code with its own config dirs (`~/.claude-<profile>-t3`), and Codex with
/// the shared `~/.codex`.
fn t3_config_dirs() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home()) else { return vec![] };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".claude-") && n.ends_with("-t3")))
        .collect();
    dirs.sort();
    dirs
}

fn configs(tool: Tool) -> Vec<Config> {
    let h = home();
    match tool {
        Tool::ClaudeCode => vec![Config::ClaudeCli { config_dir: None }],
        Tool::T3Code => {
            let mut c: Vec<Config> = t3_config_dirs().into_iter().map(|d| Config::ClaudeCli { config_dir: Some(d) }).collect();
            c.push(Config::CodexToml { path: h.join(".codex/config.toml") });
            c
        }
        Tool::Codex => vec![Config::CodexToml { path: h.join(".codex/config.toml") }],
        Tool::ClaudeDesktop => vec![Config::Json {
            path: h.join("Library/Application Support/Claude/claude_desktop_config.json"),
            key: "mcpServers",
            opencode: false,
        }],
        Tool::Cursor => vec![Config::Json { path: h.join(".cursor/mcp.json"), key: "mcpServers", opencode: false }],
        Tool::Antigravity => vec![Config::Json { path: h.join(".gemini/antigravity/mcp_config.json"), key: "mcpServers", opencode: false }],
        Tool::OpenCode => vec![Config::Json { path: h.join(".config/opencode/opencode.json"), key: "mcp", opencode: true }],
    }
}

/// Whether the tool looks installed: its CLI is on PATH, its app exists or its config folder does.
pub fn detected(tool: Tool) -> bool {
    let h = home();
    let app = app_path(tool).is_some();
    match tool {
        Tool::ClaudeCode => which("claude").is_some(),
        Tool::T3Code => app || !t3_config_dirs().is_empty(),
        Tool::Codex => which("codex").is_some() || h.join(".codex").is_dir(),
        Tool::ClaudeDesktop => app,
        Tool::Cursor => app || h.join(".cursor").is_dir(),
        Tool::Antigravity => app || h.join(".gemini/antigravity").is_dir(),
        Tool::OpenCode => which("opencode").is_some() || app || h.join(".config/opencode").is_dir(),
    }
}

impl Tool {
    /// The app bundles the tool installs as, most likely first.
    pub fn app_names(self) -> &'static [&'static str] {
        match self {
            Tool::ClaudeCode => &[],
            Tool::ClaudeDesktop => &["Claude.app"],
            Tool::Codex => &["Codex.app"],
            Tool::Cursor => &["Cursor.app"],
            Tool::Antigravity => &["Antigravity.app"],
            Tool::T3Code => &["T3 Code (Beta).app", "T3 Code.app"],
            Tool::OpenCode => &["OpenCode.app"],
        }
    }
}

/// The tool's installed app, in /Applications or ~/Applications, for its icon.
pub fn app_path(tool: Tool) -> Option<PathBuf> {
    let h = home();
    tool.app_names().iter().flat_map(|name| [Path::new("/Applications").join(name), h.join("Applications").join(name)]).find(|p| p.exists())
}

pub(crate) fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    // GUI apps and some shells don't put these on PATH.
    dirs.extend([
        home().join(".local/bin"),
        home().join(".cargo/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    dirs.into_iter().map(|d| d.join(cmd)).find(|p| p.is_file())
}

/// The `feeder` the tools should start: an absolute path, since GUI apps don't see your shell's
/// PATH. From the app it is the CLI bundled beside it (`Contents/Helpers/feeder`); a build inside
/// a cargo `target/` folder points at the installed copy instead.
pub fn feeder_binary() -> PathBuf {
    let installed = home().join(".cargo/bin/feeder");
    let exe = std::env::current_exe().and_then(|p| p.canonicalize());
    let bundled = exe.as_ref().ok().and_then(|e| {
        let macos = e.parent().filter(|d| d.ends_with("Contents/MacOS"))?;
        Some(macos.parent()?.join("Helpers/feeder")).filter(|cli| cli.is_file())
    });
    match (bundled, exe) {
        (Some(cli), _) => cli,
        (None, Ok(exe)) if !exe.components().any(|c| c.as_os_str() == "target") => exe,
        _ if installed.exists() => installed,
        (None, Ok(exe)) => exe,
        (None, Err(_)) => installed,
    }
}

fn entry(bin: &Path, opencode: bool) -> Value {
    let bin = bin.display().to_string();
    if opencode { json!({ "type": "local", "command": [bin, "mcp"], "enabled": true }) } else { json!({ "command": bin, "args": ["mcp"] }) }
}

fn codex_block(bin: &Path) -> String {
    format!("[mcp_servers.feeder]\ncommand = {:?}\nargs = [\"mcp\"]\n", bin.display().to_string())
}

/// An `mcpServers` block that starts `bin`, for tools `setup` doesn't know: most read this shape.
pub fn mcp_json(bin: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "feeder": entry(bin, false) } })).unwrap_or_default()
}

/// The config each tool needs, for `feeder setup --print`.
pub fn snippet(tool: Tool, bin: &Path) -> String {
    let mut out = String::new();
    for c in configs(tool) {
        match c {
            Config::ClaudeCli { config_dir } => {
                let prefix = config_dir.map(|d| format!("CLAUDE_CONFIG_DIR={:?} ", d.display().to_string())).unwrap_or_default();
                out.push_str(&format!("{prefix}claude mcp add --scope user feeder -- {:?} mcp\n", bin.display().to_string()));
            }
            Config::Json { path, key, opencode } => {
                let block = json!({ key: { "feeder": entry(bin, opencode) } });
                out.push_str(&format!("# {}\n{}\n", path.display(), serde_json::to_string_pretty(&block).unwrap_or_default()));
            }
            Config::CodexToml { path } => out.push_str(&format!("# {}\n{}", path.display(), codex_block(bin))),
        }
    }
    out
}

/// Whether a tool's configs start this Feeder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Every config the tool reads has a `feeder` server that runs `bin`.
    Connected,
    /// Some config has a `feeder` server, but one runs another copy (or one config lacks it).
    Outdated,
    /// No config has a `feeder` server.
    Missing,
}

/// Reads the tool's configs without changing them. Claude Code's are read from its
/// `.claude.json`, so no `claude` process starts.
pub fn status(tool: Tool, bin: &Path) -> Status {
    let found: Vec<Option<bool>> = configs(tool).into_iter().map(|c| configured_command(c).map(|cmd| same_file(&cmd, bin))).collect();
    if found.iter().all(|f| *f == Some(true)) {
        Status::Connected
    } else if found.iter().all(Option::is_none) {
        Status::Missing
    } else {
        Status::Outdated
    }
}

/// The command a config's `feeder` server runs, if it has one.
fn configured_command(config: Config) -> Option<PathBuf> {
    let read_json = |path: &Path| serde_json::from_str::<Value>(&std::fs::read_to_string(path).ok()?).ok();
    match config {
        Config::ClaudeCli { config_dir } => {
            let doc = read_json(&config_dir.unwrap_or_else(home).join(".claude.json"))?;
            doc.get("mcpServers")?.get("feeder")?.get("command")?.as_str().map(PathBuf::from)
        }
        Config::Json { path, key, opencode } => {
            let doc = read_json(&path)?;
            let server = doc.get(key)?.get("feeder")?;
            let command = if opencode { server.get("command")?.get(0)? } else { server.get("command")? };
            command.as_str().map(PathBuf::from)
        }
        Config::CodexToml { path } => {
            let text = std::fs::read_to_string(path).ok()?;
            let table = text.split_inclusive('\n').skip_while(|l| l.trim() != "[mcp_servers.feeder]").skip(1);
            let line = table.take_while(|l| !l.trim_start().starts_with('[')).find(|l| l.trim_start().starts_with("command"))?;
            let value = line.split_once('=')?.1.trim();
            Some(PathBuf::from(value.trim_matches('"').replace("\\\\", "\\")))
        }
    }
}

/// Whether two paths name one file: `~/.cargo/bin/feeder` links to the app's copy.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

pub enum Outcome {
    Added,
    Updated,
    Removed,
    /// Already as asked: connected to this Feeder, or (disconnecting) never connected.
    Already,
    Skipped(String),
}

pub fn connect(tool: Tool, bin: &Path) -> Vec<(String, Outcome)> {
    configs(tool).into_iter().map(|c| apply(c, bin)).collect()
}

fn apply(config: Config, bin: &Path) -> (String, Outcome) {
    match config {
        Config::ClaudeCli { config_dir } => {
            let place = config_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Claude Code (user scope)".into());
            let Some(claude) = which("claude") else { return (place, Outcome::Skipped("the claude CLI isn't installed".into())) };
            let run = |args: &[&str]| {
                let mut cmd = Command::new(&claude);
                cmd.args(args);
                if let Some(d) = &config_dir {
                    cmd.env("CLAUDE_CONFIG_DIR", d);
                }
                cmd.output()
            };
            let existed = run(&["mcp", "get", "feeder"]).is_ok_and(|o| o.status.success());
            if existed {
                let _ = run(&["mcp", "remove", "--scope", "user", "feeder"]);
            }
            let bin_s = bin.display().to_string();
            match run(&["mcp", "add", "--scope", "user", "feeder", "--", &bin_s, "mcp"]) {
                Ok(o) if o.status.success() => (place, if existed { Outcome::Updated } else { Outcome::Added }),
                Ok(o) => (place, Outcome::Skipped(String::from_utf8_lossy(&o.stderr).trim().to_string())),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
        Config::Json { path, key, opencode } => {
            let place = path.display().to_string();
            let mut doc = match std::fs::read_to_string(&path) {
                Ok(text) if text.trim().is_empty() => json!({}),
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) if v.is_object() => v,
                    _ => {
                        return (
                            place,
                            Outcome::Skipped("not plain JSON (comments?); add the snippet from `feeder setup --print` by hand".into()),
                        );
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
                Err(e) => return (place, Outcome::Skipped(e.to_string())),
            };
            let want = entry(bin, opencode);
            let servers = doc.as_object_mut().expect("checked above").entry(key).or_insert_with(|| json!({}));
            let Some(servers) = servers.as_object_mut() else { return (place, Outcome::Skipped(format!("`{key}` isn't an object"))) };
            let outcome = match servers.get("feeder") {
                Some(v) if *v == want => return (place, Outcome::Already),
                Some(_) => Outcome::Updated,
                None => Outcome::Added,
            };
            servers.insert("feeder".into(), want);
            match write_with_backup(&path, &(serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n")) {
                Ok(()) => (place, outcome),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
        Config::CodexToml { path } => {
            let place = path.display().to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let block = codex_block(bin);
            if text.contains(&block) {
                return (place, Outcome::Already);
            }
            let (next, outcome) = match remove_toml_table(&text, "mcp_servers.feeder") {
                Some(without) => (without, Outcome::Updated),
                None => (text.clone(), Outcome::Added),
            };
            let sep = if next.is_empty() || next.ends_with("\n\n") {
                ""
            } else if next.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            match write_with_backup(&path, &format!("{next}{sep}{block}")) {
                Ok(()) => (place, outcome),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
    }
}

/// `text` without the `[name]` table (up to the next table header), or None when it has none.
fn remove_toml_table(text: &str, name: &str) -> Option<String> {
    let header = format!("[{name}]");
    let mut out = String::new();
    let mut found = false;
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t == header {
            found = true;
            skipping = true;
            continue;
        }
        if skipping && t.starts_with('[') {
            skipping = false;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    found.then_some(out)
}

fn write_with_backup(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let target = if path.is_symlink() { std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()) } else { path.to_path_buf() };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let backup = PathBuf::from(format!("{}.feeder-backup", target.display()));
    if target.exists() && !backup.exists() {
        let existing = std::fs::read(&target)?;
        write_private_new(&backup, &existing)?;
    }
    let tmp = PathBuf::from(format!("{}.feeder-tmp-{}-{:08x}", target.display(), std::process::id(), fastrand::u32(..)));
    let write_res = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn write_private_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Writes the `feeder-update` and `feeder-notebook` skills into every skills folder these tools read that exists.
pub fn install_skills() -> Vec<PathBuf> {
    let skills = [("feeder-update", crate::skill_markdown()), ("feeder-notebook", crate::notebook_skill_markdown())];
    skill_roots()
        .into_iter()
        .filter(|r| r.is_dir())
        .flat_map(|r| skills.iter().map(move |(name, body)| (r.join(name), body)))
        .filter_map(|(dir, body)| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::write(dir.join("SKILL.md"), body).ok()?;
            Some(dir)
        })
        .collect()
}

/// Deletes the skills `install_skills` wrote, from every folder it writes to.
pub fn remove_skills() -> Vec<PathBuf> {
    skill_roots()
        .into_iter()
        .flat_map(|r| ["feeder-update", "feeder-notebook"].map(|name| r.join(name)))
        .filter(|dir| dir.join("SKILL.md").is_file() && std::fs::remove_dir_all(dir).is_ok())
        .collect()
}

/// Removes Feeder's server from every config `tool` reads; the inverse of [`connect`].
pub fn disconnect(tool: Tool) -> Vec<(String, Outcome)> {
    configs(tool).into_iter().map(unapply).collect()
}

fn unapply(config: Config) -> (String, Outcome) {
    match config {
        Config::ClaudeCli { config_dir } => {
            let place = config_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Claude Code (user scope)".into());
            if configured_command(Config::ClaudeCli { config_dir: config_dir.clone() }).is_none() {
                return (place, Outcome::Already);
            }
            let Some(claude) = which("claude") else { return (place, Outcome::Skipped("the claude CLI isn't installed".into())) };
            let mut cmd = Command::new(claude);
            cmd.args(["mcp", "remove", "--scope", "user", "feeder"]);
            if let Some(d) = &config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", d);
            }
            match cmd.output() {
                Ok(o) if o.status.success() => (place, Outcome::Removed),
                Ok(o) => (place, Outcome::Skipped(String::from_utf8_lossy(&o.stderr).trim().to_string())),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
        Config::Json { path, key, .. } => {
            let place = path.display().to_string();
            let Ok(text) = std::fs::read_to_string(&path) else { return (place, Outcome::Already) };
            let Ok(mut doc) = serde_json::from_str::<Value>(&text) else {
                return (place, Outcome::Skipped("not plain JSON (comments?); remove the `feeder` entry by hand".into()));
            };
            let removed = doc.get_mut(key).and_then(Value::as_object_mut).and_then(|servers| servers.remove("feeder"));
            if removed.is_none() {
                return (place, Outcome::Already);
            }
            match write_with_backup(&path, &(serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n")) {
                Ok(()) => (place, Outcome::Removed),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
        Config::CodexToml { path } => {
            let place = path.display().to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let Some(without) = remove_toml_table(&text, "mcp_servers.feeder") else { return (place, Outcome::Already) };
            match write_with_backup(&path, &without) {
                Ok(()) => (place, Outcome::Removed),
                Err(e) => (place, Outcome::Skipped(e.to_string())),
            }
        }
    }
}

/// Every skills folder the tools read.
fn skill_roots() -> Vec<PathBuf> {
    let h = home();
    let mut roots = vec![
        h.join(".claude/skills"),
        h.join(".codex/skills"),
        h.join(".agents/skills"),
        h.join(".config/opencode/skills"),
        h.join(".gemini/antigravity/skills"),
    ];
    roots.extend(t3_config_dirs().into_iter().map(|d| d.join("skills")));
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        roots.push(PathBuf::from(d).join("skills"));
    }
    roots.sort();
    roots.dedup();
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_parse_with_aliases() {
        assert_eq!(Tool::parse("Claude"), Some(Tool::ClaudeCode));
        assert_eq!(Tool::parse("t3"), Some(Tool::T3Code));
        assert_eq!(Tool::parse("OpenCode"), Some(Tool::OpenCode));
        assert_eq!(Tool::parse("vim"), None);
    }

    #[test]
    fn json_configs_keep_their_other_servers_and_order() {
        let dir = std::env::temp_dir().join(format!("feeder-setup-{}", std::process::id()));
        let path = dir.join("mcp.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"zeta": 1, "mcpServers": {"other": {"url": "x"}}, "alpha": 2}"#).unwrap();
        let bin = Path::new("/usr/local/bin/feeder");
        let cfg = || Config::Json { path: path.clone(), key: "mcpServers", opencode: false };
        assert!(matches!(apply(cfg(), bin).1, Outcome::Added));
        assert!(matches!(apply(cfg(), bin).1, Outcome::Already));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap(), "order kept: {text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["other"]["url"], "x");
        assert_eq!(v["mcpServers"]["feeder"]["args"][0], "mcp");
        assert!(dir.join("mcp.json.feeder-backup").exists());
        std::fs::write(&path, "{ // comment\n}").unwrap();
        assert!(matches!(apply(cfg(), bin).1, Outcome::Skipped(_)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_reads_each_config_shape() {
        let dir = std::env::temp_dir().join(format!("feeder-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("feeder");
        std::fs::write(&bin, "").unwrap();
        let json = |p: &str| Config::Json { path: dir.join(p), key: "mcpServers", opencode: false };
        assert_eq!(configured_command(json("missing.json")), None);
        apply(json("mcp.json"), &bin);
        assert_eq!(configured_command(json("mcp.json")), Some(bin.clone()));
        let toml = dir.join("config.toml");
        apply(Config::CodexToml { path: toml.clone() }, Path::new("/old/feeder"));
        assert_eq!(configured_command(Config::CodexToml { path: toml.clone() }), Some(PathBuf::from("/old/feeder")));
        let opencode = Config::Json { path: dir.join("opencode.json"), key: "mcp", opencode: true };
        apply(opencode, &bin);
        assert_eq!(configured_command(Config::Json { path: dir.join("opencode.json"), key: "mcp", opencode: true }), Some(bin.clone()));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&bin, &link).unwrap();
        assert!(same_file(&link, &bin), "a link to the same CLI counts as connected");
        assert!(mcp_json(&bin).contains("\"args\": [\n        \"mcp\"\n      ]"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn disconnect_removes_only_feeders_entry() {
        let dir = std::env::temp_dir().join(format!("feeder-unsetup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = Path::new("/usr/local/bin/feeder");
        let path = dir.join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers": {"other": {"url": "x"}}}"#).unwrap();
        let json = || Config::Json { path: path.clone(), key: "mcpServers", opencode: false };
        apply(json(), bin);
        assert!(matches!(unapply(json()).1, Outcome::Removed));
        assert!(matches!(unapply(json()).1, Outcome::Already), "a second disconnect changes nothing");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("feeder").is_none() && v["mcpServers"]["other"]["url"] == "x");
        let toml = dir.join("config.toml");
        std::fs::write(&toml, "model = \"x\"\n").unwrap();
        apply(Config::CodexToml { path: toml.clone() }, bin);
        assert!(matches!(unapply(Config::CodexToml { path: toml.clone() }).1, Outcome::Removed));
        assert!(!std::fs::read_to_string(&toml).unwrap().contains("feeder"));
        assert!(matches!(unapply(Config::Json { path: dir.join("none.json"), key: "mcpServers", opencode: false }).1, Outcome::Already));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn codex_toml_gets_one_feeder_table() {
        let dir = std::env::temp_dir().join(format!("feeder-codex-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "model = \"x\"\n\n[mcp_servers.feeder]\ncommand = \"/old/feeder\"\nargs = [\"mcp\"]\n\n[mcp_servers.other]\nurl = \"y\"\n",
        )
        .unwrap();
        let bin = Path::new("/new/feeder");
        assert!(matches!(apply(Config::CodexToml { path: path.clone() }, bin).1, Outcome::Updated));
        assert!(matches!(apply(Config::CodexToml { path: path.clone() }, bin).1, Outcome::Already));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[mcp_servers.feeder]").count(), 1);
        assert!(text.contains("/new/feeder") && !text.contains("/old/feeder") && text.contains("[mcp_servers.other]"));
        std::fs::remove_dir_all(dir).ok();
    }
}
