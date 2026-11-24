use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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

enum Config {
    ClaudeCli { config_dir: Option<PathBuf> },
    Json { path: PathBuf, key: &'static str, opencode: bool },
    CodexToml { path: PathBuf },
}

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

pub fn app_path(tool: Tool) -> Option<PathBuf> {
    let h = home();
    tool.app_names().iter().flat_map(|name| [Path::new("/Applications").join(name), h.join("Applications").join(name)]).find(|p| p.exists())
}

const PATH_PROBE_START: &str = "__FEEDER_PATH_START__";
const PATH_PROBE_END: &str = "__FEEDER_PATH_END__";
const PATH_PROBE_BYTES: usize = 64 * 1024;

static SEARCH_PATH_GENERATION: AtomicU64 = AtomicU64::new(0);
static SHELL_PATH_CACHE: Mutex<Option<(u64, Vec<PathBuf>)>> = Mutex::new(None);

pub fn refresh_search_paths() {
    SEARCH_PATH_GENERATION.fetch_add(1, Ordering::Relaxed);
}

fn probed_shell_paths() -> Vec<PathBuf> {
    if cfg!(test) {
        return Vec::new();
    }
    let generation = SEARCH_PATH_GENERATION.load(Ordering::Relaxed);
    let mut cache = SHELL_PATH_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached, paths)) = cache.as_ref()
        && *cached == generation
    {
        return paths.clone();
    }
    let paths = probe_login_shell_path();
    *cache = Some((generation, paths.clone()));
    paths
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn login_shell() -> Option<PathBuf> {
    if let Some(shell) = std::env::var_os("SHELL").map(PathBuf::from).filter(|p| is_executable_file(p)) {
        return Some(shell);
    }
    [Path::new("/bin/zsh"), Path::new("/bin/sh")].into_iter().map(Path::to_path_buf).find(|p| is_executable_file(p))
}

fn probe_login_shell_path() -> Vec<PathBuf> {
    let Some(shell) = login_shell() else {
        return Vec::new();
    };
    let is_fish = shell.file_name().and_then(|n| n.to_str()) == Some("fish");
    let script = if is_fish {
        format!("printf '{PATH_PROBE_START}%s{PATH_PROBE_END}' (string join : $PATH)")
    } else {
        format!("printf '{PATH_PROBE_START}%s{PATH_PROBE_END}' \"$PATH\"")
    };
    run_path_probe(&shell, &["-l", "-i", "-c", &script], Duration::from_millis(1500))
        .or_else(|| run_path_probe(&shell, &["-l", "-c", &script], Duration::from_millis(1000)))
        .unwrap_or_default()
}

fn kill_process_group(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id())
        && group > 0
    {
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_path_probe(shell: &Path, args: &[&str], timeout: Duration) -> Option<Vec<PathBuf>> {
    let mut cmd = Command::new(shell);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("ZSH_TMUX_AUTOSTART", "false");
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("feeder-path-probe".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut reader = stdout;
            while buf.len() < PATH_PROBE_BYTES {
                let limit = (PATH_PROBE_BYTES - buf.len()).min(chunk.len());
                let Ok(n) = reader.read(&mut chunk[..limit]) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if String::from_utf8_lossy(&buf).contains(PATH_PROBE_END) {
                    break;
                }
            }
            let _ = tx.send(buf);
        })
        .is_err()
    {
        kill_process_group(&mut child);
        return None;
    }

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            Ok(None) | Err(_) => {
                kill_process_group(&mut child);
                break None;
            }
        }
    }?;
    if !status.success() {
        return None;
    }
    let bytes = match rx.recv_timeout(Duration::from_millis(100)) {
        Ok(bytes) => bytes,
        Err(_) => {
            kill_process_group(&mut child);
            return None;
        }
    };
    parse_path_probe(&String::from_utf8_lossy(&bytes))
}

fn parse_path_probe(output: &str) -> Option<Vec<PathBuf>> {
    let (_, rest) = output.split_once(PATH_PROBE_START)?;
    let (raw, _) = rest.split_once(PATH_PROBE_END)?;
    let dirs: Vec<PathBuf> = std::env::split_paths(raw).filter(|p| !p.as_os_str().is_empty()).collect();
    (!dirs.is_empty()).then_some(dirs)
}

fn parse_etc_paths(text: &str) -> Vec<PathBuf> {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(PathBuf::from).collect()
}

fn etc_paths_dirs() -> Vec<PathBuf> {
    let mut dirs = std::fs::read_to_string("/etc/paths").map(|t| parse_etc_paths(&t)).unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir("/etc/paths.d") {
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for file in files {
            if let Ok(text) = std::fs::read_to_string(file) {
                dirs.extend(parse_etc_paths(&text));
            }
        }
    }
    dirs
}

fn parse_semver_dir(name: &str) -> Option<(u64, u64, u64)> {
    let trimmed = name.strip_prefix('v').unwrap_or(name);
    let mut parts = trimmed.split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn versioned_node_bins(home: &Path) -> Vec<PathBuf> {
    let roots = [
        (home.join(".nvm/versions/node"), Path::new("bin")),
        (home.join("Library/Application Support/fnm/node-versions"), Path::new("installation/bin")),
        (home.join(".local/share/fnm/node-versions"), Path::new("installation/bin")),
        (home.join(".fnm/node-versions"), Path::new("installation/bin")),
        (home.join(".local/share/mise/installs/node"), Path::new("bin")),
        (PathBuf::from("/usr/local/n/versions/node"), Path::new("bin")),
    ];
    let mut bins = Vec::new();
    for (root, suffix) in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut versions: Vec<((u64, u64, u64), PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let ver = parse_semver_dir(name.to_str()?)?;
                let bin_dir = entry.path().join(suffix);
                bin_dir.is_dir().then_some((ver, bin_dir))
            })
            .collect();
        versions.sort_by_key(|a| std::cmp::Reverse(a.0));
        bins.extend(versions.into_iter().map(|(_, p)| p));
    }
    bins
}

fn fallback_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        home.join(".opencode/bin"),
        home.join(".claude/local"),
        home.join(".claude/local/node_modules/.bin"),
        home.join(".antigravity/antigravity/bin"),
        home.join(".codex/bin"),
        home.join(".codex/packages/standalone/current/bin"),
        home.join(".bun/bin"),
        home.join(".volta/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".mise/shims"),
        home.join(".asdf/shims"),
        home.join(".nodenv/shims"),
        home.join("Library/pnpm"),
        home.join(".local/share/pnpm"),
        home.join(".npm-global/bin"),
        home.join(".npm/bin"),
        home.join(".yarn/bin"),
        home.join(".config/yarn/global/node_modules/.bin"),
        home.join(".cargo/bin"),
        home.join("bin"),
        home.join(".n/bin"),
    ];
    dirs.extend(versioned_node_bins(home));
    dirs.extend([
        PathBuf::from("/Applications/Antigravity.app/Contents/Resources/app/bin"),
        home.join("Applications/Antigravity.app/Contents/Resources/app/bin"),
        PathBuf::from("/Applications/Cursor.app/Contents/Resources/app/bin"),
        home.join("Applications/Cursor.app/Contents/Resources/app/bin"),
        PathBuf::from("/Applications/Codex.app/Contents/Resources"),
        home.join("Applications/Codex.app/Contents/Resources"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/sbin"),
    ]);
    if !cfg!(test) {
        dirs.extend(etc_paths_dirs());
    }
    dirs
}

fn push_unique_dirs(out: &mut Vec<PathBuf>, dirs: impl IntoIterator<Item = PathBuf>) {
    for dir in dirs {
        if !dir.as_os_str().is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    }
}

pub(crate) fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_path) = std::env::var_os("PATH") {
        push_unique_dirs(&mut dirs, std::env::split_paths(&env_path));
    }
    push_unique_dirs(&mut dirs, probed_shell_paths());
    push_unique_dirs(&mut dirs, fallback_dirs(&home()));
    dirs
}

fn search_path_env_from(extra_front: Option<&Path>, dirs: Vec<PathBuf>) -> OsString {
    let mut combined = Vec::new();
    if let Some(front) = extra_front {
        push_unique_dirs(&mut combined, [front.to_path_buf()]);
    }
    push_unique_dirs(&mut combined, dirs);
    std::env::join_paths(combined).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

pub(crate) fn search_path_env(extra_front: Option<&Path>) -> OsString {
    search_path_env_from(extra_front, search_dirs())
}

pub(crate) fn which(cmd: &str) -> Option<PathBuf> {
    let env_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    find_executable(cmd, env_dirs)
        .or_else(|| find_executable(cmd, probed_shell_paths()))
        .or_else(|| find_executable(cmd, fallback_dirs(&home())))
}

fn find_executable(cmd: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter().find_map(|dir| {
        let candidate = dir.join(cmd);
        if !is_executable_file(&candidate) {
            return None;
        }
        std::path::absolute(candidate).ok()
    })
}

pub fn feeder_binary() -> PathBuf {
    let installed = home().join(".cargo/bin/feeder");
    let exe = std::env::current_exe().and_then(|p| p.canonicalize());
    let bundled = exe.as_deref().ok().and_then(bundled_feeder).filter(|cli| cli.is_file());
    match (bundled, exe) {
        (Some(cli), _) => cli,
        (None, Ok(exe)) if !exe.components().any(|c| c.as_os_str() == "target") => exe,
        _ if installed.exists() => installed,
        (None, Ok(exe)) => exe,
        (None, Err(_)) => installed,
    }
}

pub(crate) fn bundled_feeder(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent().filter(|dir| dir.ends_with("Contents/MacOS"))?;
    Some(macos.parent()?.join("Helpers/feeder"))
}

fn entry(bin: &Path, opencode: bool) -> Value {
    let bin = bin.display().to_string();
    if opencode { json!({ "type": "local", "command": [bin, "mcp"], "enabled": true }) } else { json!({ "command": bin, "args": ["mcp"] }) }
}

fn codex_block(bin: &Path) -> String {
    format!("[mcp_servers.feeder]\ncommand = {:?}\nargs = [\"mcp\"]\n", bin.display().to_string())
}

pub fn mcp_json(bin: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "feeder": entry(bin, false) } })).unwrap_or_default()
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connected,
    Outdated,
    Missing,
}

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

pub fn cli_on_path(bin: &Path) -> Option<(PathBuf, bool)> {
    let found = which("feeder")?;
    Some((found.clone(), same_file(&found, bin)))
}

fn dir_in_paths(paths: &[PathBuf], target: &Path) -> bool {
    paths.iter().any(|dir| same_file(dir, target))
}

pub fn install_cli_link(bin: &Path) -> std::io::Result<(PathBuf, bool)> {
    let mut path_dirs: Vec<PathBuf> = Vec::new();
    if let Some(env_path) = std::env::var_os("PATH") {
        push_unique_dirs(&mut path_dirs, std::env::split_paths(&env_path));
    }
    push_unique_dirs(&mut path_dirs, probed_shell_paths());
    push_unique_dirs(&mut path_dirs, etc_paths_dirs());
    let preferred =
        [home().join(".local/bin"), home().join(".cargo/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    let target_dir = preferred
        .iter()
        .find(|dir| dir_in_paths(&path_dirs, dir) && dir_writable(dir))
        .cloned()
        .unwrap_or_else(|| home().join(".local/bin"));
    std::fs::create_dir_all(&target_dir)?;
    let link = target_dir.join("feeder");
    let tmp = target_dir.join(format!(".feeder-link-{}-{:08x}", std::process::id(), fastrand::u32(..)));
    std::os::unix::fs::symlink(bin, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, &link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let on_path = dir_in_paths(&path_dirs, &target_dir);
    Ok((link, on_path))
}

fn dir_writable(dir: &Path) -> bool {
    let Ok(meta) = dir.metadata() else { return false };
    if !meta.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".feeder-probe-{}-{:08x}", std::process::id(), fastrand::u32(..)));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn strip_jsonc_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                let end = next_char_end(input, i + 1);
                out.push_str(&input[i..end]);
                i = end;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            let end = next_char_end(input, i);
            out.push_str(&input[i..end]);
            i = end;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let end = next_char_end(input, i);
        out.push_str(&input[i..end]);
        i = end;
    }
    out
}

fn next_char_end(s: &str, start: usize) -> usize {
    s[start..].chars().next().map_or(s.len(), |c| start + c.len_utf8())
}

fn configured_command(config: Config) -> Option<PathBuf> {
    let read_json = |path: &Path| serde_json::from_str::<Value>(&std::fs::read_to_string(path).ok()?).ok();
    let read_jsonc = |path: &Path| {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<Value>(&strip_jsonc_comments(&raw)).ok()
    };
    match config {
        Config::ClaudeCli { config_dir } => {
            let doc = read_json(&config_dir.unwrap_or_else(home).join(".claude.json"))?;
            doc.get("mcpServers")?.get("feeder")?.get("command")?.as_str().map(PathBuf::from)
        }
        Config::Json { path, key, opencode } => {
            let doc = if opencode { read_jsonc(&path).or_else(|| read_jsonc(&path.with_extension("jsonc"))) } else { read_json(&path) }?;
            let server = doc.get(key)?.get("feeder")?;
            if opencode && server.get("enabled").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            let command = if opencode { server.get("command")?.get(0)? } else { server.get("command")? };
            command.as_str().map(PathBuf::from)
        }
        Config::CodexToml { path } => {
            let text = std::fs::read_to_string(path).ok()?;
            parse_codex_command(&text)
        }
    }
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_dq = false;
    let mut in_sq = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if in_dq {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_dq = false;
            }
        } else if in_sq {
            if ch == '\'' {
                in_sq = false;
            }
        } else {
            match ch {
                '"' => in_dq = true,
                '\'' => in_sq = true,
                '#' => return &line[..idx],
                _ => {}
            }
        }
    }
    line
}

fn parse_codex_command(text: &str) -> Option<PathBuf> {
    let mut in_table = false;
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_table = line == "[mcp_servers.feeder]";
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else { continue };
        if key.trim() != "command" {
            continue;
        }
        let val = val.trim();
        let parsed = if let Some(inner) = val.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            inner.replace("\\\"", "\"").replace("\\\\", "\\")
        } else if let Some(inner) = val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            inner.to_string()
        } else {
            continue;
        };
        return Some(PathBuf::from(parsed));
    }
    None
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

pub enum Outcome {
    Added,
    Updated,
    Removed,
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
            if configured_command(Config::ClaudeCli { config_dir: config_dir.clone() }).is_some_and(|cmd| same_file(&cmd, bin)) {
                return (place, Outcome::Already);
            }
            let Some(claude) = which("claude") else { return (place, Outcome::Skipped("the claude CLI isn't installed".into())) };
            let env_path = search_path_env(claude.parent());
            let run = |args: &[&str]| {
                let mut cmd = Command::new(&claude);
                cmd.env("PATH", &env_path).args(args);
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

fn remove_toml_table(text: &str, name: &str) -> Option<String> {
    let exact = format!("[{name}]");
    let subprefix = format!("[{name}.");
    let mut out = String::new();
    let mut found = false;
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let t = strip_toml_comment(line).trim();
        if t == exact || (t.starts_with(&subprefix) && t.ends_with(']')) {
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

pub fn install_skills() -> Vec<PathBuf> {
    let skills = [("feeder-update", crate::skill_markdown()), ("feeder-notebook", crate::notebook_skill_markdown())];
    skill_roots()
        .into_iter()
        .filter(|r| r.is_dir() || r.parent().is_some_and(Path::is_dir))
        .flat_map(|r| skills.iter().map(move |(name, body)| (r.join(name), body)))
        .filter_map(|(dir, body)| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::write(dir.join("SKILL.md"), body).ok()?;
            Some(dir)
        })
        .collect()
}

pub fn sync_installed_skills() -> Vec<PathBuf> {
    let skills = [("feeder-update", crate::skill_markdown()), ("feeder-notebook", crate::notebook_skill_markdown())];
    skill_roots()
        .into_iter()
        .filter(|r| r.is_dir())
        .flat_map(|r| skills.iter().map(move |(name, body)| (r.join(name), body)))
        .filter_map(|(dir, body)| {
            let file = dir.join("SKILL.md");
            let existing = std::fs::read_to_string(&file).ok()?;
            if existing == *body {
                return None;
            }
            std::fs::write(&file, body).ok()?;
            Some(dir)
        })
        .collect()
}

pub fn remove_skills() -> Vec<PathBuf> {
    skill_roots()
        .into_iter()
        .flat_map(|r| ["feeder-update", "feeder-notebook"].map(|name| r.join(name)))
        .filter(|dir| dir.join("SKILL.md").is_file() && std::fs::remove_dir_all(dir).is_ok())
        .collect()
}

pub fn disconnect(tool: Tool) -> Vec<(String, Outcome)> {
    configs(tool)
        .into_iter()
        .filter(|c| !(tool == Tool::T3Code && matches!(c, Config::CodexToml { .. }) && detected(Tool::Codex)))
        .map(unapply)
        .collect()
}

fn unapply(config: Config) -> (String, Outcome) {
    match config {
        Config::ClaudeCli { config_dir } => {
            let place = config_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Claude Code (user scope)".into());
            if configured_command(Config::ClaudeCli { config_dir: config_dir.clone() }).is_none() {
                return (place, Outcome::Already);
            }
            let Some(claude) = which("claude") else { return (place, Outcome::Skipped("the claude CLI isn't installed".into())) };
            let mut cmd = Command::new(&claude);
            cmd.env("PATH", search_path_env(claude.parent())).args(["mcp", "remove", "--scope", "user", "feeder"]);
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
    fn cli_lookup_skips_non_executable_files() {
        let dir = std::env::temp_dir().join(format!("feeder-cli-lookup-{}-{:08x}", std::process::id(), fastrand::u32(..)));
        let shadow = dir.join("shadow");
        std::fs::create_dir_all(&shadow).expect("create fixture directories");
        let blocked = shadow.join("opencode");
        let runnable = dir.join("opencode");
        std::fs::write(&blocked, "").expect("create non-executable candidate");
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).expect("clear execute bits");
        std::fs::write(&runnable, "#!/bin/sh\n").expect("create runnable candidate");
        std::fs::set_permissions(&runnable, std::fs::Permissions::from_mode(0o755)).expect("set execute bits");
        let found = find_executable("opencode", [shadow, dir.clone()]);
        std::fs::remove_dir_all(&dir).expect("remove fixtures");
        assert_eq!(found, Some(runnable));
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
        let disabled = json!({ "mcp": { "feeder": { "type": "local", "command": [bin, "mcp"], "enabled": false } } });
        std::fs::write(dir.join("opencode.json"), disabled.to_string()).expect("disable the saved connection");
        assert_eq!(configured_command(Config::Json { path: dir.join("opencode.json"), key: "mcp", opencode: true }), None);
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

    #[test]
    fn parse_path_probe_ignores_shell_noise_and_extracts_dirs() {
        let output =
            format!("Last login: Mon Nov 23\nmotd\n{PATH_PROBE_START}/Users/a/.local/bin:/opt/homebrew/bin:{PATH_PROBE_END}\nbye\n");
        assert_eq!(parse_path_probe(&output), Some(vec![PathBuf::from("/Users/a/.local/bin"), PathBuf::from("/opt/homebrew/bin")]));
        assert_eq!(parse_path_probe("no markers here"), None);
        assert_eq!(parse_path_probe(&format!("{PATH_PROBE_START}{PATH_PROBE_END}")), None);
    }

    #[test]
    fn versioned_node_bins_prefers_newest_semver() {
        let base = std::env::temp_dir().join(format!("feeder-nvm-{}-{:08x}", std::process::id(), fastrand::u32(..)));
        let node = base.join(".nvm/versions/node");
        for ver in ["v18.19.0", "v22.2.1", "v20.11.0", "system"] {
            std::fs::create_dir_all(node.join(ver).join("bin")).unwrap();
        }
        let found = versioned_node_bins(&base);
        std::fs::remove_dir_all(&base).ok();
        assert_eq!(found, vec![node.join("v22.2.1/bin"), node.join("v20.11.0/bin"), node.join("v18.19.0/bin")]);
    }

    #[test]
    fn parse_etc_paths_skips_blank_lines() {
        assert_eq!(
            parse_etc_paths("\n  /usr/local/bin  \n# comment\n\n/usr/bin\n"),
            vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]
        );
    }

    #[test]
    fn search_path_env_prepends_extra_without_duplicates() {
        let joined = search_path_env_from(
            Some(Path::new("/extra/bin")),
            vec![PathBuf::from("/first/bin"), PathBuf::from("/extra/bin"), PathBuf::from("/second/bin")],
        );
        let parsed: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parsed, vec![PathBuf::from("/extra/bin"), PathBuf::from("/first/bin"), PathBuf::from("/second/bin")]);
    }

    #[test]
    fn fallback_dirs_include_opencode_and_claude_local() {
        let h = Path::new("/tmp/feeder-home");
        let dirs = fallback_dirs(h);
        assert!(dirs.contains(&h.join(".opencode/bin")));
        assert!(dirs.contains(&h.join(".claude/local")));
        assert!(dirs.contains(&h.join(".local/bin")));
    }
}
