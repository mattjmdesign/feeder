use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Backend;
use crate::Result;

pub(super) fn command(backend: Backend, binary: &Path, data: &Path) -> Result<Command> {
    let profile = profile_dir(backend, data)?;
    let mut cmd = Command::new(binary);
    cmd.env_clear().current_dir(&profile);

    cmd.env("HOME", dirs::home_dir().unwrap_or_else(|| profile.clone()));

    for var in ["PATH", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    Ok(cmd)
}

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
