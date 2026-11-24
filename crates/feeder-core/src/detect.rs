use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedProject {
    pub name: String,
    pub path: PathBuf,
}

pub fn detect_project(dir: &Path) -> DetectedProject {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let root = match dir.ancestors().find(|p| p.join(".git").exists()) {
        Some(top) => main_worktree(top).unwrap_or_else(|| top.to_path_buf()),
        None => dir.clone(),
    };
    let name =
        root.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "untitled".to_string());
    DetectedProject { name, path: root }
}

fn main_worktree(top: &Path) -> Option<PathBuf> {
    let file = top.join(".git");
    if !file.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&file).ok()?;
    let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() { gitdir } else { top.join(gitdir) };
    let worktrees = gitdir.parent()?;
    let dotgit = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || dotgit.file_name()? != ".git" {
        return None;
    }
    let main = dotgit.parent()?;
    Some(main.canonicalize().unwrap_or_else(|_| main.to_path_buf()))
}

pub fn detect_branch(dir: &Path) -> Option<String> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let top = dir.ancestors().find(|p| p.join(".git").exists())?;
    let dotgit = top.join(".git");
    let git_dir = if dotgit.is_dir() {
        dotgit
    } else if dotgit.is_file() {
        let text = std::fs::read_to_string(&dotgit).ok()?;
        let raw = text.trim().strip_prefix("gitdir:")?.trim();
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { top.join(p) }
    } else {
        return None;
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        let branch = branch.trim();
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    if let Some(rest) = head.strip_prefix("ref:") {
        let name = rest.trim().rsplit('/').next().unwrap_or("").trim();
        return (!name.is_empty()).then(|| name.to_string());
    }
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "untitled".to_string() } else { slug }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitMeta {
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub diff_stat: Option<String>,
}

pub fn detect_git_meta(dir: &Path) -> GitMeta {
    let branch = detect_branch(dir);
    if branch.is_none() {
        return GitMeta::default();
    }
    let commit_sha = git_output(dir, &["rev-parse", "--short=7", "HEAD"]).filter(|s| !s.is_empty());
    let raw_stat = git_output(dir, &["diff", "--shortstat", "HEAD"])
        .filter(|s| !s.is_empty())
        .or_else(|| git_output(dir, &["diff-tree", "--no-commit-id", "--shortstat", "-r", "HEAD"]));
    let diff_stat = raw_stat.as_deref().and_then(format_shortstat);
    GitMeta { branch, commit_sha, diff_stat }
}

pub fn latest_commit_summary(dir: &Path) -> Option<(String, GitMeta)> {
    let branch = detect_branch(dir);
    let commit_sha = git_output(dir, &["rev-parse", "--short=7", "HEAD"]).filter(|s| !s.is_empty())?;
    let subject = git_output(dir, &["log", "-1", "--pretty=%s", "HEAD"]).filter(|s| !s.is_empty())?;
    let raw_stat = git_output(dir, &["diff-tree", "--no-commit-id", "--shortstat", "-r", "HEAD"]);
    let diff_stat = raw_stat.as_deref().and_then(format_shortstat);
    Some((subject, GitMeta { branch, commit_sha: Some(commit_sha), diff_stat }))
}

pub fn format_shortstat(raw: &str) -> Option<String> {
    let mut files: Option<u64> = None;
    let mut ins: u64 = 0;
    let mut del: u64 = 0;
    for part in raw.split(',') {
        let trimmed = part.trim();
        let num = trimmed.split_whitespace().next()?.parse::<u64>().ok()?;
        if trimmed.contains("file") {
            files = Some(num);
        } else if trimmed.contains("insertion") {
            ins = num;
        } else if trimmed.contains("deletion") {
            del = num;
        }
    }
    let files = files?;
    if files == 0 && ins == 0 && del == 0 {
        return None;
    }
    Some(format!("+{ins}/-{del} · {files}f"))
}

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("--no-optional-locks")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

const HOOK_START: &str = "# >>> feeder-hook >>>";
const HOOK_END: &str = "# <<< feeder-hook <<<";

pub fn install_post_commit_hook(dir: &Path, feeder_bin: &Path) -> crate::Result<PathBuf> {
    let hooks_dir = git_output(dir, &["rev-parse", "--git-path", "hooks"])
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { dir.join(p) })
        .ok_or_else(|| crate::Error::Invalid(format!("{} is not a git repository", dir.display())))?;
    std::fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("post-commit");
    let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();
    let cleaned = strip_hook_block(&existing);
    let bin = feeder_bin.display().to_string().replace('\'', "'\\''");
    let snippet = format!(
        "{HOOK_START}\nif command -v '{bin}' >/dev/null 2>&1; then\n  '{bin}' hook-post-commit >/dev/null 2>&1 &\nfi\n{HOOK_END}\n"
    );
    let mut next = if cleaned.trim().is_empty() {
        format!("#!/bin/sh\n{snippet}")
    } else {
        let mut s = cleaned.trim_end().to_string();
        s.push_str("\n\n");
        s.push_str(&snippet);
        s
    };
    if !next.starts_with("#!") {
        next = format!("#!/bin/sh\n{next}");
    }
    std::fs::write(&hook_path, next)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&hook_path, perms)?;
    }
    Ok(hook_path)
}

pub fn remove_post_commit_hook(dir: &Path) -> crate::Result<bool> {
    let Some(hooks_rel) = git_output(dir, &["rev-parse", "--git-path", "hooks"]) else {
        return Err(crate::Error::Invalid(format!("{} is not a git repository", dir.display())));
    };
    let hooks_dir = {
        let p = PathBuf::from(hooks_rel);
        if p.is_absolute() { p } else { dir.join(p) }
    };
    let hook_path = hooks_dir.join("post-commit");
    let Ok(existing) = std::fs::read_to_string(&hook_path) else {
        return Ok(false);
    };
    if !existing.contains(HOOK_START) {
        return Ok(false);
    }
    let cleaned = strip_hook_block(&existing);
    if cleaned.trim().is_empty() || cleaned.trim() == "#!/bin/sh" {
        std::fs::remove_file(&hook_path)?;
    } else {
        std::fs::write(&hook_path, cleaned)?;
    }
    Ok(true)
}

fn strip_hook_block(content: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line.trim() == HOOK_START {
            skipping = true;
            continue;
        }
        if line.trim() == HOOK_END {
            skipping = false;
            continue;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_stable_keys() {
        assert_eq!(slugify("My Cool_Project!"), "my-cool-project");
        assert_eq!(slugify("  "), "untitled");
        assert_eq!(slugify("feeder"), "feeder");
    }

    #[test]
    fn a_nested_directory_resolves_to_its_git_root() {
        let base = std::env::temp_dir().join(format!("feeder-detect-{}", std::process::id()));
        let nested = base.join("Proj").join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(base.join("Proj").join(".git")).unwrap();
        std::fs::write(base.join("Proj").join(".git").join("HEAD"), "ref: refs/heads/feat/fts5\n").unwrap();
        let found = detect_project(&nested);
        assert_eq!(found.name, "Proj");
        assert!(found.path.ends_with("Proj"));
        assert_eq!(detect_branch(&nested).as_deref(), Some("feat/fts5"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_linked_worktree_resolves_to_its_main_checkout() {
        let base = std::env::temp_dir().join(format!("feeder-worktree-{}", std::process::id()));
        let main = base.join("spec-refinery");
        let wt = base.join("spec-refinery-tal-249");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("tal-249")).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let gitdir = main.join(".git/worktrees/tal-249");
        std::fs::write(gitdir.join("HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        let found = detect_project(&wt.join("src"));
        assert_eq!(found.name, "spec-refinery");
        assert_eq!(found.path, main.canonicalize().unwrap());
        assert_eq!(detect_branch(&wt.join("src")).as_deref(), Some("0123456"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn shortstat_parses_git_output_cleanly() {
        assert_eq!(format_shortstat(" 4 files changed, 42 insertions(+), 18 deletions(-)").as_deref(), Some("+42/-18 · 4f"));
        assert_eq!(format_shortstat(" 1 file changed, 7 insertions(+)").as_deref(), Some("+7/-0 · 1f"));
        assert_eq!(format_shortstat(""), None);
    }

    #[test]
    fn hook_strip_preserves_existing_commands() {
        let raw = "#!/bin/sh\necho before\n# >>> feeder-hook >>>\nfeeder hook-post-commit\n# <<< feeder-hook <<<\necho after\n";
        assert_eq!(strip_hook_block(raw), "#!/bin/sh\necho before\necho after\n");
    }
}
