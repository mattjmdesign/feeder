use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{DetectedProject, Error, Orb, Result, now_ms, parse_attention, slugify, validate_body};

const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_VIDEO_BYTES: u64 = 50 * 1024 * 1024;
const MAX_BRIEF_CHARS: usize = 600;
const MAX_REPLY_CHARS: usize = 900;
pub const MAX_TODO_CHARS: usize = 240;
pub const MAX_MEDIA_PER_UPDATE: usize = 8;
const IMAGE_TYPES: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
const VIDEO_TYPES: [&str; 4] = ["mp4", "mov", "webm", "m4v"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    pub id: i64,
    pub update_id: i64,
    pub position: usize,
    pub path: PathBuf,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub caption: Option<String>,
}

impl MediaAttachment {
    pub fn kind(&self) -> Option<MediaKind> {
        media_kind_for_path(&self.path)
    }

    pub fn is_image(&self) -> bool {
        self.kind() == Some(MediaKind::Image)
    }

    pub fn is_video(&self) -> bool {
        self.kind() == Some(MediaKind::Video)
    }
}

pub fn media_kind_for_path(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if IMAGE_TYPES.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_TYPES.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = File::open(path).ok()?;
    image_dimensions_from_reader(file)
}

fn image_dimensions_from_reader<R: Read + Seek>(mut r: R) -> Option<(u32, u32)> {
    let mut head = [0u8; 32];
    let n = r.read(&mut head).ok()?;
    if n >= 24 && &head[0..8] == b"\x89PNG\r\n\x1a\n" && &head[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
        let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
        return (w > 0 && h > 0).then_some((w, h));
    }
    if n >= 10 && (&head[0..6] == b"GIF87a" || &head[0..6] == b"GIF89a") {
        let w = u32::from(u16::from_le_bytes([head[6], head[7]]));
        let h = u32::from(u16::from_le_bytes([head[8], head[9]]));
        return (w > 0 && h > 0).then_some((w, h));
    }
    if n >= 16 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        match &head[12..16] {
            b"VP8 " if n >= 30 && head[23..26] == [0x9d, 0x01, 0x2a] => {
                let w = u32::from(u16::from_le_bytes([head[26], head[27]]) & 0x3fff);
                let h = u32::from(u16::from_le_bytes([head[28], head[29]]) & 0x3fff);
                return (w > 0 && h > 0).then_some((w, h));
            }
            b"VP8L" if n >= 25 && head[20] == 0x2f => {
                let bits = u32::from_le_bytes([head[21], head[22], head[23], head[24]]);
                let w = (bits & 0x3fff) + 1;
                let h = ((bits >> 14) & 0x3fff) + 1;
                return (w > 0 && h > 0).then_some((w, h));
            }
            b"VP8X" if n >= 30 => {
                let w = 1 + (u32::from(head[24]) | (u32::from(head[25]) << 8) | (u32::from(head[26]) << 16));
                let h = 1 + (u32::from(head[27]) | (u32::from(head[28]) << 8) | (u32::from(head[29]) << 16));
                return (w > 0 && h > 0).then_some((w, h));
            }
            _ => {}
        }
    }
    if n >= 2 && head[0] == 0xff && head[1] == 0xd8 {
        r.seek(SeekFrom::Start(2)).ok()?;
        return jpeg_dimensions(&mut r);
    }
    None
}

fn jpeg_dimensions<R: Read + Seek>(r: &mut R) -> Option<(u32, u32)> {
    let mut b = [0u8; 1];
    loop {
        loop {
            r.read_exact(&mut b).ok()?;
            if b[0] == 0xff {
                break;
            }
        }
        let mut marker = 0xff;
        while marker == 0xff {
            r.read_exact(&mut b).ok()?;
            marker = b[0];
        }
        match marker {
            0x00 | 0x01 | 0xd0..=0xd7 => continue,
            0xd8..=0xda => return None,
            _ => {}
        }
        let mut len_buf = [0u8; 2];
        r.read_exact(&mut len_buf).ok()?;
        let seg_len = u16::from_be_bytes(len_buf);
        if seg_len < 2 {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if seg_len < 7 {
                return None;
            }
            let mut sof = [0u8; 5];
            r.read_exact(&mut sof).ok()?;
            let h = u32::from(u16::from_be_bytes([sof[1], sof[2]]));
            let w = u32::from(u16::from_be_bytes([sof[3], sof[4]]));
            return (w > 0 && h > 0).then_some((w, h));
        }
        r.seek(SeekFrom::Current(i64::from(seg_len) - 2)).ok()?;
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FEEDER_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("feeder")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub path: Option<PathBuf>,
    pub orb: Orb,
    pub created_at: i64,
    pub archived: bool,
    pub seen_at: Option<i64>,
    pub pinned: bool,
    pub brief: Option<String>,
}

impl Project {
    pub fn folder(&self) -> Option<String> {
        self.path.as_ref()?.file_name().map(|n| n.to_string_lossy().into_owned())
    }

    pub fn renamed(&self) -> bool {
        self.folder().is_some_and(|f| slugify(&f) != slugify(&self.name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub id: i64,
    pub update_id: i64,
    pub project_id: i64,
    pub author: String,
    pub from_human: bool,
    pub body: String,
    pub created_at: i64,
    pub read_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pulse {
    pub project_id: i64,
    pub agent: String,
    pub branch: Option<String>,
    pub status: String,
    pub updated_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub id: i64,
    pub project_id: i64,
    pub title: String,
    pub done: bool,
    pub position: i64,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSummary {
    pub project: Project,
    pub updates: i64,
    pub last_update_id: Option<i64>,
    pub last_at: Option<i64>,
    pub last_agent: Option<String>,
    pub last_via: Option<String>,
    pub last_body: Option<String>,
    pub needs_you: bool,
    pub unread_replies: i64,
    pub open_todos: i64,
    pub active_pulse: Option<Pulse>,
}

impl ProjectSummary {
    pub fn unseen(&self) -> bool {
        !self.project.archived && self.last_at.is_some_and(|at| self.project.seen_at.is_none_or(|seen| at > seen))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedUpdate {
    pub id: i64,
    pub project_id: i64,
    pub agent: String,
    pub body: String,
    pub screenshot: Option<PathBuf>,
    pub media_w: Option<u32>,
    pub media_h: Option<u32>,
    pub media: Vec<MediaAttachment>,
    pub branch: Option<String>,
    pub created_at: i64,
    pub diff_stat: Option<String>,
    pub commit_sha: Option<String>,
    pub attention_dismissed_at: Option<i64>,
    pub via: Option<String>,
}

impl FeedUpdate {
    pub fn media_kind(&self) -> Option<MediaKind> {
        self.media.first().and_then(MediaAttachment::kind).or_else(|| self.screenshot.as_deref().and_then(media_kind_for_path))
    }

    pub fn has_image(&self) -> bool {
        if !self.media.is_empty() { self.media.iter().any(MediaAttachment::is_image) } else { self.media_kind() == Some(MediaKind::Image) }
    }

    pub fn has_video(&self) -> bool {
        if !self.media.is_empty() { self.media.iter().any(MediaAttachment::is_video) } else { self.media_kind() == Some(MediaKind::Video) }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewUpdate {
    pub agent: String,
    pub body: String,
    pub screenshot: Option<PathBuf>,
    pub screenshots: Vec<PathBuf>,
    pub branch: Option<String>,
    pub diff_stat: Option<String>,
    pub commit_sha: Option<String>,
}

const PROJECT_COLS: &str = "id, slug, name, path, orb, created_at, archived, seen_at, pinned, brief";
const UPDATE_COLS: &str =
    "id, project_id, agent, body, screenshot, media_w, media_h, branch, created_at, diff_stat, commit_sha, attention_dismissed_at, via";
const UPDATE_U_COLS: &str = "u.id, u.project_id, u.agent, u.body, u.screenshot, u.media_w, u.media_h, u.branch, u.created_at, u.diff_stat, u.commit_sha, u.attention_dismissed_at, u.via";

pub struct Store {
    pub(crate) conn: Connection,
    pub(crate) dir: PathBuf,
}

impl Store {
    pub fn open_default() -> Result<Store> {
        Store::open(&data_dir())
    }

    pub fn open(dir: &Path) -> Result<Store> {
        let media = dir.join("media");
        std::fs::create_dir_all(&media)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o700));
        }
        let conn = Connection::open(dir.join("feeder.db"))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                path TEXT UNIQUE,
                orb TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0,
                seen_at INTEGER,
                pinned INTEGER NOT NULL DEFAULT 0,
                brief TEXT
            );
            CREATE TABLE IF NOT EXISTS updates (
                id INTEGER PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent TEXT NOT NULL,
                body TEXT NOT NULL,
                screenshot TEXT,
                media_w INTEGER,
                media_h INTEGER,
                branch TEXT,
                created_at INTEGER NOT NULL,
                diff_stat TEXT,
                commit_sha TEXT,
                attention_dismissed_at INTEGER,
                via TEXT
            );
            CREATE INDEX IF NOT EXISTS updates_by_project ON updates(project_id, created_at DESC, id DESC);
            CREATE TABLE IF NOT EXISTS update_media (
                id INTEGER PRIMARY KEY,
                update_id INTEGER NOT NULL REFERENCES updates(id) ON DELETE CASCADE,
                position INTEGER NOT NULL DEFAULT 0,
                path TEXT NOT NULL,
                width INTEGER,
                height INTEGER,
                caption TEXT
            );
            CREATE INDEX IF NOT EXISTS update_media_by_update ON update_media(update_id, position ASC, id ASC);
            CREATE TABLE IF NOT EXISTS replies (
                id INTEGER PRIMARY KEY,
                update_id INTEGER NOT NULL REFERENCES updates(id) ON DELETE CASCADE,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                author TEXT NOT NULL,
                from_human INTEGER NOT NULL DEFAULT 1,
                body TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                read_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS replies_by_update ON replies(update_id, created_at ASC, id ASC);
            CREATE INDEX IF NOT EXISTS replies_unread ON replies(project_id, read_at);
            CREATE TABLE IF NOT EXISTS pulses (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent TEXT NOT NULL,
                branch TEXT,
                status TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY (project_id, agent)
            );
            CREATE TABLE IF NOT EXISTS todos (
                id INTEGER PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                done INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                completed_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS todos_by_project ON todos(project_id, position ASC, id ASC);",
        )?;
        migrate_projects(&conn)?;
        migrate_updates(&conn, dir)?;
        crate::notebook::migrate(&conn)?;
        crate::knowledge::migrate(&conn)?;
        crate::gardener::migrate(&conn)?;
        Ok(Store { conn, dir: dir.to_path_buf() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn data_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    pub fn notebook_stamp(&self) -> Result<(i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(updated_at), 0), COALESCE(SUM(helped), 0), COALESCE(SUM(standing = 'archived'), 0) FROM notebook_pages",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }

    pub fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let now = now_ms();
        let mut pulse_map: HashMap<i64, Pulse> = HashMap::new();
        for pulse in self.active_pulses_at(now)? {
            pulse_map.entry(pulse.project_id).or_insert(pulse);
        }
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.slug, p.name, p.path, p.orb, p.created_at, p.archived, p.seen_at, p.pinned, p.brief,
                    (SELECT COUNT(*) FROM updates u WHERE u.project_id = p.id),
                    l.id, l.created_at, l.agent, l.body, l.attention_dismissed_at,
                    CASE WHEN l.id IS NOT NULL AND EXISTS(
                        SELECT 1 FROM replies r WHERE r.update_id = l.id AND r.from_human = 1
                    ) THEN 1 ELSE 0 END,
                    (SELECT COUNT(*) FROM replies r WHERE r.project_id = p.id AND r.from_human = 1 AND r.read_at IS NULL),
                    (SELECT COUNT(*) FROM todos t WHERE t.project_id = p.id AND t.done = 0),
                    l.via
             FROM projects p
             LEFT JOIN updates l ON l.id = (
                SELECT id FROM updates WHERE project_id = p.id ORDER BY created_at DESC, id DESC LIMIT 1)
             ORDER BY p.pinned DESC, COALESCE(l.created_at, p.created_at) DESC, l.id DESC, p.id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let project = project_row(r)?;
            let updates: i64 = r.get(10)?;
            let last_update_id: Option<i64> = r.get(11)?;
            let last_at: Option<i64> = r.get(12)?;
            let last_agent: Option<String> = r.get(13)?;
            let last_body: Option<String> = r.get(14)?;
            let last_dismissed_at: Option<i64> = r.get(15)?;
            let last_answered: i64 = r.get(16)?;
            let unread_replies: i64 = r.get(17)?;
            let open_todos: i64 = r.get(18)?;
            let last_via: Option<String> = r.get(19)?;
            let needs_you = !project.archived
                && last_answered == 0
                && last_dismissed_at.is_none()
                && last_body.as_deref().is_some_and(|b| parse_attention(b).is_some());
            let active_pulse = pulse_map.remove(&project.id);
            Ok(ProjectSummary {
                project,
                updates,
                last_update_id,
                last_at,
                last_agent,
                last_via,
                last_body,
                needs_you,
                unread_replies,
                open_todos,
                active_pulse,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn project(&self, id: i64) -> Result<Option<Project>> {
        self.one(&format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1"), params![id])
    }

    pub fn find_project(&self, query: &str) -> Result<Option<Project>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let slug = slugify(trimmed);
        let found = self.one(
            &format!(
                "SELECT {PROJECT_COLS} FROM projects
                 WHERE slug = ?1 OR lower(name) = lower(?2)
                    OR lower(path) = lower(?2)
                    OR (length(?2) > 0 AND length(path) > length(?2) AND substr(lower(path), -length(?2) - 1) = '/' || lower(?2))
                 ORDER BY slug = ?1 DESC, lower(name) = lower(?2) DESC LIMIT 1"
            ),
            params![slug, trimmed],
        )?;
        if found.is_some() {
            return Ok(found);
        }
        match trimmed.parse::<i64>() {
            Ok(id) => self.project(id),
            Err(_) => Ok(None),
        }
    }

    pub fn project_named(&self, name: &str, path: Option<&Path>) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("a project needs a name".into()));
        }
        if let Some(p) = self.find_project(name)? {
            if p.path.is_none()
                && let Some(path) = path
                && self.project_at_path(path)?.is_none()
            {
                self.conn.execute("UPDATE projects SET path = ?1 WHERE id = ?2", params![path_str(path), p.id])?;
                return Ok(Project { path: Some(path.to_path_buf()), ..p });
            }
            return Ok(p);
        }
        self.create_project(name, path)
    }

    pub fn project_at(&self, detected: &DetectedProject) -> Result<Project> {
        if let Some(p) = self.project_at_path(&detected.path)? {
            return Ok(p);
        }
        if let Some(p) = self.find_project(&detected.name)?
            && p.path.is_none()
        {
            self.conn.execute("UPDATE projects SET path = ?1 WHERE id = ?2", params![path_str(&detected.path), p.id])?;
            return Ok(Project { path: Some(detected.path.clone()), ..p });
        }
        self.create_project(&detected.name, Some(&detected.path))
    }

    pub fn find_detected(&self, detected: &DetectedProject) -> Result<Option<Project>> {
        if let Some(p) = self.project_at_path(&detected.path)? {
            return Ok(Some(p));
        }
        Ok(self.find_project(&detected.name)?.filter(|p| p.path.is_none()))
    }

    fn project_at_path(&self, path: &Path) -> Result<Option<Project>> {
        self.one(&format!("SELECT {PROJECT_COLS} FROM projects WHERE path = ?1"), params![path_str(path)])
    }

    fn create_project(&self, name: &str, path: Option<&Path>) -> Result<Project> {
        let base = slugify(name);
        let mut slug = base.clone();
        let mut n = 2;
        while self.one(&format!("SELECT {PROJECT_COLS} FROM projects WHERE slug = ?1"), params![slug])?.is_some() {
            slug = format!("{base}-{n}");
            n += 1;
        }
        let existing: Vec<Orb> = {
            let mut stmt = self.conn.prepare("SELECT orb FROM projects")?;
            let rows = stmt.query_map([], |r| {
                let raw: String = r.get(0)?;
                Ok(serde_json::from_str::<Orb>(&raw).ok())
            })?;
            rows.filter_map(|r| r.ok().flatten()).collect()
        };
        let orb = Orb::generate(&existing);
        let created_at = now_ms();
        let orb_json = serde_json::to_string(&orb).map_err(|e| Error::Invalid(format!("orb serialization failed: {e}")))?;
        self.conn.execute(
            "INSERT INTO projects (slug, name, path, orb, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![slug, name, path.map(path_str), orb_json, created_at],
        )?;
        Ok(Project {
            id: self.conn.last_insert_rowid(),
            slug,
            name: name.to_string(),
            path: path.map(Path::to_path_buf),
            orb,
            created_at,
            archived: false,
            seen_at: None,
            pinned: false,
            brief: None,
        })
    }

    pub fn rename_project(&self, id: i64, name: &str) -> Result<()> {
        let project = self.project(id)?.ok_or_else(|| Error::NotFound(format!("no project {id}")))?;
        let folder = project.folder();
        let name = match name.trim() {
            "" => folder.as_deref().unwrap_or(&project.slug),
            n => n,
        };
        self.conn.execute("UPDATE projects SET name = ?1 WHERE id = ?2", params![name, id])?;
        Ok(())
    }

    pub fn set_project_brief(&self, id: i64, brief: Option<&str>) -> Result<()> {
        let cleaned = brief.map(str::trim).filter(|s| !s.is_empty());
        if let Some(b) = cleaned {
            let chars = b.chars().count();
            if chars > MAX_BRIEF_CHARS {
                return Err(Error::Invalid(format!("brief is {chars} characters; keep it to {MAX_BRIEF_CHARS} or fewer")));
            }
            reject_secret(b)?;
        }
        let changed = self.conn.execute("UPDATE projects SET brief = ?1 WHERE id = ?2", params![cleaned, id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no project {id}")));
        }
        Ok(())
    }

    pub fn set_archived(&self, id: i64, archived: bool) -> Result<()> {
        let changed = self.conn.execute("UPDATE projects SET archived = ?1 WHERE id = ?2", params![i64::from(archived), id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no project {id}")));
        }
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        let changed = self.conn.execute("UPDATE projects SET pinned = ?1 WHERE id = ?2", params![i64::from(pinned), id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no project {id}")));
        }
        Ok(())
    }

    pub fn mark_seen(&self, id: i64) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE projects
             SET seen_at = MAX(COALESCE((SELECT MAX(created_at) FROM updates WHERE project_id = ?1), 0), ?2)
             WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn mark_all_seen(&self) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE projects
             SET seen_at = MAX(COALESCE((SELECT MAX(created_at) FROM updates WHERE project_id = projects.id), 0), ?1)
             WHERE archived = 0",
            params![now],
        )?;
        Ok(())
    }

    pub fn dismiss_attention(&self, update_id: i64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE updates SET attention_dismissed_at = COALESCE(attention_dismissed_at, ?1) WHERE id = ?2",
            params![now_ms(), update_id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no update {update_id}")));
        }
        Ok(())
    }

    pub fn undismiss_attention(&self, update_id: i64) -> Result<()> {
        let changed = self.conn.execute("UPDATE updates SET attention_dismissed_at = NULL WHERE id = ?1", params![update_id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no update {update_id}")));
        }
        Ok(())
    }

    pub fn dismiss_project_attention(&self, project_id: i64) -> Result<bool> {
        let latest_id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM updates WHERE project_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1", params![project_id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(id) = latest_id else {
            return Ok(false);
        };
        self.dismiss_attention(id)?;
        Ok(true)
    }

    pub fn delete_update(&self, id: i64) -> Result<()> {
        let shot: Option<Option<String>> =
            self.conn.query_row("SELECT screenshot FROM updates WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
        let Some(shot) = shot else {
            return Err(Error::NotFound(format!("no update {id}")));
        };
        let extra_paths: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT path FROM update_media WHERE update_id = ?1")?;
            let rows = stmt.query_map(params![id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut to_remove = std::collections::HashSet::new();
        if let Some(rel) = shot {
            to_remove.insert(rel);
        }
        for rel in extra_paths {
            to_remove.insert(rel);
        }
        for rel in to_remove {
            std::fs::remove_file(self.dir.join(rel)).ok();
        }
        self.conn.execute("DELETE FROM updates WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn remove_project(&self, id: i64) -> Result<()> {
        for u in self.feed(id, usize::MAX, None)? {
            let mut seen = std::collections::HashSet::new();
            if let Some(shot) = u.screenshot {
                seen.insert(shot);
            }
            for item in u.media {
                seen.insert(item.path);
            }
            for path in seen {
                std::fs::remove_file(path).ok();
            }
        }
        self.conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn post(&self, project_id: i64, update: NewUpdate) -> Result<FeedUpdate> {
        let body = validate_body(&update.body)?;
        reject_secret(&body)?;
        let agent = update.agent.trim();
        let agent = if agent.is_empty() { "Unknown agent" } else { agent };
        let branch = update.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(str::to_string);
        let diff_stat = update.diff_stat.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let commit_sha = update.commit_sha.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let project = self.project(project_id)?.ok_or_else(|| Error::NotFound(format!("no project {project_id}")))?;
        let created_at = now_ms();

        let mut sources: Vec<PathBuf> = Vec::new();
        if let Some(src) = update.screenshot {
            sources.push(src);
        }
        sources.extend(update.screenshots);
        if sources.len() > MAX_MEDIA_PER_UPDATE {
            return Err(Error::Invalid(format!("an update can attach at most {MAX_MEDIA_PER_UPDATE} media files (got {})", sources.len())));
        }

        let mut stored_items: Vec<(String, Option<u32>, Option<u32>)> = Vec::with_capacity(sources.len());
        for src in &sources {
            match self.store_media(&project, src, created_at) {
                Ok(rel) => {
                    let full = self.dir.join(&rel);
                    let (w, h) = if media_kind_for_path(&full) == Some(MediaKind::Image) {
                        image_dimensions(&full).map(|(w, h)| (Some(w), Some(h))).unwrap_or((None, None))
                    } else {
                        (None, None)
                    };
                    stored_items.push((rel, w, h));
                }
                Err(e) => {
                    for (rel, _, _) in &stored_items {
                        std::fs::remove_file(self.dir.join(rel)).ok();
                    }
                    return Err(e);
                }
            }
        }

        let primary_rel = stored_items.first().map(|(rel, _, _)| rel.clone());
        let (media_w, media_h) = stored_items.first().map(|(_, w, h)| (*w, *h)).unwrap_or((None, None));

        let via = crate::agent::host_from_env().filter(|host| !host.eq_ignore_ascii_case(agent));
        let tx_res: rusqlite::Result<(i64, Vec<MediaAttachment>)> = (|| {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO updates (project_id, agent, body, screenshot, media_w, media_h, branch, created_at, diff_stat, commit_sha, via)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    project_id,
                    agent,
                    body,
                    primary_rel,
                    media_w.map(i64::from),
                    media_h.map(i64::from),
                    branch,
                    created_at,
                    diff_stat,
                    commit_sha,
                    via
                ],
            )?;
            let update_id = tx.last_insert_rowid();
            let mut media = Vec::with_capacity(stored_items.len());
            for (pos, (rel, w, h)) in stored_items.iter().enumerate() {
                tx.execute(
                    "INSERT INTO update_media (update_id, position, path, width, height, caption)
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                    params![update_id, pos as i64, rel, w.map(i64::from), h.map(i64::from)],
                )?;
                media.push(MediaAttachment {
                    id: tx.last_insert_rowid(),
                    update_id,
                    position: pos,
                    path: self.dir.join(rel),
                    width: *w,
                    height: *h,
                    caption: None,
                });
            }
            tx.commit()?;
            Ok((update_id, media))
        })();

        let (update_id, media) = match tx_res {
            Ok(v) => v,
            Err(e) => {
                for (rel, _, _) in &stored_items {
                    std::fs::remove_file(self.dir.join(rel)).ok();
                }
                return Err(e.into());
            }
        };
        let _ = self.clear_pulse(project_id, agent);
        Ok(FeedUpdate {
            id: update_id,
            project_id,
            agent: agent.to_string(),
            body,
            screenshot: primary_rel.map(|rel| self.dir.join(rel)),
            media_w,
            media_h,
            media,
            branch,
            created_at,
            diff_stat,
            commit_sha,
            attention_dismissed_at: None,
            via: via.map(str::to_owned),
        })
    }

    pub fn update(&self, id: i64) -> Result<Option<FeedUpdate>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {UPDATE_COLS} FROM updates WHERE id = ?1"))?;
        let mut found = stmt.query_row(params![id], |r| self.update_row(r)).optional()?;
        if let Some(u) = found.as_mut() {
            self.attach_media(std::slice::from_mut(u))?;
        }
        Ok(found)
    }

    pub fn has_commit(&self, project_id: i64, commit_sha: &str) -> Result<bool> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM updates WHERE project_id = ?1 AND commit_sha = ?2)",
            params![project_id, commit_sha],
            |r| r.get(0),
        )?;
        Ok(exists)
    }

    pub fn edit_update(&self, id: i64, body: &str) -> Result<FeedUpdate> {
        let body = validate_body(body)?;
        reject_secret(&body)?;
        let changed = self.conn.execute("UPDATE updates SET body = ?1 WHERE id = ?2", params![body, id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no update {id}")));
        }
        self.update(id)?.ok_or_else(|| Error::NotFound(format!("no update {id}")))
    }

    pub fn feed(&self, project_id: i64, limit: usize, before: Option<i64>) -> Result<Vec<FeedUpdate>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {UPDATE_COLS} FROM updates
             WHERE project_id = ?1 AND (?2 IS NULL OR id < ?2)
             ORDER BY created_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![project_id, before, limit], |r| self.update_row(r))?;
        let mut updates: Vec<FeedUpdate> = rows.collect::<rusqlite::Result<_>>()?;
        self.attach_media(&mut updates)?;
        Ok(updates)
    }

    pub fn timeline(&self, limit: usize) -> Result<Vec<FeedUpdate>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {UPDATE_U_COLS}
             FROM updates u
             JOIN projects p ON p.id = u.project_id
             WHERE p.archived = 0
             ORDER BY u.created_at DESC, u.id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit], |r| self.update_row(r))?;
        let mut updates: Vec<FeedUpdate> = rows.collect::<rusqlite::Result<_>>()?;
        self.attach_media(&mut updates)?;
        Ok(updates)
    }

    pub fn search_updates(&self, query: &str, project_id: Option<i64>, include_archived: bool, limit: usize) -> Result<Vec<FeedUpdate>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let inc_arch = i64::from(include_archived);
        if let Some(fts) = fts_query(&needle) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {UPDATE_U_COLS}
                 FROM updates u
                 JOIN projects p ON p.id = u.project_id
                 JOIN updates_fts ON updates_fts.rowid = u.id
                 WHERE updates_fts MATCH ?1
                   AND (?2 IS NULL OR u.project_id = ?2)
                   AND (?3 = 1 OR p.archived = 0)
                 ORDER BY u.created_at DESC, u.id DESC
                 LIMIT ?4"
            ))?;
            let rows = stmt.query_map(params![fts, project_id, inc_arch, limit_i64], |r| self.update_row(r))?;
            let mut hits: Vec<FeedUpdate> = rows.collect::<rusqlite::Result<_>>()?;
            if !hits.is_empty() {
                self.attach_media(&mut hits)?;
                return Ok(hits);
            }
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {UPDATE_U_COLS}
             FROM updates u
             JOIN projects p ON p.id = u.project_id
             WHERE (?1 IS NULL OR u.project_id = ?1)
               AND (?2 = 1 OR p.archived = 0)
               AND (
                   instr(lower(u.body), ?3) > 0
                   OR instr(lower(u.agent), ?3) > 0
                   OR instr(lower(COALESCE(u.branch, '')), ?3) > 0
               )
             ORDER BY u.created_at DESC, u.id DESC
             LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![project_id, inc_arch, needle, limit_i64], |r| self.update_row(r))?;
        let mut hits: Vec<FeedUpdate> = rows.collect::<rusqlite::Result<_>>()?;
        self.attach_media(&mut hits)?;
        Ok(hits)
    }

    pub fn add_reply(&self, update_id: i64, author: &str, from_human: bool, body: &str) -> Result<Reply> {
        let body = body.trim();
        if body.is_empty() {
            return Err(Error::Invalid("reply cannot be empty".into()));
        }
        let chars = body.chars().count();
        if chars > MAX_REPLY_CHARS {
            return Err(Error::Invalid(format!("reply is {chars} characters; keep it to {MAX_REPLY_CHARS} or fewer")));
        }
        reject_secret(body)?;
        let update = self.update(update_id)?.ok_or_else(|| Error::NotFound(format!("no update {update_id}")))?;
        let author = author.trim();
        let author = if author.is_empty() { if from_human { "You" } else { "Agent" } } else { author };
        let created_at = now_ms();
        let read_at = if from_human { None } else { Some(created_at) };
        self.conn.execute(
            "INSERT INTO replies (update_id, project_id, author, from_human, body, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![update_id, update.project_id, author, i64::from(from_human), body, created_at, read_at],
        )?;
        Ok(Reply {
            id: self.conn.last_insert_rowid(),
            update_id,
            project_id: update.project_id,
            author: author.to_string(),
            from_human,
            body: body.to_string(),
            created_at,
            read_at,
        })
    }

    pub fn replies_for_updates(&self, update_ids: &[i64]) -> Result<Vec<Reply>> {
        const CHUNK: usize = 500;
        if update_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for chunk in update_ids.chunks(CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT id, update_id, project_id, author, from_human, body, created_at, read_at
                 FROM replies WHERE update_id IN ({placeholders})
                 ORDER BY created_at ASC, id ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), reply_row)?;
            for row in rows {
                out.push(row?);
            }
        }
        if update_ids.len() > CHUNK {
            out.sort_by_key(|r| (r.created_at, r.id));
        }
        Ok(out)
    }

    pub fn unread_human_replies(&self, project_id: Option<i64>) -> Result<Vec<Reply>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.update_id, r.project_id, r.author, r.from_human, r.body, r.created_at, r.read_at
             FROM replies r
             JOIN projects p ON p.id = r.project_id
             WHERE r.from_human = 1 AND r.read_at IS NULL
               AND (?1 IS NULL OR r.project_id = ?1)
               AND (?1 IS NOT NULL OR p.archived = 0)
             ORDER BY r.created_at ASC, r.id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], reply_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_replies_read(&self, ids: &[i64]) -> Result<()> {
        const CHUNK: usize = 500;
        if ids.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        for chunk in ids.chunks(CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(", ");
            let sql = format!("UPDATE replies SET read_at = COALESCE(read_at, ?1) WHERE id IN ({placeholders})");
            let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(chunk.len() + 1);
            params_vec.push(now.into());
            for &id in chunk {
                params_vec.push(id.into());
            }
            self.conn.execute(&sql, rusqlite::params_from_iter(params_vec))?;
        }
        Ok(())
    }

    pub fn mark_project_replies_read(&self, project_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE replies SET read_at = ?1 WHERE project_id = ?2 AND from_human = 1 AND read_at IS NULL",
            params![now_ms(), project_id],
        )?;
        Ok(())
    }

    pub fn set_pulse(&self, project_id: i64, agent: &str, branch: Option<&str>, status: &str, ttl_ms: i64) -> Result<Pulse> {
        if self.project(project_id)?.is_none() {
            return Err(Error::NotFound(format!("no project {project_id}")));
        }
        let agent = agent.trim();
        let agent = if agent.is_empty() { "Agent" } else { agent };
        let status = status.trim();
        let status = if status.is_empty() { "Working" } else { status };
        let branch = branch.map(str::trim).filter(|b| !b.is_empty()).map(str::to_string);
        let updated_at = now_ms();
        let expires_at = updated_at + ttl_ms.clamp(5_000, 3_600_000);
        self.conn.execute(
            "INSERT INTO pulses (project_id, agent, branch, status, updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, agent) DO UPDATE SET
                branch = excluded.branch,
                status = excluded.status,
                updated_at = excluded.updated_at,
                expires_at = excluded.expires_at",
            params![project_id, agent, branch, status, updated_at, expires_at],
        )?;
        Ok(Pulse { project_id, agent: agent.to_string(), branch, status: status.to_string(), updated_at, expires_at })
    }

    pub fn clear_pulse(&self, project_id: i64, agent: &str) -> Result<()> {
        self.conn.execute("DELETE FROM pulses WHERE project_id = ?1 AND agent = ?2", params![project_id, agent])?;
        Ok(())
    }

    fn active_pulses_at(&self, now: i64) -> Result<Vec<Pulse>> {
        let mut stmt = self.conn.prepare(
            "SELECT project_id, agent, branch, status, updated_at, expires_at
             FROM pulses WHERE expires_at > ?1
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![now], |r| {
            Ok(Pulse {
                project_id: r.get(0)?,
                agent: r.get(1)?,
                branch: r.get(2)?,
                status: r.get(3)?,
                updated_at: r.get(4)?,
                expires_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn todos(&self, project_id: i64) -> Result<Vec<TodoItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, done, position, created_at, completed_at
             FROM todos
             WHERE project_id = ?1
             ORDER BY position ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], todo_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn todo(&self, id: i64) -> Result<Option<TodoItem>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, project_id, title, done, position, created_at, completed_at
                 FROM todos WHERE id = ?1",
                params![id],
                todo_row,
            )
            .optional()?)
    }

    pub fn add_todo(&self, project_id: i64, title: &str) -> Result<TodoItem> {
        if self.project(project_id)?.is_none() {
            return Err(Error::NotFound(format!("no project {project_id}")));
        }
        let title = normalize_todo_title(title)?;
        let position: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM todos WHERE project_id = ?1", params![project_id], |r| r.get(0))?;
        let created_at = now_ms();
        self.conn.execute(
            "INSERT INTO todos (project_id, title, done, position, created_at, completed_at)
             VALUES (?1, ?2, 0, ?3, ?4, NULL)",
            params![project_id, title, position, created_at],
        )?;
        Ok(TodoItem { id: self.conn.last_insert_rowid(), project_id, title, done: false, position, created_at, completed_at: None })
    }

    pub fn set_todo_done(&self, id: i64, done: bool) -> Result<TodoItem> {
        let completed_at = done.then(now_ms);
        let changed =
            self.conn.execute("UPDATE todos SET done = ?1, completed_at = ?2 WHERE id = ?3", params![i64::from(done), completed_at, id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no to-do {id}")));
        }
        self.todo(id)?.ok_or_else(|| Error::NotFound(format!("no to-do {id}")))
    }

    pub fn rename_todo(&self, id: i64, title: &str) -> Result<TodoItem> {
        let title = normalize_todo_title(title)?;
        let changed = self.conn.execute("UPDATE todos SET title = ?1 WHERE id = ?2", params![title, id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no to-do {id}")));
        }
        self.todo(id)?.ok_or_else(|| Error::NotFound(format!("no to-do {id}")))
    }

    pub fn delete_todo(&self, id: i64) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM todos WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no to-do {id}")));
        }
        Ok(())
    }

    pub fn clear_completed_todos(&self, project_id: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM todos WHERE project_id = ?1 AND done = 1", params![project_id])?)
    }

    pub fn clear_project_todos(&self, project_id: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM todos WHERE project_id = ?1", params![project_id])?)
    }

    fn update_row(&self, r: &Row<'_>) -> rusqlite::Result<FeedUpdate> {
        let shot: Option<String> = r.get(4)?;
        let media_w: Option<i64> = r.get(5)?;
        let media_h: Option<i64> = r.get(6)?;
        Ok(FeedUpdate {
            id: r.get(0)?,
            project_id: r.get(1)?,
            agent: r.get(2)?,
            body: r.get(3)?,
            screenshot: shot.map(|rel| self.dir.join(rel)),
            media_w: media_w.and_then(|v| u32::try_from(v).ok()),
            media_h: media_h.and_then(|v| u32::try_from(v).ok()),
            media: Vec::new(),
            branch: r.get(7)?,
            created_at: r.get(8)?,
            diff_stat: r.get(9)?,
            commit_sha: r.get(10)?,
            attention_dismissed_at: r.get(11)?,
            via: r.get(12)?,
        })
    }

    fn media_row(&self, r: &Row<'_>) -> rusqlite::Result<MediaAttachment> {
        let pos: i64 = r.get(2)?;
        let rel: String = r.get(3)?;
        let width: Option<i64> = r.get(4)?;
        let height: Option<i64> = r.get(5)?;
        Ok(MediaAttachment {
            id: r.get(0)?,
            update_id: r.get(1)?,
            position: usize::try_from(pos).unwrap_or(0),
            path: self.dir.join(rel),
            width: width.and_then(|v| u32::try_from(v).ok()),
            height: height.and_then(|v| u32::try_from(v).ok()),
            caption: r.get(6)?,
        })
    }

    fn attach_media(&self, updates: &mut [FeedUpdate]) -> Result<()> {
        const CHUNK: usize = 500;
        if updates.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = updates.iter().map(|u| u.id).collect();
        let mut by_update: HashMap<i64, Vec<MediaAttachment>> = HashMap::new();
        for chunk in ids.chunks(CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT id, update_id, position, path, width, height, caption
                 FROM update_media
                 WHERE update_id IN ({placeholders})
                 ORDER BY position ASC, id ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| self.media_row(r))?;
            for row in rows {
                let item = row?;
                by_update.entry(item.update_id).or_default().push(item);
            }
        }
        for u in updates.iter_mut() {
            if let Some(items) = by_update.remove(&u.id) {
                if let Some(first) = items.first() {
                    if u.screenshot.is_none() {
                        u.screenshot = Some(first.path.clone());
                    }
                    if u.media_w.is_none() {
                        u.media_w = first.width;
                    }
                    if u.media_h.is_none() {
                        u.media_h = first.height;
                    }
                }
                u.media = items;
            }
        }
        Ok(())
    }

    fn store_media(&self, project: &Project, src: &Path, created_at: i64) -> Result<String> {
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .filter(|e| IMAGE_TYPES.contains(&e.as_str()) || VIDEO_TYPES.contains(&e.as_str()))
            .ok_or_else(|| {
                Error::Invalid(format!("media must be an image ({}) or short video ({})", IMAGE_TYPES.join(", "), VIDEO_TYPES.join(", ")))
            })?;
        let is_video = VIDEO_TYPES.contains(&ext.as_str());
        let max_bytes = if is_video { MAX_VIDEO_BYTES } else { MAX_IMAGE_BYTES };
        let meta = std::fs::symlink_metadata(src).map_err(|e| Error::Invalid(format!("can't read media {}: {e}", src.display())))?;
        if meta.file_type().is_symlink() {
            return Err(Error::Invalid(format!("{} is a symlink; pass a regular media file", src.display())));
        }
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", src.display())));
        }
        if meta.len() > max_bytes {
            return Err(Error::Invalid(format!("media must be under {} MB", max_bytes / 1024 / 1024)));
        }
        if !matches_media_magic(src, &ext) {
            return Err(Error::Invalid(format!("{} does not look like a valid .{ext} file", src.display())));
        }
        let rel = format!("media/{}-{}-{:08x}.{ext}", project.slug, created_at, fastrand::u32(..));
        std::fs::copy(src, self.dir.join(&rel))?;
        Ok(rel)
    }

    fn one(&self, sql: &str, params: impl rusqlite::Params) -> Result<Option<Project>> {
        Ok(self.conn.query_row(sql, params, project_row).optional()?)
    }
}

fn reject_secret(text: &str) -> Result<()> {
    if let Some(kind) = crate::secret_in(text) {
        return Err(Error::Invalid(format!("looks like {kind} — remove the secret before saving")));
    }
    Ok(())
}

fn matches_media_magic(path: &Path, ext: &str) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = [0u8; 16];
    let Ok(n) = f.read(&mut head) else { return false };
    match ext {
        "png" => n >= 8 && &head[0..8] == b"\x89PNG\r\n\x1a\n",
        "jpg" | "jpeg" => n >= 3 && head[0..3] == [0xff, 0xd8, 0xff],
        "gif" => n >= 6 && (&head[0..6] == b"GIF87a" || &head[0..6] == b"GIF89a"),
        "webp" => n >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP",
        "mp4" | "m4v" | "mov" => n >= 8 && matches!(&head[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free"),
        "webm" => n >= 4 && head[0..4] == [0x1a, 0x45, 0xdf, 0xa3],
        _ => false,
    }
}

fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> =
        query.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| format!("\"{}\"*", t.replace('"', "\"\""))).collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn migrate_projects(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(projects)")?;
    let cols: Vec<String> = stmt.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    if !cols.iter().any(|c| c == "archived") {
        conn.execute("ALTER TABLE projects ADD COLUMN archived INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !cols.iter().any(|c| c == "seen_at") {
        conn.execute("ALTER TABLE projects ADD COLUMN seen_at INTEGER", [])?;
        conn.execute(
            "UPDATE projects
             SET seen_at = COALESCE((SELECT MAX(created_at) FROM updates WHERE project_id = projects.id), created_at)
             WHERE seen_at IS NULL",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "pinned") {
        conn.execute("ALTER TABLE projects ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !cols.iter().any(|c| c == "brief") {
        conn.execute("ALTER TABLE projects ADD COLUMN brief TEXT", [])?;
    }
    Ok(())
}

fn migrate_updates(conn: &Connection, dir: &Path) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(updates)")?;
    let cols: Vec<String> = stmt.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    if !cols.iter().any(|c| c == "via") {
        conn.execute("ALTER TABLE updates ADD COLUMN via TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "branch") {
        conn.execute("ALTER TABLE updates ADD COLUMN branch TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "diff_stat") {
        conn.execute("ALTER TABLE updates ADD COLUMN diff_stat TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "commit_sha") {
        conn.execute("ALTER TABLE updates ADD COLUMN commit_sha TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "attention_dismissed_at") {
        conn.execute("ALTER TABLE updates ADD COLUMN attention_dismissed_at INTEGER", [])?;
    }
    let added_media_dims = !cols.iter().any(|c| c == "media_w");
    if added_media_dims {
        conn.execute("ALTER TABLE updates ADD COLUMN media_w INTEGER", [])?;
    }
    if !cols.iter().any(|c| c == "media_h") {
        conn.execute("ALTER TABLE updates ADD COLUMN media_h INTEGER", [])?;
    }
    if added_media_dims {
        let mut q = conn.prepare("SELECT id, screenshot FROM updates WHERE screenshot IS NOT NULL AND media_w IS NULL")?;
        let existing: Vec<(i64, String)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, rel) in existing {
            let full = dir.join(&rel);
            if media_kind_for_path(&full) == Some(MediaKind::Image)
                && let Some((w, h)) = image_dimensions(&full)
            {
                conn.execute("UPDATE updates SET media_w = ?1, media_h = ?2 WHERE id = ?3", params![i64::from(w), i64::from(h), id])?;
            }
        }
    }
    conn.execute(
        "INSERT INTO update_media (update_id, position, path, width, height, caption)
         SELECT u.id, 0, u.screenshot, u.media_w, u.media_h, NULL
         FROM updates u
         WHERE u.screenshot IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM update_media m WHERE m.update_id = u.id)",
        [],
    )?;
    let had_fts: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'updates_fts')", [], |r| r.get(0))?;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS updates_fts USING fts5(
            body,
            agent,
            branch,
            content='updates',
            content_rowid='id',
            tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS updates_ai AFTER INSERT ON updates BEGIN
            INSERT INTO updates_fts(rowid, body, agent, branch) VALUES (new.id, new.body, new.agent, COALESCE(new.branch, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS updates_ad AFTER DELETE ON updates BEGIN
            INSERT INTO updates_fts(updates_fts, rowid, body, agent, branch) VALUES('delete', old.id, old.body, old.agent, COALESCE(old.branch, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS updates_au AFTER UPDATE OF body, agent, branch ON updates BEGIN
            INSERT INTO updates_fts(updates_fts, rowid, body, agent, branch) VALUES('delete', old.id, old.body, old.agent, COALESCE(old.branch, ''));
            INSERT INTO updates_fts(rowid, body, agent, branch) VALUES (new.id, new.body, new.agent, COALESCE(new.branch, ''));
        END;",
    )?;
    if !had_fts {
        conn.execute("INSERT INTO updates_fts(updates_fts) VALUES('rebuild')", [])?;
    }
    Ok(())
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn project_row(r: &Row<'_>) -> rusqlite::Result<Project> {
    let orb: String = r.get(4)?;
    let orb =
        serde_json::from_str(&orb).map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;
    let path: Option<String> = r.get(3)?;
    let archived: i64 = r.get(6)?;
    let pinned: i64 = r.get(8)?;
    Ok(Project {
        id: r.get(0)?,
        slug: r.get(1)?,
        name: r.get(2)?,
        path: path.map(PathBuf::from),
        orb,
        created_at: r.get(5)?,
        archived: archived != 0,
        seen_at: r.get(7)?,
        pinned: pinned != 0,
        brief: r.get(9)?,
    })
}

fn reply_row(r: &Row<'_>) -> rusqlite::Result<Reply> {
    let from_human: i64 = r.get(4)?;
    Ok(Reply {
        id: r.get(0)?,
        update_id: r.get(1)?,
        project_id: r.get(2)?,
        author: r.get(3)?,
        from_human: from_human != 0,
        body: r.get(5)?,
        created_at: r.get(6)?,
        read_at: r.get(7)?,
    })
}

fn normalize_todo_title(raw: &str) -> Result<String> {
    let cleaned: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        return Err(Error::Invalid("to-do cannot be empty".into()));
    }
    let chars = cleaned.chars().count();
    if chars > MAX_TODO_CHARS {
        return Err(Error::Invalid(format!("to-do is {chars} characters; keep it to {MAX_TODO_CHARS} or fewer")));
    }
    Ok(cleaned)
}

fn todo_row(r: &Row<'_>) -> rusqlite::Result<TodoItem> {
    let done: i64 = r.get(3)?;
    Ok(TodoItem {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        done: done != 0,
        position: r.get(4)?,
        created_at: r.get(5)?,
        completed_at: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("feeder-store-{tag}-{}-{}", std::process::id(), fastrand::u32(..)));
        (Store::open(&dir).unwrap(), dir)
    }

    fn note(body: &str) -> NewUpdate {
        NewUpdate { agent: "Claude Code".into(), body: body.into(), ..NewUpdate::default() }
    }

    #[test]
    fn posting_creates_the_feed_newest_first_and_orders_projects_by_activity() {
        let (s, dir) = temp_store("feed");
        let a = s.project_named("Alpha", None).unwrap();
        let b = s.project_named("Beta", None).unwrap();
        s.post(a.id, note("first")).unwrap();
        s.post(b.id, note("beta work")).unwrap();
        s.post(a.id, note("second")).unwrap();
        let feed = s.feed(a.id, 10, None).unwrap();
        assert_eq!(feed.iter().map(|u| u.body.as_str()).collect::<Vec<_>>(), ["second", "first"]);
        let older = s.feed(a.id, 10, Some(feed[0].id)).unwrap();
        assert_eq!(older.len(), 1);
        let all = s.timeline(10).unwrap();
        assert_eq!(all.iter().map(|u| u.body.as_str()).collect::<Vec<_>>(), ["second", "beta work", "first"]);
        assert_eq!(s.timeline(2).unwrap().len(), 2);
        let projects = s.projects().unwrap();
        assert_eq!(projects[0].project.name, "Alpha");
        assert_eq!(projects[0].updates, 2);
        assert_eq!(projects[0].last_agent.as_deref(), Some("Claude Code"));
        assert_ne!(projects[0].project.orb, projects[1].project.orb);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn detected_folders_reuse_their_project_and_same_names_elsewhere_get_their_own() {
        let (s, dir) = temp_store("detect");
        let here = DetectedProject { name: "app".into(), path: PathBuf::from("/work/app") };
        let there = DetectedProject { name: "app".into(), path: PathBuf::from("/other/app") };
        let p1 = s.project_at(&here).unwrap();
        assert_eq!(s.project_at(&here).unwrap().id, p1.id);
        let p2 = s.project_at(&there).unwrap();
        assert_ne!(p1.id, p2.id);
        assert_eq!(p2.slug, "app-2");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_named_project_adopts_the_folder_it_is_first_posted_from() {
        let (s, dir) = temp_store("adopt");
        let p = s.project_named("Site", None).unwrap();
        let adopted = s.project_at(&DetectedProject { name: "site".into(), path: PathBuf::from("/w/site") }).unwrap();
        assert_eq!(adopted.id, p.id);
        assert_eq!(adopted.path.as_deref(), Some(Path::new("/w/site")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn screenshots_are_copied_in_and_bad_notes_store_nothing() {
        let (s, dir) = temp_store("shot");
        let p = s.project_named("Shots", None).unwrap();
        let spoof = dir.join("fake.png");
        std::fs::write(&spoof, b"not really a png").unwrap();
        assert!(s.post(p.id, NewUpdate { screenshot: Some(spoof), ..note("spoofed") }).is_err());

        let src = dir.join("in.png");
        std::fs::write(&src, b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR").unwrap();
        let u = s.post(p.id, NewUpdate { screenshot: Some(src.clone()), ..note("with a picture") }).unwrap();
        let stored = u.screenshot.unwrap();
        assert!(stored.starts_with(dir.join("media")) && stored.exists());
        assert!(s.post(p.id, note("a\n\nb\n\nc\n\nd")).is_err());
        let txt = dir.join("in.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert!(s.post(p.id, NewUpdate { screenshot: Some(txt), ..note("bad file") }).is_err());
        assert_eq!(s.feed(p.id, 10, None).unwrap().len(), 1);
        s.remove_project(p.id).unwrap();
        assert!(!stored.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_renamed_project_is_still_found_by_its_folder_and_old_name() {
        let (s, dir) = temp_store("rename");
        let p = s.project_at(&DetectedProject { name: "acme-site".into(), path: PathBuf::from("/w/acme-site") }).unwrap();
        s.rename_project(p.id, "Acme").unwrap();
        let renamed = s.project(p.id).unwrap().unwrap();
        assert_eq!(renamed.name, "Acme");
        assert!(renamed.renamed());
        for q in ["Acme", "acme-site", "/w/acme-site"] {
            assert_eq!(s.find_project(q).unwrap().map(|p| p.id), Some(p.id), "{q}");
        }
        assert_eq!(s.find_project("%").unwrap(), None);
        assert_eq!(s.find_project("acme_s_te").unwrap(), None);
        assert_eq!(s.find_project("   ").unwrap(), None);
        s.rename_project(p.id, "  ").unwrap();
        assert_eq!(s.project(p.id).unwrap().unwrap().name, "acme-site");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn archiving_hides_from_timeline_and_restores_cleanly() {
        let (s, dir) = temp_store("archive");
        let a = s.project_named("Alpha", None).unwrap();
        let b = s.project_named("Beta", None).unwrap();
        s.post(a.id, note("alpha 1")).unwrap();
        s.post(b.id, note("beta 1")).unwrap();
        s.set_archived(a.id, true).unwrap();
        assert!(s.project(a.id).unwrap().unwrap().archived);
        let tl = s.timeline(10).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].project_id, b.id);
        s.set_archived(a.id, false).unwrap();
        assert_eq!(s.timeline(10).unwrap().len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unseen_tracks_new_updates_until_marked_seen() {
        let (s, dir) = temp_store("seen");
        let a = s.project_named("Alpha", None).unwrap();
        assert!(!s.projects().unwrap()[0].unseen());
        s.post(a.id, note("first")).unwrap();
        assert!(s.projects().unwrap()[0].unseen());
        s.mark_seen(a.id).unwrap();
        assert!(!s.projects().unwrap()[0].unseen());
        std::thread::sleep(Duration::from_millis(5));
        s.post(a.id, note("second")).unwrap();
        assert!(s.projects().unwrap()[0].unseen());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn legacy_database_migrates_archived_and_seen_at_and_fts() {
        let dir = std::env::temp_dir().join(format!("feeder-store-migrate-{}-{}", std::process::id(), fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("feeder.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL, path TEXT UNIQUE, orb TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE updates (id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, agent TEXT NOT NULL, body TEXT NOT NULL, screenshot TEXT, created_at INTEGER NOT NULL);
             INSERT INTO projects (id, slug, name, path, orb, created_at) VALUES (1, 'old', 'Old', NULL, '{\"hues\":[210.0,225.0,240.0],\"chroma\":0.18,\"pattern\":\"nebula\",\"angle\":1.0,\"seed\":1}', 100);
             INSERT INTO updates (id, project_id, agent, body, screenshot, created_at) VALUES (1, 1, 'Codex', 'legacy webhook note', NULL, 200);",
        ).unwrap();
        drop(conn);
        let s = Store::open(&dir).unwrap();
        let ps = s.projects().unwrap();
        assert_eq!(ps.len(), 1);
        assert!(!ps[0].project.archived);
        assert_eq!(ps[0].project.seen_at, Some(200));
        assert!(!ps[0].unseen());
        assert_eq!(s.search_updates("webhook", None, false, 10).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn another_connections_commit_moves_the_data_version() {
        let (watcher, dir) = temp_store("version");
        let writer = Store::open(&dir).unwrap();
        let before = watcher.data_version().unwrap();
        let p = writer.project_named("Watched", None).unwrap();
        writer.post(p.id, note("hello")).unwrap();
        assert_ne!(watcher.data_version().unwrap(), before);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pinning_anchors_first_and_mark_all_seen_clears_active_dots() {
        let (s, dir) = temp_store("pin-seen");
        let a = s.project_named("Alpha", None).unwrap();
        let b = s.project_named("Beta", None).unwrap();
        s.post(a.id, note("alpha note")).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        s.post(b.id, note("beta note")).unwrap();
        s.set_pinned(a.id, true).unwrap();
        let ps = s.projects().unwrap();
        assert_eq!(ps[0].project.id, a.id);
        assert!(ps[0].project.pinned);
        assert_eq!(ps[0].last_body.as_deref(), Some("alpha note"));
        assert!(ps[0].unseen() && ps[1].unseen());
        s.mark_all_seen().unwrap();
        let ps = s.projects().unwrap();
        assert!(!ps[0].unseen() && !ps[1].unseen());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn delete_update_removes_screenshot_and_search_finds_matches() {
        let (s, dir) = temp_store("search-del");
        let a = s.project_named("Alpha", None).unwrap();
        let b = s.project_named("Beta", None).unwrap();
        let src = dir.join("pic.png");
        std::fs::write(&src, b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR").unwrap();
        let u1 = s
            .post(a.id, NewUpdate { screenshot: Some(src), branch: Some("feat/stripe-retry".into()), ..note("Fixed Stripe webhook retry") })
            .unwrap();
        let u2 = s.post(b.id, note("Polished orb shader")).unwrap();
        let shot = u1.screenshot.clone().unwrap();
        assert!(shot.exists());
        assert_eq!(u1.branch.as_deref(), Some("feat/stripe-retry"));

        let hits = s.search_updates("stripe", None, false, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, u1.id);

        assert_eq!(s.search_updates("feat/stripe", None, false, 10).unwrap()[0].id, u1.id);
        assert_eq!(s.search_updates("ebhook", None, false, 10).unwrap()[0].id, u1.id);

        let edited = s.edit_update(u1.id, "Fixed Stripe billing retry").unwrap();
        assert_eq!(edited.body, "Fixed Stripe billing retry");
        assert!(s.search_updates("webhook", None, false, 10).unwrap().is_empty());
        assert_eq!(s.search_updates("billing", None, false, 10).unwrap()[0].id, u1.id);

        s.set_archived(b.id, true).unwrap();
        assert!(s.search_updates("shader", None, false, 10).unwrap().is_empty());
        assert_eq!(s.search_updates("shader", None, true, 10).unwrap()[0].id, u2.id);

        s.delete_update(u1.id).unwrap();
        assert!(!shot.exists());
        assert!(s.feed(a.id, 10, None).unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn brief_replies_needs_you_and_pulses_work_end_to_end() {
        let (s, dir) = temp_store("new-features");
        let p = s.project_named("Orbit", None).unwrap();

        s.set_project_brief(p.id, Some("Goal: Ship v2\nFocus: Billing\nTest: cargo test")).unwrap();
        assert_eq!(s.project(p.id).unwrap().unwrap().brief.as_deref(), Some("Goal: Ship v2\nFocus: Billing\nTest: cargo test"));
        s.set_project_brief(p.id, Some("   ")).unwrap();
        assert_eq!(s.project(p.id).unwrap().unwrap().brief, None);

        let pulse = s.set_pulse(p.id, "Codex", Some("feat/pulse"), "Wiring replies", 60_000).unwrap();
        assert_eq!(pulse.status, "Wiring replies");
        let sum = &s.projects().unwrap()[0];
        assert!(sum.active_pulse.is_some());

        let clip = dir.join("demo.mp4");
        std::fs::write(&clip, b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00").unwrap();
        let u = s
            .post(
                p.id,
                NewUpdate {
                    agent: "Codex".into(),
                    body: "Done: built replies.\n\nWhy: two-way loop.\n\nDecision needed: keep TTL at 120s?".into(),
                    screenshot: Some(clip),
                    screenshots: Vec::new(),
                    branch: Some("feat/pulse".into()),
                    diff_stat: Some("+80/-12 · 3f".into()),
                    commit_sha: Some("abc1234".into()),
                },
            )
            .unwrap();
        assert!(u.has_video());
        assert!(!u.has_image());
        assert!(s.has_commit(p.id, "abc1234").unwrap());
        let sum = &s.projects().unwrap()[0];
        assert!(sum.active_pulse.is_none());
        assert!(sum.needs_you);

        let reply = s.add_reply(u.id, "You", true, "Yes, 120s is great.").unwrap();
        assert_eq!(reply.read_at, None);
        let sum = &s.projects().unwrap()[0];
        assert!(!sum.needs_you);
        assert_eq!(sum.unread_replies, 1);

        let unread = s.unread_human_replies(Some(p.id)).unwrap();
        assert_eq!(unread.len(), 1);
        s.mark_replies_read(&[reply.id]).unwrap();
        assert!(s.unread_human_replies(Some(p.id)).unwrap().is_empty());
        assert_eq!(s.replies_for_updates(&[u.id]).unwrap().len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn image_dimensions_parses_png_gif_jpeg_and_webp_headers() {
        use std::io::Cursor;

        let mut png = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&720u32.to_be_bytes());
        assert_eq!(image_dimensions_from_reader(Cursor::new(&png)), Some((1280, 720)));

        let mut gif = Vec::from(&b"GIF89a"[..]);
        gif.extend_from_slice(&320u16.to_le_bytes());
        gif.extend_from_slice(&240u16.to_le_bytes());
        assert_eq!(image_dimensions_from_reader(Cursor::new(&gif)), Some((320, 240)));

        let jpeg =
            [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x02, 0x58, 0x03, 0x20, 0x03, 0x01, 0x11, 0x00];
        assert_eq!(image_dimensions_from_reader(Cursor::new(&jpeg)), Some((800, 600)));

        let bits: u32 = (640 - 1) | ((480 - 1) << 14);
        let mut webp = Vec::from(&b"RIFF\x00\x00\x00\x00WEBPVP8L\x00\x00\x00\x00\x2f"[..]);
        webp.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(image_dimensions_from_reader(Cursor::new(&webp)), Some((640, 480)));
    }

    #[test]
    fn post_and_migration_persist_image_dimensions() {
        let dir = std::env::temp_dir().join(format!("feeder-store-dims-{}-{}", std::process::id(), fastrand::u32(..)));
        std::fs::create_dir_all(dir.join("media")).unwrap();
        let mut png = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
        png.extend_from_slice(&1440u32.to_be_bytes());
        png.extend_from_slice(&900u32.to_be_bytes());
        std::fs::write(dir.join("media/legacy.png"), &png).unwrap();

        let conn = Connection::open(dir.join("feeder.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL, path TEXT UNIQUE, orb TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE updates (id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, agent TEXT NOT NULL, body TEXT NOT NULL, screenshot TEXT, created_at INTEGER NOT NULL);
             INSERT INTO projects (id, slug, name, path, orb, created_at) VALUES (1, 'old', 'Old', NULL, '{\"hues\":[210.0,225.0,240.0],\"chroma\":0.18,\"pattern\":\"nebula\",\"angle\":1.0,\"seed\":1}', 100);
             INSERT INTO updates (id, project_id, agent, body, screenshot, created_at) VALUES (1, 1, 'Codex', 'legacy image', 'media/legacy.png', 200);",
        ).unwrap();
        drop(conn);

        let s = Store::open(&dir).unwrap();
        let feed = s.feed(1, 10, None).unwrap();
        assert_eq!(feed[0].media_w, Some(1440));
        assert_eq!(feed[0].media_h, Some(900));

        let new_src = dir.join("fresh.png");
        let mut png2 = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
        png2.extend_from_slice(&800u32.to_be_bytes());
        png2.extend_from_slice(&600u32.to_be_bytes());
        std::fs::write(&new_src, &png2).unwrap();
        let posted = s.post(1, NewUpdate { screenshot: Some(new_src), ..note("fresh") }).unwrap();
        assert_eq!((posted.media_w, posted.media_h), (Some(800), Some(600)));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_todos_crud_counts_and_cascade_work() {
        let (s, dir) = temp_store("todos");
        let p = s.project_named("Feeder", None).unwrap();
        assert!(s.todos(p.id).unwrap().is_empty());
        assert_eq!(s.projects().unwrap()[0].open_todos, 0);

        assert!(s.add_todo(p.id, "   \n ").is_err());
        assert!(s.add_todo(p.id, &"x".repeat(MAX_TODO_CHARS + 1)).is_err());
        assert!(s.add_todo(9999, "orphan").is_err());

        let t1 = s.add_todo(p.id, "  Ship   project to-do lists\n").unwrap();
        let t2 = s.add_todo(p.id, "Wire MCP todo tools").unwrap();
        let t3 = s.add_todo(p.id, "Verify light mode").unwrap();
        assert_eq!(t1.title, "Ship project to-do lists");
        assert!(!t1.done);
        assert_eq!(t1.completed_at, None);
        assert_eq!(s.projects().unwrap()[0].open_todos, 3);

        let done_t1 = s.set_todo_done(t1.id, true).unwrap();
        assert!(done_t1.done);
        assert!(done_t1.completed_at.is_some());
        assert_eq!(s.projects().unwrap()[0].open_todos, 2);

        let undone_t1 = s.set_todo_done(t1.id, false).unwrap();
        assert!(!undone_t1.done);
        assert_eq!(undone_t1.completed_at, None);
        assert_eq!(s.projects().unwrap()[0].open_todos, 3);

        let renamed = s.rename_todo(t2.id, "Wire MCP and CLI todo tools").unwrap();
        assert_eq!(renamed.title, "Wire MCP and CLI todo tools");

        s.set_todo_done(t1.id, true).unwrap();
        assert_eq!(s.clear_completed_todos(p.id).unwrap(), 1);
        let remaining = s.todos(p.id).unwrap();
        assert_eq!(remaining.iter().map(|t| t.id).collect::<Vec<_>>(), vec![t2.id, t3.id]);

        s.delete_todo(t2.id).unwrap();
        assert_eq!(s.todos(p.id).unwrap().len(), 1);
        assert_eq!(s.projects().unwrap()[0].open_todos, 1);

        assert_eq!(s.clear_project_todos(p.id).unwrap(), 1);
        assert!(s.todos(p.id).unwrap().is_empty());

        s.add_todo(p.id, "Cascade check").unwrap();
        s.remove_project(p.id).unwrap();
        assert!(s.todos(p.id).unwrap().is_empty());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dismissing_attention_clears_needs_you_without_fake_reply() {
        let (s, dir) = temp_store("dismiss");
        let p = s.project_named("Orbit", None).unwrap();
        let u = s.post(p.id, note("Done: wired auth.\n\nDecision needed: keep legacy token?")).unwrap();
        let sum = &s.projects().unwrap()[0];
        assert!(sum.needs_you);
        assert_eq!(sum.last_update_id, Some(u.id));

        s.dismiss_attention(u.id).unwrap();
        let sum = &s.projects().unwrap()[0];
        assert!(!sum.needs_you);
        assert!(s.update(u.id).unwrap().unwrap().attention_dismissed_at.is_some());
        assert!(s.replies_for_updates(&[u.id]).unwrap().is_empty());

        s.undismiss_attention(u.id).unwrap();
        let sum = &s.projects().unwrap()[0];
        assert!(sum.needs_you);
        assert!(s.update(u.id).unwrap().unwrap().attention_dismissed_at.is_none());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn secrets_are_rejected_in_updates_edits_replies_and_briefs() {
        let (s, dir) = temp_store("secrets");
        let p = s.project_named("Vault", None).unwrap();
        let key = "sk-abcdefghijklmnopqrstuvwxyz123456";
        assert!(s.post(p.id, note(&format!("Done: used {key}"))).is_err());
        assert!(s.set_project_brief(p.id, Some(&format!("Key: {key}"))).is_err());

        let u = s.post(p.id, note("Done: clean update.")).unwrap();
        assert!(s.edit_update(u.id, &format!("Done: leaked {key}")).is_err());
        assert!(s.add_reply(u.id, "You", true, &format!("Try {key}")).is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_media_is_refused() {
        let (s, dir) = temp_store("symlink-media");
        let p = s.project_named("Links", None).unwrap();
        let real = dir.join("real.png");
        let link = dir.join("link.png");
        std::fs::write(&real, b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(s.post(p.id, NewUpdate { screenshot: Some(link), ..note("symlinked") }).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn chunked_reply_queries_handle_over_500_ids() {
        let (s, dir) = temp_store("chunk-replies");
        let p = s.project_named("Big", None).unwrap();
        let u = s.post(p.id, note("Done: batch test.")).unwrap();
        let r = s.add_reply(u.id, "You", true, "Looks good.").unwrap();

        let mut ids: Vec<i64> = (1000..1600).collect();
        ids.push(u.id);
        let found = s.replies_for_updates(&ids).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, r.id);

        let mut rids: Vec<i64> = (2000..2600).collect();
        rids.push(r.id);
        s.mark_replies_read(&rids).unwrap();
        assert!(s.unread_human_replies(Some(p.id)).unwrap().is_empty());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_media_carousel_stores_orders_caps_and_cleans_up() {
        let (s, dir) = temp_store("carousel");
        let p = s.project_named("Gallery", None).unwrap();
        let make_png = |name: &str, w: u32, h: u32| -> PathBuf {
            let path = dir.join(name);
            let mut bytes = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
            bytes.extend_from_slice(&w.to_be_bytes());
            bytes.extend_from_slice(&h.to_be_bytes());
            bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let p1 = make_png("a.png", 640, 400);
        let p2 = make_png("b.png", 800, 600);
        let p3 = make_png("c.png", 1200, 900);

        let posted = s
            .post(p.id, NewUpdate { screenshot: Some(p1.clone()), screenshots: vec![p2.clone(), p3.clone()], ..note("Done: 3 screens.") })
            .unwrap();
        assert_eq!(posted.media.len(), 3);
        assert_eq!(posted.screenshot.as_ref(), Some(&posted.media[0].path));
        assert_eq!((posted.media_w, posted.media_h), (Some(640), Some(400)));
        assert_eq!((posted.media[1].width, posted.media[1].height), (Some(800), Some(600)));
        assert_eq!((posted.media[2].width, posted.media[2].height), (Some(1200), Some(900)));

        let reloaded = s.feed(p.id, 10, None).unwrap();
        assert_eq!(reloaded[0].media.len(), 3);
        let stored_paths: Vec<PathBuf> = reloaded[0].media.iter().map(|m| m.path.clone()).collect();
        assert!(stored_paths.iter().all(|path| path.exists()));

        let too_many: Vec<PathBuf> = (0..=MAX_MEDIA_PER_UPDATE).map(|_| p1.clone()).collect();
        assert!(s.post(p.id, NewUpdate { screenshots: too_many, ..note("too many") }).is_err());

        s.delete_update(posted.id).unwrap();
        assert!(stored_paths.iter().all(|path| !path.exists()));

        std::fs::remove_dir_all(dir).ok();
    }
}
