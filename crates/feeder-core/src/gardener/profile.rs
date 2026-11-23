//! Provider command environments for Curator.
//!
//! Every provider keeps the user's real `HOME`, so macOS finds the login keychain and the CLI's
//! existing sign-in, while `env_clear()` excludes shell API keys and each runner disables personal
//! hooks, settings, built-in tools, and personal MCP servers. Antigravity's Curator settings and
//! MCP config live in [`gemini_dir`], passed as `--gemini-dir`, so they never touch `~/.gemini`.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Backend;
use crate::Result;

/// Builds a provider command with shell secrets stripped and an empty working directory.
pub(super) fn command(backend: Backend, binary: &Path, data: &Path) -> Result<Command> {
    let profile = profile_dir(backend, data)?;
    let mut cmd = Command::new(binary);
    cmd.env_clear().current_dir(&profile);

    // A substitute HOME hides `~/Library/Keychains`, and macOS then can't save or read the
    // provider's sign-in ("Keychain Not Found").
    cmd.env("HOME", dirs::home_dir().unwrap_or_else(|| profile.clone()));

    // Keep standard OS session variables needed by macOS Keychain, LaunchServices browser launch,
    // and temp files, while excluding shell API keys and personal provider overrides.
    for var in ["PATH", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    Ok(cmd)
}

/// Antigravity's Curator-only settings folder, for `agy acp --gemini-dir`.
pub(super) fn gemini_dir(data: &Path) -> Result<PathBuf> {
    let dir = profile_dir(Backend::Antigravity, data)?.join(".gemini");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    Ok(dir)
}

pub(super) fn profile_dir(backend: Backend, data: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(data)?.join("curator-profiles").join(backend.as_str());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&path)?;
    Ok(path)
}
