//! Per-provider login and configuration directories for Curator.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Backend;
use crate::Result;

/// Run with the CLI's own login in a Curator-specific profile, without inheriting shell secrets.
pub(super) fn command(backend: Backend, binary: &Path, data: &Path) -> Result<Command> {
    let home = profile_dir(backend, data)?;
    let mut cmd = Command::new(binary);
    cmd.env_clear().env("HOME", &home).current_dir(&home);
    match backend {
        Backend::ClaudeCode | Backend::Codex => {
            let (variable, name) = if backend == Backend::ClaudeCode { ("CLAUDE_CONFIG_DIR", ".claude") } else { ("CODEX_HOME", ".codex") };
            let config = home.join(name);
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&config)?;
            cmd.env(variable, config);
        }
        Backend::OpenCode | Backend::Antigravity => {}
    }
    for var in ["PATH", "LANG", "LC_ALL", "TERM"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    for (name, dir) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")]
    {
        cmd.env(name, home.join(dir));
    }
    Ok(cmd)
}

pub(super) fn profile_dir(backend: Backend, data: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(data)?.join("curator-profiles").join(backend.as_str());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&path)?;
    Ok(path)
}
