use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

const MAX_TEXT_BYTES: u64 = 512 * 1024;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum FilePreview {
    Text(String),
    Image(Vec<u8>),
    Unsupported,
    TooLarge,
}

pub fn read_file_preview(root: &Path, path: &Path) -> io::Result<FilePreview> {
    let mut file = open_project_file(root, path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(FilePreview::Unsupported);
    }

    let mut bytes = Vec::new();
    (&mut file).take(16).read_to_end(&mut bytes)?;
    let image = is_preview_image(&bytes);
    let limit = if image { MAX_IMAGE_BYTES } else { MAX_TEXT_BYTES };
    if metadata.len() > limit {
        return Ok(FilePreview::TooLarge);
    }
    file.take(limit + 1 - bytes.len() as u64).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(FilePreview::TooLarge);
    }
    if image {
        return Ok(FilePreview::Image(bytes));
    }

    let Ok(text) = String::from_utf8(bytes) else { return Ok(FilePreview::Unsupported) };
    if text.chars().any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n' | '\u{000c}')) {
        return Ok(FilePreview::Unsupported);
    }
    Ok(FilePreview::Text(text))
}

fn is_preview_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn open_project_file(root: &Path, path: &Path) -> io::Result<File> {
    let mut dir = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW).open(root)?;
    let mut parts = path.components().peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Expected a project-relative file path"));
        };
        let name = CString::new(name.as_bytes())?;
        let is_dir = parts.peek().is_some();
        let next = open_component(dir.as_fd(), &name, is_dir)?;
        if !is_dir {
            return Ok(next);
        }
        dir = next;
    }
    Err(io::Error::new(io::ErrorKind::InvalidInput, "Expected a file path"))
}

fn open_component(directory: BorrowedFd<'_>, name: &std::ffi::CStr, is_dir: bool) -> io::Result<File> {
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if is_dir {
        flags |= libc::O_DIRECTORY;
    }
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("feeder-preview-{}-{}", std::process::id(), fastrand::u64(..)));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn previews_text_image_and_unsupported_content() {
        let dir = TempDir::new();
        let normal = "README with spaces.md";
        fs::write(dir.0.join(normal), "# File\n\nLine two\n").unwrap();
        fs::write(dir.0.join("picture"), b"\x89PNG\r\n\x1a\n\0").unwrap();
        fs::write(dir.0.join("binary"), b"abc\0xyz").unwrap();
        assert_eq!(read_file_preview(&dir.0, Path::new(normal)).unwrap(), FilePreview::Text("# File\n\nLine two\n".into()));
        assert!(matches!(read_file_preview(&dir.0, Path::new("picture")).unwrap(), FilePreview::Image(_)));
        assert_eq!(read_file_preview(&dir.0, Path::new("binary")).unwrap(), FilePreview::Unsupported);
    }

    #[test]
    fn refuses_paths_outside_the_project_and_bounds_large_files() {
        let dir = TempDir::new();
        fs::create_dir(dir.0.join("src")).unwrap();
        fs::write(dir.0.join("src/lib.rs"), b"fn main() {}").unwrap();
        symlink(dir.0.join("src"), dir.0.join("link")).unwrap();
        symlink(dir.0.join("src/lib.rs"), dir.0.join("alias")).unwrap();
        File::create(dir.0.join("large")).unwrap().set_len(MAX_TEXT_BYTES + 1).unwrap();
        assert!(matches!(read_file_preview(&dir.0, Path::new("src/lib.rs")).unwrap(), FilePreview::Text(_)));
        assert_eq!(read_file_preview(&dir.0, Path::new("large")).unwrap(), FilePreview::TooLarge);
        for path in ["link/lib.rs", "alias", "../lib.rs", "/lib.rs", ""] {
            assert!(read_file_preview(&dir.0, Path::new(path)).is_err(), "{path}");
        }
    }
}
