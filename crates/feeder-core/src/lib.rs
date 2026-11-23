//! Feeder's shared core: the local store every client writes to (CLI, MCP server, desktop app),
//! the rules an update must follow, and the orb each project is drawn as.

mod agent;
mod detect;
pub mod gardener;
mod guide;
mod knowledge;
mod notebook;
mod orb;
mod settings;
pub mod setup;
mod store;
mod time;
mod update;

pub use agent::{agent_display_name, agent_from_env};
pub use detect::{
    DetectedProject, GitMeta, detect_branch, detect_git_meta, detect_project, format_shortstat, install_post_commit_hook,
    latest_commit_summary, remove_post_commit_hook, slugify,
};
pub use guide::{GUIDE, NOTEBOOK_GUIDE, notebook_skill_markdown, skill_markdown};
pub use knowledge::FeedReviewStatus;
pub use knowledge::{
    Change, ChangeAction, Curated, Curation, FLAG_NOTE_CHARS, FLAG_SUGGESTION_MAX_CHARS, Flag, FlagKind, FlagOutcome, FlagStatus, GARDENER,
    NewFlag, PageSnapshot, Provenance, REASON_MAX_CHARS,
};
pub use notebook::{
    BODY_CHARS, CANONICAL_TOPICS, HUMAN_AUTHOR, Kind, OnSimilar, Page, PageDraft, PageFilter, Standing, TAG_COUNT, TITLE_CHARS,
    TOPIC_MAX_CHARS, TagCount, TopicSpec, TopicSummary, compose_legacy_body, infer_topic, markdown_excerpt, normalize_tags,
    normalize_topic, secret_in, tag_counts, toggle_markdown_task, topic_counts, topic_label,
};
pub use orb::{Orb, Pattern};
pub use settings::{OrbSize, Order, Settings, Theme};
pub use store::{
    FeedUpdate, MAX_MEDIA_PER_UPDATE, MAX_TODO_CHARS, MediaAttachment, MediaKind, NewUpdate, Project, ProjectSummary, Pulse, Reply, Store,
    TodoItem, data_dir, image_dimensions, media_kind_for_path,
};
pub use time::{ago, day_key, day_label, now_ms, relative_time};
pub use update::{
    AttentionKind, MAX_CHARS, MAX_PARAGRAPHS, UpdateError, build_resume_prompt, extract_block, parse_attention, update_blocks,
    validate_body,
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    Io(std::io::Error),
    Invalid(String),
    NotFound(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Db(e) => write!(f, "database error: {e}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Invalid(m) | Error::NotFound(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Db(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Invalid(_) | Error::NotFound(_) => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<UpdateError> for Error {
    fn from(e: UpdateError) -> Self {
        Error::Invalid(e.to_string())
    }
}
