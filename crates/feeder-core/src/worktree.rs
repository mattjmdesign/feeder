use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::UNIX_EPOCH;

use crate::now_ms;

#[derive(Clone, Debug, PartialEq)]
pub struct WorkingTree {
    pub status: TreeStatus,
    pub root: Option<PathBuf>,
    pub branch: Option<String>,
    pub files: Vec<String>,
    pub changes: Vec<FileChange>,
    pub last_commit: Option<Commit>,
    pub read_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TreeStatus {
    Ready,
    NoFolder,
    Missing,
    NotGit,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    pub modified_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChangeKind {
    Edited,
    New,
    Deleted,
    Renamed { from: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub subject: String,
    pub at: i64,
}

pub fn read_working_tree(path: Option<&Path>) -> WorkingTree {
    let mut tree = WorkingTree {
        status: TreeStatus::NoFolder,
        root: path.map(Path::to_path_buf),
        branch: None,
        files: Vec::new(),
        changes: Vec::new(),
        last_commit: None,
        read_at: 0,
    };
    if let Some(path) = path {
        tree.status = match fs::canonicalize(path) {
            Ok(root) if root.is_dir() => {
                tree.root = Some(root.clone());
                load(&root, &mut tree).unwrap_or_else(TreeStatus::Failed)
            }
            Ok(_) => TreeStatus::Missing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => TreeStatus::Missing,
            Err(e) => TreeStatus::Failed(format!("Couldn't access the folder: {e}")),
        };
    }
    tree.read_at = now_ms();
    tree
}

fn load(folder: &Path, tree: &mut WorkingTree) -> Result<TreeStatus, String> {
    let top = git(folder, &["rev-parse", "--show-toplevel"])?;
    if !top.status.success() {
        let error = top.stderr.as_slice();
        if error.starts_with(b"fatal: not a git repository") || error.starts_with(b"fatal: this operation must be run in a work tree") {
            tree.files = walk(folder).map_err(|e| format!("Couldn't list the folder: {e}"))?;
            return Ok(TreeStatus::NotGit);
        }
        return Err(git_error("rev-parse", &top));
    }
    let root = PathBuf::from(OsStr::from_bytes(top.stdout.strip_suffix(b"\n").unwrap_or(&top.stdout)));
    tree.root = Some(root.clone());

    let listed = git_read(&root, &["ls-files", "-z", "--cached"])?;
    let mut files = fields(&listed).map(path_string).collect::<Result<BTreeSet<_>, _>>()?;
    let status = git_read(&root, &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all", "--renames"])?;
    let mut entries = fields(&status);
    let head = entries.next().and_then(|e| e.strip_prefix(b"## ")).ok_or("Git status omitted the branch header")?;
    let head = String::from_utf8_lossy(head);
    let unborn = head.strip_prefix("No commits yet on ").or_else(|| head.strip_prefix("Initial commit on "));
    let branch = if head == "HEAD (no branch)" {
        None
    } else {
        Some(unborn.unwrap_or_else(|| head.split_once("...").map_or(&head, |(branch, _)| branch)).to_string())
    };

    let tracked_changes = entries.clone().any(|e| !e.starts_with(b"?? "));
    let diff = if unborn.is_some() || !tracked_changes {
        Vec::new()
    } else {
        git_read(&root, &["diff", "HEAD", "--numstat", "-z", "-M", "--no-ext-diff", "--no-textconv", "--"])?
    };
    let counts = numstat(&diff)?;
    let last_commit = unborn.is_none().then(|| last_commit(&root)).transpose()?;
    let mut changes = Vec::new();
    while let Some(entry) = entries.next() {
        let [x, y, b' ', path @ ..] = entry else { return Err("Couldn't read a Git status entry".into()) };
        let path = path_string(path)?;
        if (*x, *y) == (b'?', b'?') {
            files.insert(path.clone());
        }
        let kind = if [*x, *y].iter().any(|c| matches!(c, b'R' | b'C')) {
            let from = path_string(entries.next().ok_or("Git status omitted a rename source")?)?;
            if *x == b'R' || *y == b'R' { ChangeKind::Renamed { from } } else { ChangeKind::New }
        } else {
            match (*x, *y) {
                (b'?', b'?') => ChangeKind::New,
                (b'A' | b' ' | b'M' | b'T', b'D') | (b'D', b' ') => ChangeKind::Deleted,
                (b'A', b' ' | b'M' | b'T') => ChangeKind::New,
                _ => ChangeKind::Edited,
            }
        };
        let absolute = root.join(&path);
        let (added, removed) = counts.get(path.as_str()).copied().unwrap_or_else(|| {
            let added = if kind == ChangeKind::New { count_lines(&absolute).unwrap_or(0) } else { 0 };
            (added, 0)
        });
        let modified_at = if kind == ChangeKind::Deleted {
            None
        } else {
            fs::symlink_metadata(&absolute)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
        };
        changes.push(FileChange { path, kind, added, removed, modified_at });
    }
    changes.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    tree.branch = branch;
    tree.files = files.into_iter().collect();
    tree.changes = changes;
    tree.last_commit = last_commit;
    Ok(TreeStatus::Ready)
}

fn git(root: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| format!("Couldn't run git {}: {e}", args.first().copied().unwrap_or("")))
}

fn git_read(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = git(root, args)?;
    if out.status.success() { Ok(out.stdout) } else { Err(git_error(args[0], &out)) }
}

fn git_error(command: &str, out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail: String = stderr.lines().next().unwrap_or_default().chars().take(300).collect();
    if detail.is_empty() { format!("git {command} failed ({})", out.status) } else { format!("git {command}: {detail}") }
}

fn last_commit(root: &Path) -> Result<Commit, String> {
    let raw = git_read(root, &["log", "-1", "--format=%ct%x00%s", "--encoding=UTF-8", "--"])?;
    let text = String::from_utf8_lossy(&raw);
    let (seconds, subject) = text.strip_suffix('\n').unwrap_or(&text).split_once('\0').ok_or("Couldn't read the last commit")?;
    let at = seconds.parse::<i64>().map_err(|_| "Couldn't read the last commit time")?.saturating_mul(1_000);
    Ok(Commit { subject: subject.to_string(), at })
}

fn fields(bytes: &[u8]) -> impl Iterator<Item = &[u8]> + Clone {
    bytes.split(|b| *b == 0).filter(|field| !field.is_empty())
}

fn path_string(path: &[u8]) -> Result<String, String> {
    let path = std::str::from_utf8(path).map_err(|_| "Git returned a filename that isn't UTF-8")?;
    let path = path.strip_suffix('/').unwrap_or(path);
    if path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err("Git returned an invalid relative path".into());
    }
    Ok(path.to_owned())
}

fn numstat(bytes: &[u8]) -> Result<HashMap<&str, (u32, u32)>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Git returned a filename that isn't UTF-8")?;
    let mut fields = text.split_terminator('\0');
    let mut counts = HashMap::new();
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("Couldn't read a Git diff entry".into());
        };
        let path = if path.is_empty() {
            fields.next().ok_or("Git diff omitted a rename source")?;
            fields.next().ok_or("Git diff omitted a rename destination")?
        } else {
            path
        };
        counts.insert(path, (added.parse().unwrap_or(0), removed.parse().unwrap_or(0)));
    }
    Ok(counts)
}

fn count_lines(path: &Path) -> io::Result<u32> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(0);
    }
    let mut file = file.take(metadata.len());
    let mut buffer = [0; 8192];
    let mut lines = 0_u32;
    let mut last = b'\n';
    loop {
        let count = file.read(&mut buffer)?;
        let bytes = &buffer[..count];
        let Some(&tail) = bytes.last() else { return Ok(lines.saturating_add(u32::from(last != b'\n'))) };
        if bytes.contains(&0) {
            return Ok(0);
        }
        let newlines = u32::try_from(bytes.iter().filter(|&&byte| byte == b'\n').count()).unwrap_or(u32::MAX);
        lines = lines.saturating_add(newlines);
        last = tail;
    }
}

fn walk(root: &Path) -> io::Result<Vec<String>> {
    const LIMIT: usize = 5000;
    let mut files = Vec::new();
    walk_folder(root, root, &mut files, LIMIT)?;
    files.sort_unstable();
    Ok(files)
}

fn walk_folder(root: &Path, folder: &Path, files: &mut Vec<String>, limit: usize) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let name = entry.file_name();
            if !name.as_bytes().starts_with(b".") && name != "node_modules" && name != "target" {
                walk_folder(root, &path, files, limit)?;
            }
        } else if (kind.is_file() || kind.is_symlink())
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.to_string_lossy().into_owned());
        }
        if files.len() == limit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("feeder-files-{}-{}", std::process::id(), fastrand::u64(..)));
            fs::create_dir(&path).unwrap();
            TestDir(path.canonicalize().unwrap())
        }

        fn write(&self, path: &str, body: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }

        fn git(&self, args: &[&str]) {
            git_read(&self.0, args).unwrap();
        }

        fn init(&self) {
            self.git(&["init", "-q", "-b", "main"]);
        }

        fn commit(&self) {
            self.git(&["add", "."]);
            self.git(&[
                "-c",
                "user.name=Feeder Test",
                "-c",
                "user.email=qa@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "Start",
            ]);
        }

        fn read(&self) -> WorkingTree {
            read_working_tree(Some(&self.0))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn clean_tree_resolves_a_nested_folder() {
        let dir = TestDir::new();
        dir.init();
        dir.write("src/app.rs", "fn main() {}\n");
        dir.commit();
        let tree = read_working_tree(Some(&dir.0.join("src")));
        assert_eq!(tree.status, TreeStatus::Ready);
        assert_eq!(tree.root.as_ref(), Some(&dir.0));
        assert_eq!(tree.branch.as_deref(), Some("main"));
        assert_eq!(tree.files, ["src/app.rs"]);
        assert!(tree.changes.is_empty());
        let commit = tree.last_commit.unwrap();
        assert_eq!(commit.subject, "Start");
        assert!(commit.at > 0 && commit.at <= tree.read_at);
    }

    #[test]
    fn linked_worktree_keeps_its_own_root_branch_and_changes() {
        let main = TestDir::new();
        main.init();
        main.write("src/app.rs", "fn main() {}\n");
        main.commit();
        let linked = TestDir::new();
        main.git(&["worktree", "add", "-q", "-b", "feature", linked.0.to_str().unwrap()]);
        linked.write("src/app.rs", "fn main() { todo!() }\n");

        let tree = read_working_tree(Some(&linked.0.join("src")));
        assert_eq!(tree.status, TreeStatus::Ready);
        assert_eq!(tree.root.as_ref(), Some(&linked.0));
        assert_eq!(tree.branch.as_deref(), Some("feature"));
        assert_eq!(tree.changes[0].path, "src/app.rs");
        assert!(main.read().changes.is_empty());
    }

    #[test]
    fn changes_include_renames_counts_and_literal_paths() {
        let dir = TestDir::new();
        dir.init();
        dir.write(".gitignore", "ignored/\n");
        dir.write("edit.rs", "before\n");
        dir.write("delete.rs", "gone\n");
        dir.write("old name.rs", "aa\nbb\ncc\ndd\n");
        dir.commit();
        dir.write("edit.rs", "after\nmore\n");
        fs::remove_file(dir.0.join("delete.rs")).unwrap();
        dir.git(&["mv", "old name.rs", "new name.rs"]);
        dir.write("new name.rs", "aa\nbb\ncc\ndd\nextra\n");
        dir.write("fresh -> file.md", "two\nlines");
        dir.write("ignored/skip.md", "ignore\n");
        let tree = dir.read();
        assert_eq!(tree.status, TreeStatus::Ready);
        assert!(tree.files.contains(&"fresh -> file.md".to_owned()));
        assert!(!tree.files.iter().any(|p| p == "old name.rs" || p.starts_with("ignored/")));
        let changes: Vec<_> = tree.changes.iter().map(|c| (c.path.as_str(), c.kind.clone(), c.added, c.removed)).collect();
        assert_eq!(
            changes,
            [
                ("delete.rs", ChangeKind::Deleted, 0, 1),
                ("edit.rs", ChangeKind::Edited, 2, 1),
                ("fresh -> file.md", ChangeKind::New, 2, 0),
                ("new name.rs", ChangeKind::Renamed { from: "old name.rs".into() }, 1, 0),
            ]
        );
        assert_eq!(tree.changes[0].modified_at, None);
        assert!(tree.changes[1..].iter().all(|c| c.modified_at.is_some()));
    }

    #[test]
    fn a_repo_without_head_is_ready() {
        let dir = TestDir::new();
        dir.init();
        dir.write("staged.rs", "first\nsecond\n");
        dir.git(&["add", "staged.rs"]);
        dir.write("untracked.rs", "third");
        let tree = dir.read();
        assert_eq!(tree.status, TreeStatus::Ready);
        assert_eq!(tree.branch.as_deref(), Some("main"));
        assert_eq!(tree.last_commit, None);
        assert_eq!(tree.files, ["staged.rs", "untracked.rs"]);
        assert_eq!(tree.changes.iter().map(|c| (&c.kind, c.added)).collect::<Vec<_>>(), [(&ChangeKind::New, 2), (&ChangeKind::New, 1)]);
    }

    #[test]
    fn embedded_repo_is_a_single_untracked_entry() {
        let dir = TestDir::new();
        dir.init();
        dir.write("inner/source.rs", "nested\n");
        git_read(&dir.0.join("inner"), &["init", "-q", "-b", "main"]).unwrap();
        let tree = dir.read();
        assert_eq!(tree.status, TreeStatus::Ready);
        assert_eq!(tree.files, ["inner"]);
        assert_eq!((tree.changes[0].kind.clone(), tree.changes[0].added), (ChangeKind::New, 0));
    }

    #[test]
    fn no_folder_missing_and_not_git_are_distinct() {
        let dir = TestDir::new();
        assert_eq!(read_working_tree(None).status, TreeStatus::NoFolder);
        assert_eq!(read_working_tree(Some(&dir.0.join("missing"))).status, TreeStatus::Missing);
        dir.write("src/info.md", "hello\n");
        dir.write(".hidden/skip", "");
        dir.write("node_modules/skip", "");
        dir.write("src/target/skip", "");
        let tree = dir.read();
        assert_eq!(tree.status, TreeStatus::NotGit);
        assert_eq!(tree.root.as_ref(), Some(&dir.0));
        assert_eq!(tree.files, ["src/info.md"]);
    }

    #[test]
    fn git_read_errors_do_not_publish_an_empty_ready_tree() {
        let dir = TestDir::new();
        dir.init();
        dir.write(".git/index", "broken index");
        let tree = dir.read();
        assert!(matches!(tree.status, TreeStatus::Failed(ref reason) if reason.contains("ls-files")));
    }
}
