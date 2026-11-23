//! The notebook: one shared, searchable knowledge library of freeform Markdown articles across
//! every project and every agent.
//!
//! Write a note like you would in Obsidian or Notion: a title, an optional category/topic, and a
//! freeform `.md` body (headings, code blocks, checklists, tables, callouts, links). Near-duplicate
//! titles are flagged for agents so existing articles stay fresh, and anything that looks like a
//! secret is turned away.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Error, Result, Store, ago, now_ms};

/// Title length, in characters.
pub const TITLE_CHARS: std::ops::RangeInclusive<usize> = 2..=120;
/// Markdown article body length, in characters.
pub const BODY_CHARS: std::ops::RangeInclusive<usize> = 1..=50_000;
/// Optional tags per page.
pub const TAG_COUNT: std::ops::RangeInclusive<usize> = 0..=10;
/// Characters per tag.
pub const TAG_CHARS: std::ops::RangeInclusive<usize> = 2..=24;
/// Title-word overlap (Jaccard) at which two pages count as the same note.
const SIMILAR_TITLE: f32 = 0.6;

/// The author name the app writes under when the person edits the Notebook by hand.
pub const HUMAN_AUTHOR: &str = "You";

/// Optional kind label for a notebook article. Freeform notes default to [`Kind::Note`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A freeform Markdown note or article.
    #[default]
    Note,
    /// A bug and how it was solved.
    Fix,
    /// An approach that failed or broke something, and why to avoid it.
    Pitfall,
    /// An approach that works well and is worth reusing.
    Pattern,
    /// A design or architecture choice and the reasoning behind it.
    Decision,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Note, Kind::Fix, Kind::Pitfall, Kind::Pattern, Kind::Decision];

    /// The stored and typed form: `note`, `fix`, `pitfall`, `pattern`, `decision`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Fix => "fix",
            Kind::Pitfall => "pitfall",
            Kind::Pattern => "pattern",
            Kind::Decision => "decision",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Note => "Note",
            Kind::Fix => "Fix",
            Kind::Pitfall => "Pitfall",
            Kind::Pattern => "Pattern",
            Kind::Decision => "Decision",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            Kind::Note => "Notes",
            Kind::Fix => "Fixes",
            Kind::Pitfall => "Pitfalls",
            Kind::Pattern => "Patterns",
            Kind::Decision => "Decisions",
        }
    }

    /// One line on when to file a page under this kind.
    pub fn hint(self) -> &'static str {
        match self {
            Kind::Note => "A freeform Markdown note, guide, or reference",
            Kind::Fix => "A bug whose cause wasn't obvious, and how it was solved",
            Kind::Pitfall => "Something tried that failed or broke, and why to avoid it",
            Kind::Pattern => "An approach that worked well and applies elsewhere",
            Kind::Decision => "A choice that was settled, and the reasoning behind it",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Kind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Kind> {
        let s = s.trim().to_ascii_lowercase();
        let found = match s.as_str() {
            "" | "note" | "notes" | "article" | "doc" | "guide" => Kind::Note,
            "fix" | "fixes" | "bug" | "bugfix" => Kind::Fix,
            "pitfall" | "pitfalls" | "trap" | "gotcha" | "avoid" => Kind::Pitfall,
            "pattern" | "patterns" | "practice" | "technique" => Kind::Pattern,
            "decision" | "decisions" | "choice" => Kind::Decision,
            other => {
                return Err(Error::Invalid(format!("unknown kind `{other}`; use one of: note, fix, pitfall, pattern, decision")));
            }
        };
        Ok(found)
    }
}

/// Maximum characters for a custom topic slug.
pub const TOPIC_MAX_CHARS: usize = 32;

/// A canonical high-level category/topic in the notebook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicSpec {
    pub slug: &'static str,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
}

/// Curated top-level categories ordered by stack specificity for inference (e.g. Next.js before React).
pub const CANONICAL_TOPICS: &[TopicSpec] = &[
    TopicSpec { slug: "nextjs", label: "Next.js", aliases: &["nextjs", "next", "next-js", "next.js", "app-router", "rsc", "vercel"] },
    TopicSpec { slug: "react-native", label: "React Native", aliases: &["react-native", "reactnative", "rn", "expo", "metro"] },
    TopicSpec { slug: "react", label: "React", aliases: &["react", "reactjs", "react-dom", "jsx", "hooks", "redux", "zustand"] },
    TopicSpec {
        slug: "rust",
        label: "Rust",
        aliases: &["rust", "rustc", "cargo", "crate", "crates", "serde", "tokio", "rune", "rune-kit", "rusqlite", "tauri"],
    },
    TopicSpec {
        slug: "swift",
        label: "Swift & iOS",
        aliases: &["swift", "swiftui", "ios", "macos", "xcode", "appkit", "uikit", "cocoapods"],
    },
    TopicSpec {
        slug: "typescript",
        label: "TypeScript",
        aliases: &["typescript", "ts", "javascript", "js", "node", "nodejs", "bun", "pnpm", "vite"],
    },
    TopicSpec {
        slug: "agents",
        label: "Agents & MCP",
        aliases: &["agents", "agent", "mcp", "ai", "llm", "prompts", "skills", "claude", "codex"],
    },
    TopicSpec {
        slug: "data",
        label: "Data & SQLite",
        aliases: &["data", "database", "databases", "sqlite", "postgres", "postgresql", "sql", "fts", "fts5", "drizzle", "prisma", "redis"],
    },
    TopicSpec {
        slug: "design",
        label: "Design & UI",
        aliases: &[
            "design",
            "ui",
            "ux",
            "css",
            "tailwind",
            "layout",
            "typography",
            "animation",
            "motion",
            "theme",
            "accessibility",
            "a11y",
            "images",
        ],
    },
    TopicSpec {
        slug: "tooling",
        label: "Tooling & Git",
        aliases: &["tooling", "tools", "git", "ci", "cli", "build", "testing", "test", "devops", "docker", "nix"],
    },
    TopicSpec { slug: "taste", label: "Taste", aliases: &[] },
];

/// Normalizes a typed or stored category/topic into a canonical slug (or custom slug).
pub fn normalize_topic(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered == "general" {
        return Some("general".into());
    }
    for spec in CANONICAL_TOPICS {
        if spec.slug == lowered || spec.label.eq_ignore_ascii_case(trimmed) || spec.aliases.contains(&lowered.as_str()) {
            return Some(spec.slug.to_string());
        }
    }
    let slug = crate::slugify(trimmed);
    if slug.is_empty() || (slug == "untitled" && lowered != "untitled") {
        return None;
    }
    Some(slug.chars().take(TOPIC_MAX_CHARS).collect())
}

/// Human-readable label for a category/topic slug.
pub fn topic_label(slug: &str) -> String {
    let clean = slug.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("general") {
        return "General".into();
    }
    if let Some(spec) = CANONICAL_TOPICS.iter().find(|s| s.slug == clean) {
        return spec.label.to_string();
    }
    clean
        .split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|f| f.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Picks the best high-level category from tags first, then title/body words.
pub fn infer_topic(tags: &[String], title: &str, body: &str) -> String {
    let norm_tags: Vec<String> = tags.iter().map(|t| crate::slugify(t.trim().trim_start_matches('#'))).collect();
    for spec in CANONICAL_TOPICS {
        if norm_tags.iter().any(|t| t == spec.slug || spec.aliases.contains(&t.as_str())) {
            return spec.slug.to_string();
        }
    }
    let words: HashSet<String> = title
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '.')
        .chain(body.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '.').take(256))
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 2)
        .collect();
    for spec in CANONICAL_TOPICS {
        if words.contains(spec.slug) || spec.aliases.iter().any(|a| words.contains(*a)) {
            return spec.slug.to_string();
        }
    }
    "general".into()
}

/// Formats legacy structured fields (`what_happened`, optional `why`, `do_this`) into clean Markdown.
pub fn compose_legacy_body(context: &str, cause: Option<&str>, rule: &str) -> String {
    let mut parts = Vec::new();
    let ctx = context.trim();
    if !ctx.is_empty() {
        parts.push(format!("## What happened\n\n{ctx}"));
    }
    if let Some(c) = cause.map(str::trim).filter(|c| !c.is_empty()) {
        parts.push(format!("## Why\n\n{c}"));
    }
    let r = rule.trim();
    if !r.is_empty() {
        parts.push(format!("## Takeaway\n\n{r}"));
    }
    parts.join("\n\n")
}

/// Whether a page is in the notebook's working set or kept only for the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Standing {
    #[default]
    Current,
    /// Outdated or superseded: hidden from search by default, kept for history.
    Archived,
}

impl Standing {
    fn as_str(self) -> &'static str {
        match self {
            Standing::Current => "current",
            Standing::Archived => "archived",
        }
    }
}

/// One Markdown article in the notebook.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub id: i64,
    pub kind: Kind,
    /// High-level category (`rust`, `react`, `nextjs`, `design`, `general`, or custom).
    pub topic: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Freeform Markdown content of the article.
    pub body: String,
    /// The project the note came from, if any.
    pub project_id: Option<i64>,
    /// Who wrote the latest revision.
    pub agent: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
    pub standing: Standing,
    /// How many times an agent or the user marked the page as having helped.
    pub helped: i64,
    /// The person wrote or edited this page by hand at some point, so curators only add to it.
    pub human_edited: bool,
}

impl Page {
    pub fn is_archived(&self) -> bool {
        self.standing == Standing::Archived
    }

    pub fn topic_label(&self) -> String {
        topic_label(&self.topic)
    }

    /// Plain-text one-line preview extracted from the Markdown body for lists and search hits.
    pub fn excerpt(&self, max_chars: usize) -> String {
        markdown_excerpt(&self.body, max_chars)
    }

    /// The page as clean Markdown, for the CLI, MCP, and the app's Copy Markdown action.
    pub fn to_markdown(&self, project: Option<&str>, now: i64) -> String {
        let mut meta = vec![self.topic_label()];
        if self.kind != Kind::Note {
            meta.push(self.kind.label().to_string());
        }
        if !self.tags.is_empty() {
            meta.push(self.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        if let Some(p) = project {
            meta.push(format!("from {p}"));
        }
        meta.push(format!("rev {} by {} · {}", self.revision, self.agent, ago(self.updated_at, now)));
        if self.helped > 0 {
            meta.push(format!("helped {}×", self.helped));
        }
        if self.is_archived() {
            meta.push("archived".into());
        }
        format!("# {}\n_{}_\n\n{}\n", self.title, meta.join(" · "), self.body.trim())
    }

    /// One line for search results: id, category, title, and a short excerpt.
    pub fn summary_line(&self) -> String {
        let mut badge = self.topic_label();
        if self.kind != Kind::Note {
            badge.push_str(" · ");
            badge.push_str(self.kind.label());
        }
        let mut trail = Vec::new();
        if !self.tags.is_empty() {
            trail.push(self.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        if self.helped > 0 {
            trail.push(format!("helped {}×", self.helped));
        }
        let suffix = if trail.is_empty() { String::new() } else { format!(" ({})", trail.join(" · ")) };
        let preview = self.excerpt(140);
        if preview.is_empty() {
            format!("#{} [{}] {}{suffix}", self.id, badge, self.title)
        } else {
            format!("#{} [{}] {} — {preview}{suffix}", self.id, badge, self.title)
        }
    }
}

/// What a page says, before it's saved. [`PageDraft::checked`] normalizes it and checks limits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageDraft {
    pub kind: Kind,
    /// Optional high-level category; inferred from tags and content when omitted.
    pub topic: Option<String>,
    pub title: String,
    pub tags: Vec<String>,
    /// Freeform Markdown content.
    pub body: String,
}

impl PageDraft {
    /// Starts a freeform Markdown article draft.
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self { kind: Kind::Note, topic: None, title: title.into(), tags: Vec::new(), body: body.into() }
    }

    /// Builds a draft from legacy structured fields (`what_happened`, `why`, `do_this`).
    pub fn from_legacy(
        kind: Kind,
        topic: Option<String>,
        title: String,
        tags: Vec<String>,
        context: String,
        cause: Option<String>,
        rule: String,
    ) -> Self {
        let body = compose_legacy_body(&context, cause.as_deref(), &rule);
        Self { kind, topic, title, tags, body }
    }

    /// Trims title/body, normalizes category and optional tags, and enforces limits & secret checks.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the first check that failed.
    pub fn checked(self) -> Result<PageDraft> {
        let title = squash(&self.title).trim_end_matches('.').to_string();
        let body = self.body.trim().to_string();
        let tags = normalize_tags(&self.tags);
        let topic = self.topic.as_deref().and_then(normalize_topic).unwrap_or_else(|| infer_topic(&tags, &title, &body));

        within("title", &title, TITLE_CHARS, "give the note a clear title")?;
        within("body", &body, BODY_CHARS, "write the note content in Markdown")?;
        if !TAG_COUNT.contains(&tags.len()) {
            return Err(Error::Invalid(format!("keep tags to {} or fewer (got {})", TAG_COUNT.end(), tags.len())));
        }
        for text in [&topic, &title, &body] {
            if let Some(what) = secret_in(text) {
                return Err(Error::Invalid(format!(
                    "this note looks like it contains {what}; the notebook is shared across projects, so leave secrets out"
                )));
            }
        }
        Ok(PageDraft { kind: self.kind, topic: Some(topic), title, tags, body })
    }

    pub fn resolved_topic(&self) -> String {
        self.topic.as_deref().and_then(normalize_topic).unwrap_or_else(|| infer_topic(&self.tags, &self.title, &self.body))
    }
}

/// What to do when a new page reads like one already in the notebook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnSimilar {
    /// Refuse, naming the existing page, so the writer revises it instead (agents).
    Refuse,
    /// Refuse duplicates in the same project or shared Notebook scope.
    RefuseInScope,
    /// File it anyway (the user curating by hand, or an agent that checked and disagrees).
    Allow,
}

/// Which pages a listing or search covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageFilter {
    pub kind: Option<Kind>,
    pub topic: Option<String>,
    pub tag: Option<String>,
    pub project_id: Option<i64>,
    pub include_archived: bool,
}

/// A notebook-wide count, for the app's shelves and `feeder notebook tags`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub pages: usize,
}

/// A high-level category summary, with its current page count and scoped tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicSummary {
    pub topic: String,
    pub label: String,
    pub pages: usize,
    pub tags: Vec<TagCount>,
}

const PAGE_COLS: &str = "id, kind, topic, title, tags, body, context, cause, rule, project_id, agent, created_at, updated_at, revision, standing, helped, human_edited";

impl Store {
    /// Every page that passes `filter`, the most recently revised first.
    pub fn pages(&self, filter: &PageFilter) -> Result<Vec<Page>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {PAGE_COLS} FROM notebook_pages ORDER BY updated_at DESC, id DESC"))?;
        let rows = stmt.query_map([], page_row)?;
        let all: Vec<Page> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(all.into_iter().filter(|p| passes(p, filter)).collect())
    }

    pub fn page(&self, id: i64) -> Result<Option<Page>> {
        Ok(self.conn.query_row(&format!("SELECT {PAGE_COLS} FROM notebook_pages WHERE id = ?1"), params![id], page_row).optional()?)
    }

    /// Pages matching `query`, best first: title and tags count most, then category, then body.
    /// An empty query lists the filter's pages.
    pub fn search_pages(&self, query: &str, filter: &PageFilter, limit: usize) -> Result<Vec<Page>> {
        self.search_notebook(query, filter, limit, |_| true)
    }

    /// Searches the given project and the shared Notebook, without returning other projects' notes.
    pub fn search_project_notes(&self, project_id: i64, query: &str, filter: &PageFilter, limit: usize) -> Result<Vec<Page>> {
        self.search_notebook(query, filter, limit, |page| page.project_id.is_none_or(|id| id == project_id))
    }

    fn search_notebook(&self, query: &str, filter: &PageFilter, limit: usize, in_scope: impl Fn(&Page) -> bool) -> Result<Vec<Page>> {
        let limit = limit.max(1);
        let Some(fts) = fts_terms(query) else {
            return Ok(self.pages(filter)?.into_iter().filter(in_scope).take(limit).collect());
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM notebook_pages p
             JOIN notebook_fts ON notebook_fts.rowid = p.id
             WHERE notebook_fts MATCH ?1
             ORDER BY bm25(notebook_fts, 4.0, 6.0, 4.0, 2.0) - (p.helped * 0.15), p.updated_at DESC",
            PAGE_COLS.split(", ").map(|c| format!("p.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let rows = stmt.query_map(params![fts], page_row)?;
        let mut hits: Vec<Page> =
            rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter(|p| passes(p, filter) && in_scope(p)).collect();
        if hits.is_empty() {
            // FTS needs whole-token prefixes; fall back to substrings so "busy_time" still finds pages.
            let needle = query.trim().to_lowercase();
            hits = self
                .pages(filter)?
                .into_iter()
                .filter(in_scope)
                .filter(|p| {
                    [&p.topic, &p.title, &p.body].iter().any(|t| t.to_lowercase().contains(&needle))
                        || p.topic_label().to_lowercase().contains(&needle)
                        || p.tags.iter().any(|t| t.contains(&needle))
                })
                .collect();
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Current pages that read like `draft`: the same note by title, best match first.
    pub fn similar_pages(&self, draft: &PageDraft) -> Result<Vec<Page>> {
        let want = title_words(&draft.title);
        let mut scored: Vec<(f32, Page)> = self
            .pages(&PageFilter::default())?
            .into_iter()
            .map(|p| (jaccard(&want, &title_words(&p.title)), p))
            .filter(|(score, p)| *score >= SIMILAR_TITLE || p.title.eq_ignore_ascii_case(&draft.title))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(scored.into_iter().map(|(_, p)| p).collect())
    }

    /// Files a new Markdown article.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the draft falls short of limits, or, with [`OnSimilar::Refuse`],
    /// when a current page already covers the same topic by title.
    pub fn create_page(&self, draft: PageDraft, author: &str, project_id: Option<i64>, on_similar: OnSimilar) -> Result<Page> {
        let draft = draft.checked()?;
        let topic = draft.resolved_topic();
        if on_similar != OnSimilar::Allow
            && let Some(existing) =
                self.similar_pages(&draft)?.into_iter().find(|p| on_similar == OnSimilar::Refuse || p.project_id == project_id)
        {
            return Err(Error::Invalid(format!(
                "the notebook already has a page on this: #{} \"{}\". Revise it (pass id {}) rather than adding another, or file this as new only if it's a genuinely different note",
                existing.id, existing.title, existing.id
            )));
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO notebook_pages (kind, topic, title, tags, body, context, cause, rule, project_id, agent, created_at, updated_at, human_edited)
             VALUES (?1, ?2, ?3, ?4, ?5, '', NULL, '', ?6, ?7, ?8, ?8, ?9)",
            params![
                draft.kind.as_str(),
                topic,
                draft.title,
                draft.tags.join(" "),
                draft.body,
                project_id,
                author_name(author),
                now,
                author_name(author) == HUMAN_AUTHOR
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.page(id)?.ok_or_else(|| Error::NotFound(format!("page {id} vanished")))
    }

    /// Rewrites an existing page. The originating project stays; the revision count increments.
    pub fn revise_page(&self, id: i64, draft: PageDraft, author: &str) -> Result<Page> {
        let draft = draft.checked()?;
        let topic = draft.resolved_topic();
        let changed = self.conn.execute(
            "UPDATE notebook_pages
             SET kind = ?1, topic = ?2, title = ?3, tags = ?4, body = ?5, context = '', cause = NULL, rule = '',
                 agent = ?6, updated_at = ?7, revision = revision + 1, human_edited = human_edited OR ?9
             WHERE id = ?8",
            params![
                draft.kind.as_str(),
                topic,
                draft.title,
                draft.tags.join(" "),
                draft.body,
                author_name(author),
                now_ms(),
                id,
                author_name(author) == HUMAN_AUTHOR
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no notebook page {id}")));
        }
        self.page(id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {id}")))
    }

    /// Toggles the `task_index`-th GFM task checkbox (`- [ ]` / `- [x]`) in a page's Markdown body.
    pub fn toggle_page_task(&self, id: i64, task_index: usize, checked: bool) -> Result<Page> {
        let page = self.page(id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {id}")))?;
        let Some(updated) = toggle_markdown_task(&page.body, task_index, checked) else {
            return Ok(page);
        };
        self.conn.execute("UPDATE notebook_pages SET body = ?1, updated_at = ?2 WHERE id = ?3", params![updated, now_ms(), id])?;
        self.page(id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {id}")))
    }

    pub fn set_page_standing(&self, id: i64, standing: Standing) -> Result<()> {
        let changed = self.conn.execute("UPDATE notebook_pages SET standing = ?1 WHERE id = ?2", params![standing.as_str(), id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no notebook page {id}")));
        }
        Ok(())
    }

    /// Records that a page helped, so the pages that keep paying off rise in search.
    pub fn mark_page_helped(&self, id: i64) -> Result<Page> {
        let changed = self.conn.execute("UPDATE notebook_pages SET helped = helped + 1 WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no notebook page {id}")));
        }
        self.page(id)?.ok_or_else(|| Error::NotFound(format!("no notebook page {id}")))
    }

    pub fn delete_page(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM notebook_pages WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Tags across current pages, the most used first.
    pub fn page_tags(&self) -> Result<Vec<TagCount>> {
        Ok(tag_counts(&self.pages(&PageFilter::default())?))
    }

    /// High-level categories across current pages, the largest first.
    pub fn page_topics(&self) -> Result<Vec<TopicSummary>> {
        Ok(topic_counts(&self.pages(&PageFilter::default())?))
    }
}

/// High-level categories across current `pages`, sorted by page count descending then label A–Z.
pub fn topic_counts(pages: &[Page]) -> Vec<TopicSummary> {
    let mut grouped: HashMap<String, Vec<&Page>> = HashMap::new();
    for p in pages.iter().filter(|p| !p.is_archived()) {
        let slug = if p.topic.trim().is_empty() { infer_topic(&p.tags, &p.title, &p.body) } else { p.topic.clone() };
        grouped.entry(slug).or_default().push(p);
    }
    let mut out: Vec<TopicSummary> = grouped
        .into_iter()
        .map(|(topic, group)| {
            let mut tag_map: HashMap<&str, usize> = HashMap::new();
            for pg in &group {
                for t in &pg.tags {
                    *tag_map.entry(t.as_str()).or_default() += 1;
                }
            }
            let mut tags: Vec<TagCount> = tag_map.into_iter().map(|(tag, pages)| TagCount { tag: tag.to_string(), pages }).collect();
            tags.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.tag.cmp(&b.tag)));
            TopicSummary { label: topic_label(&topic), pages: group.len(), topic, tags }
        })
        .collect();
    out.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.label.cmp(&b.label)));
    out
}

/// Tags across `pages`, the most used first, then A to Z.
pub fn tag_counts(pages: &[Page]) -> Vec<TagCount> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in pages.iter().filter(|p| !p.is_archived()) {
        for t in &p.tags {
            *counts.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut out: Vec<TagCount> = counts.into_iter().map(|(tag, pages)| TagCount { tag: tag.to_string(), pages }).collect();
    out.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.tag.cmp(&b.tag)));
    out
}

/// Short lowercase slugs, no `#`, no duplicates, in the order given. Accepts "rust, sqlite" too.
pub fn normalize_tags(raw: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.iter()
        .flat_map(|t| t.split([',', ' ']))
        .map(|t| {
            let slug = crate::slugify(t.trim().trim_start_matches('#'));
            if slug == "untitled" { String::new() } else { slug }
        })
        .filter(|t| TAG_CHARS.contains(&t.chars().count()))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// Extracts a concise plain-text preview line from a Markdown body (skipping fences and markers).
pub fn markdown_excerpt(md: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut in_fence = false;
    let mut parts = Vec::new();
    for raw in md.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.is_empty() || trimmed.starts_with("---") || trimmed.starts_with("***") {
            continue;
        }
        // Skip GFM table separator lines like `| --- | --- |`
        if trimmed.starts_with('|') && trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
            continue;
        }
        // Strip Obsidian callout tags (`> [!note]` or `> [!tip] Custom title`), keeping any custom title.
        let line_for_strip = if trimmed.starts_with('>') {
            let inner = trimmed.trim_start_matches('>').trim();
            if let Some(after_open) = inner.strip_prefix("[!")
                && let Some(end) = after_open.find(']')
            {
                let rest = after_open[end + 1..].trim();
                if rest.is_empty() {
                    continue;
                }
                rest
            } else {
                trimmed
            }
        } else {
            trimmed
        };
        let cleaned = strip_markdown_line(line_for_strip);
        if !cleaned.is_empty() {
            parts.push(cleaned);
            let joined_len: usize = parts.iter().map(|s| s.chars().count() + 1).sum();
            if joined_len >= max_chars + 16 {
                break;
            }
        }
    }
    let joined = parts.join(" ");
    let mut chars = joined.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let trimmed = head.trim_end_matches(|c: char| !c.is_alphanumeric()).trim_end();
        format!("{trimmed}…")
    } else {
        head
    }
}

fn strip_markdown_line(line: &str) -> String {
    let mut s = line.trim();
    // Strip leading quote markers, headings, list bullets, ordered markers, checkboxes.
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim_start();
    }
    if s.starts_with('#') {
        s = s.trim_start_matches('#').trim_start();
    }
    for prefix in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "* [X] ", "- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim_start();
            break;
        }
    }
    if let Some(dot) = s.find(". ")
        && dot > 0
        && dot <= 4
        && s[..dot].chars().all(|c| c.is_ascii_digit())
    {
        s = s[dot + 2..].trim_start();
    }
    // Strip inline `[label](url)` -> `label`, and emphasis/code punctuation.
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close_br) = (i + 1..chars.len()).find(|&j| chars[j] == ']')
            && close_br + 1 < chars.len()
            && chars[close_br + 1] == '('
            && let Some(close_par) = (close_br + 2..chars.len()).find(|&j| chars[j] == ')')
        {
            push_stripped_inline(&chars[i + 1..close_br], &mut out);
            i = close_par + 1;
            continue;
        }
        if chars[i] == '~' && chars.get(i + 1) == Some(&'~') {
            i += 2;
            continue;
        }
        let ch = chars[i];
        let keep_underscore =
            ch == '_' && i > 0 && chars[i - 1].is_alphanumeric() && chars.get(i + 1).is_some_and(|next| next.is_alphanumeric());
        if keep_underscore || !matches!(ch, '*' | '_' | '`') {
            out.push(ch);
        }
        i += 1;
    }
    squash(&out)
}

fn push_stripped_inline(chars: &[char], out: &mut String) {
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' && chars.get(i + 1) == Some(&'~') {
            i += 2;
            continue;
        }
        let ch = chars[i];
        let keep_underscore =
            ch == '_' && i > 0 && chars[i - 1].is_alphanumeric() && chars.get(i + 1).is_some_and(|next| next.is_alphanumeric());
        if keep_underscore || !matches!(ch, '*' | '_' | '`') {
            out.push(ch);
        }
        i += 1;
    }
}

/// Toggles the `task_index`-th GFM task list checkbox outside fenced code blocks.
pub fn toggle_markdown_task(md: &str, task_index: usize, checked: bool) -> Option<String> {
    let mut in_fence = false;
    let mut seen = 0usize;
    let mut out = String::with_capacity(md.len());
    let mut changed = false;
    for chunk in md.split_inclusive('\n') {
        let trimmed = chunk.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(chunk);
            continue;
        }
        if !in_fence && let Some(box_offset) = find_task_box_offset(chunk) {
            if seen == task_index {
                let mark = if checked { 'x' } else { ' ' };
                out.push_str(&chunk[..box_offset]);
                out.push(mark);
                out.push_str(&chunk[box_offset + 1..]);
                changed = true;
                seen += 1;
                continue;
            }
            seen += 1;
        }
        out.push_str(chunk);
    }
    changed.then_some(out)
}

/// Returns the byte index of the character inside `[ ]` / `[x]` for a GFM task list line.
fn find_task_box_offset(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'>') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    if matches!(bytes[i], b'-' | b'*' | b'+') {
        i += 1;
    } else if bytes[i].is_ascii_digit() {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && matches!(bytes[i], b'.' | b')') {
            i += 1;
        } else {
            return None;
        }
    } else {
        return None;
    }
    if i >= bytes.len() || !matches!(bytes[i], b' ' | b'\t') {
        return None;
    }
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if i + 3 <= bytes.len()
        && bytes[i] == b'['
        && matches!(bytes[i + 1], b' ' | b'x' | b'X')
        && bytes[i + 2] == b']'
        && (i + 3 == bytes.len() || matches!(bytes[i + 3], b' ' | b'\t' | b'\r' | b'\n'))
    {
        Some(i + 1)
    } else {
        None
    }
}

fn passes(p: &Page, f: &PageFilter) -> bool {
    (f.include_archived || !p.is_archived())
        && f.kind.is_none_or(|k| p.kind == k)
        && f.topic.as_deref().is_none_or(|t| {
            let want = normalize_topic(t).unwrap_or_else(|| t.to_ascii_lowercase());
            p.topic == want
        })
        && f.tag.as_deref().is_none_or(|t| {
            let trimmed = t.trim().trim_start_matches('#');
            let want = crate::slugify(trimmed);
            p.tags.iter().any(|pt| pt == &want || pt.eq_ignore_ascii_case(trimmed))
        })
        && f.project_id.is_none_or(|id| p.project_id == Some(id))
}

fn within(part: &str, text: &str, range: std::ops::RangeInclusive<usize>, advice: &str) -> Result<()> {
    let n = text.chars().count();
    if n < *range.start() {
        return Err(Error::Invalid(format!("`{part}` is too short ({n} characters); {advice}")));
    }
    if n > *range.end() {
        return Err(Error::Invalid(format!("`{part}` is {n} characters; keep it under {}", range.end())));
    }
    Ok(())
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn author_name(a: &str) -> &str {
    let a = a.trim();
    if a.is_empty() { "Unknown agent" } else { a }
}

/// Words that carry meaning in a title, for telling two pages on the same note apart.
fn title_words(title: &str) -> HashSet<String> {
    const STOP: [&str; 24] = [
        "the", "and", "for", "with", "when", "while", "from", "into", "that", "this", "its", "are", "was", "not", "use", "using", "via",
        "how", "why", "what", "after", "before", "over", "under",
    ];
    title
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count() as f32;
    shared / a.union(b).count() as f32
}

/// What kind of secret `text` seems to hold, if any.
pub fn secret_in(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("a private key");
    }
    for word in text.split(|c: char| c.is_whitespace() || "\"'`(),;=".contains(c)) {
        let long_tail = |prefix: &str| {
            word.strip_prefix(prefix)
                .is_some_and(|rest| rest.len() >= 16 && rest.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)))
        };
        if ["sk-", "sk_live_", "sk_test_", "rk_live_", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "glpat-", "AIza"]
            .iter()
            .any(|p| long_tail(p))
        {
            return Some("an API key or token");
        }
        if word.len() == 20 && word.starts_with("AKIA") && word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            return Some("an AWS access key");
        }
    }
    None
}

/// A safe FTS5 prefix query (`"tok1"* "tok2"*`), or None when nothing searchable is left.
fn fts_terms(query: &str) -> Option<String> {
    let terms: Vec<String> =
        query.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| format!("\"{}\"*", t.replace('"', "\"\""))).collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn page_row(r: &Row<'_>) -> rusqlite::Result<Page> {
    let kind: String = r.get(1)?;
    let raw_topic: String = r.get(2)?;
    let title: String = r.get(3)?;
    let tags_raw: String = r.get(4)?;
    let tags: Vec<String> = tags_raw.split_whitespace().map(str::to_string).collect();
    let raw_body: String = r.get(5)?;
    let body = if raw_body.trim().is_empty() {
        let context: String = r.get(6)?;
        let cause: Option<String> = r.get(7)?;
        let rule: String = r.get(8)?;
        compose_legacy_body(&context, cause.as_deref(), &rule)
    } else {
        raw_body
    };
    let topic = normalize_topic(&raw_topic).unwrap_or_else(|| infer_topic(&tags, &title, &body));
    let standing: String = r.get(14)?;
    Ok(Page {
        id: r.get(0)?,
        kind: kind.parse().unwrap_or(Kind::Note),
        topic,
        title,
        tags,
        body,
        project_id: r.get(9)?,
        agent: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        revision: r.get(13)?,
        standing: if standing == "archived" { Standing::Archived } else { Standing::Current },
        helped: r.get(15)?,
        human_edited: r.get(16)?,
    })
}

/// Creates the notebook's table and its full-text index (kept in step by triggers).
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notebook_pages (
            id INTEGER PRIMARY KEY,
            kind TEXT NOT NULL DEFAULT 'note',
            topic TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL DEFAULT '',
            context TEXT NOT NULL DEFAULT '',
            cause TEXT,
            rule TEXT NOT NULL DEFAULT '',
            project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            agent TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            revision INTEGER NOT NULL DEFAULT 1,
            standing TEXT NOT NULL DEFAULT 'current',
            helped INTEGER NOT NULL DEFAULT 0,
            human_edited INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS notebook_by_updated ON notebook_pages(updated_at DESC, id DESC);",
    )?;

    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(notebook_pages)")?;
        let rows = stmt.query_map([], |r| r.get(1))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !cols.iter().any(|c| c == "topic") {
        conn.execute("ALTER TABLE notebook_pages ADD COLUMN topic TEXT NOT NULL DEFAULT ''", [])?;
    }
    if !cols.iter().any(|c| c == "body") {
        conn.execute("ALTER TABLE notebook_pages ADD COLUMN body TEXT NOT NULL DEFAULT ''", [])?;
    }
    if !cols.iter().any(|c| c == "human_edited") {
        conn.execute("ALTER TABLE notebook_pages ADD COLUMN human_edited INTEGER NOT NULL DEFAULT 0", [])?;
        // Before this column, the latest author is all there is to go on.
        conn.execute("UPDATE notebook_pages SET human_edited = 1 WHERE agent = ?1", params![HUMAN_AUTHOR])?;
    }

    // Backfill Markdown `body` for legacy structured rows whose `body` is still empty.
    let needs_body: Vec<(i64, String, Option<String>, String)> = {
        let mut stmt = conn.prepare("SELECT id, context, cause, rule FROM notebook_pages WHERE trim(body) = ''")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, context, cause, rule) in needs_body {
        let body = compose_legacy_body(&context, cause.as_deref(), &rule);
        if !body.is_empty() {
            conn.execute("UPDATE notebook_pages SET body = ?1 WHERE id = ?2", params![body, id])?;
        }
    }

    // Backfill any page whose topic is still empty.
    let needs_topic: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, tags, title, body FROM notebook_pages WHERE trim(topic) = ''")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, tags_raw, title, body) in needs_topic {
        let tags: Vec<String> = tags_raw.split_whitespace().map(str::to_string).collect();
        let topic = infer_topic(&tags, &title, &body);
        conn.execute("UPDATE notebook_pages SET topic = ?1 WHERE id = ?2", params![topic, id])?;
    }

    let fts_sql: Option<String> =
        conn.query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notebook_fts'", [], |r| r.get(0)).optional()?;
    let needs_fts_rebuild = match fts_sql {
        None => true,
        Some(sql) => !sql.contains("body") || !sql.contains("topic"),
    };
    if needs_fts_rebuild {
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS notebook_ai;
            DROP TRIGGER IF EXISTS notebook_ad;
            DROP TRIGGER IF EXISTS notebook_au;
            DROP TABLE IF EXISTS notebook_fts;",
        )?;
    }

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS notebook_fts USING fts5(
            topic, title, tags, body,
            content='notebook_pages', content_rowid='id', tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS notebook_ai AFTER INSERT ON notebook_pages BEGIN
            INSERT INTO notebook_fts(rowid, topic, title, tags, body)
            VALUES (new.id, new.topic, new.title, new.tags, new.body);
        END;
        CREATE TRIGGER IF NOT EXISTS notebook_ad AFTER DELETE ON notebook_pages BEGIN
            INSERT INTO notebook_fts(notebook_fts, rowid, topic, title, tags, body)
            VALUES ('delete', old.id, old.topic, old.title, old.tags, old.body);
        END;
        CREATE TRIGGER IF NOT EXISTS notebook_au AFTER UPDATE OF topic, title, tags, body ON notebook_pages BEGIN
            INSERT INTO notebook_fts(notebook_fts, rowid, topic, title, tags, body)
            VALUES ('delete', old.id, old.topic, old.title, old.tags, old.body);
            INSERT INTO notebook_fts(rowid, topic, title, tags, body)
            VALUES (new.id, new.topic, new.title, new.tags, new.body);
        END;",
    )?;
    if needs_fts_rebuild {
        conn.execute("INSERT INTO notebook_fts(notebook_fts) VALUES('rebuild')", [])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("feeder-notebook-{tag}-{}-{}", std::process::id(), fastrand::u32(..)));
        (Store::open(&dir).expect("store opens"), dir)
    }

    fn draft(title: &str) -> PageDraft {
        PageDraft {
            kind: Kind::Note,
            topic: None,
            title: title.into(),
            tags: vec!["sqlite".into(), "Rust".into()],
            body: "## Symptom\n\nReaders in the app stalled while the CLI copied a screenshot inside a write transaction.\n\n## Rule\n\nCopy media files before opening the SQLite write transaction.".into(),
        }
    }

    #[test]
    fn drafts_are_normalized_and_freeform_without_forced_tags() {
        let d = PageDraft {
            title: "  SQLite   reads stall during long writes. ".into(),
            tags: vec!["#Rust, sqlite".into(), "rust".into()],
            ..draft("x")
        };
        let ok = d.checked().expect("a good page passes");
        assert_eq!(ok.title, "SQLite reads stall during long writes");
        assert_eq!(ok.tags, ["rust", "sqlite"]);
        assert_eq!(ok.topic.as_deref(), Some("rust"));

        // Freeform notes need no tags or kind at all.
        let freeform = PageDraft::new("Rune scroll tricks", "Bound an ancestor height and add `min_h(0)`.").checked().unwrap();
        assert_eq!(freeform.kind, Kind::Note);
        assert!(freeform.tags.is_empty());
        assert_eq!(freeform.topic.as_deref(), Some("rust"));

        let explicit = PageDraft { topic: Some("Next.js".into()), ..draft("App Router streaming falls back") }.checked().unwrap();
        assert_eq!(explicit.topic.as_deref(), Some("nextjs"));
        let custom = PageDraft { topic: Some("GraphQL API".into()), ..draft("Cache headers break hydration") }.checked().unwrap();
        assert_eq!(custom.topic.as_deref(), Some("graphql-api"));
        assert_eq!(topic_label("graphql-api"), "Graphql Api");

        let empty_body = PageDraft::new("Valid title", "   ");
        assert!(empty_body.checked().unwrap_err().to_string().contains("`body` is too short"));
        let leaky = PageDraft::new("Webhook keys", "Called the API with sk-abcdefghijklmnopqrstuvwxyz123456 and it failed.");
        assert!(leaky.checked().unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn legacy_structured_db_migrates_into_markdown_body_and_fts() {
        assert_eq!(infer_topic(&["react".into(), "nextjs".into()], "Server actions", "body"), "nextjs");
        assert_eq!(infer_topic(&["react".into(), "expo".into()], "Scroll jitter", "body"), "react-native");
        assert_eq!(infer_topic(&["hooks".into()], "Effect cleanup", "body"), "react");

        let dir = std::env::temp_dir().join(format!("feeder-notebook-legacy-{}-{}", std::process::id(), fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("feeder.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE notebook_pages (
                    id INTEGER PRIMARY KEY,
                    kind TEXT NOT NULL,
                    title TEXT NOT NULL,
                    tags TEXT NOT NULL DEFAULT '',
                    context TEXT NOT NULL,
                    cause TEXT,
                    rule TEXT NOT NULL,
                    project_id INTEGER,
                    agent TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 1,
                    standing TEXT NOT NULL DEFAULT 'current',
                    helped INTEGER NOT NULL DEFAULT 0
                );
                CREATE VIRTUAL TABLE notebook_fts USING fts5(title, tags, rule, context, cause, content='notebook_pages', content_rowid='id');
                INSERT INTO notebook_pages (id, kind, title, tags, context, cause, rule, agent, created_at, updated_at)
                VALUES (1, 'pitfall', 'Reanimated worklets drop closures', 'expo react-native', 'Shared value broke.', 'JS closure not serialized.', 'Mark callbacks with worklet.', 'Codex', 1, 1);",
            )
            .unwrap();
        }
        let s = Store::open(&dir).unwrap();
        let page = s.page(1).unwrap().unwrap();
        assert_eq!(page.topic, "react-native");
        assert_eq!(page.kind, Kind::Pitfall);
        assert!(page.body.contains("## What happened"));
        assert!(page.body.contains("Shared value broke."));
        assert!(page.body.contains("## Why"));
        assert!(page.body.contains("## Takeaway"));
        let hits = s.search_pages("worklet", &PageFilter::default(), 5).unwrap();
        assert_eq!(hits.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn toggle_task_checkboxes_skips_code_fences_and_persists() {
        let (s, dir) = temp_store("tasks");
        let page = s
            .create_page(
                PageDraft::new("Release checklist", "```md\n- [ ] ignore fenced task\n```\n\n- [ ] Run clippy\n- [x] Update changelog\n"),
                "You",
                None,
                OnSimilar::Allow,
            )
            .unwrap();
        let updated = s.toggle_page_task(page.id, 0, true).unwrap();
        assert!(updated.body.contains("- [ ] ignore fenced task"));
        assert!(updated.body.contains("- [x] Run clippy"));
        assert!(updated.body.contains("- [x] Update changelog"));

        let updated2 = s.toggle_page_task(page.id, 1, false).unwrap();
        assert!(updated2.body.contains("- [ ] Update changelog"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn near_duplicates_are_refused_unless_allowed_and_revisions_count_up() {
        let (s, dir) = temp_store("dupes");
        let first = s.create_page(draft("SQLite reads stall during long writes"), "Codex", None, OnSimilar::Refuse).unwrap();
        let again = s.create_page(draft("SQLite reads stall during a long write"), "Claude Code", None, OnSimilar::Refuse);
        let msg = again.unwrap_err().to_string();
        assert!(msg.contains(&format!("#{}", first.id)), "{msg}");
        assert!(s.create_page(draft("Rune scroll viewports collapse inside grow columns"), "Claude Code", None, OnSimilar::Refuse).is_ok());
        assert!(s.create_page(draft("SQLite reads stall during a long write"), "You", None, OnSimilar::Allow).is_ok());

        let revised = s
            .revise_page(
                first.id,
                PageDraft {
                    topic: Some("data".into()),
                    body: "Keep write transactions short; copy files before opening SQLite.".into(),
                    ..draft("SQLite reads stall during long writes")
                },
                "T3 Code",
            )
            .unwrap();
        assert_eq!(revised.revision, 2);
        assert_eq!(revised.topic, "data");
        assert_eq!(revised.agent, "T3 Code");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn search_ranks_titles_and_tags_and_skips_archived_pages() {
        let (s, dir) = temp_store("search");
        let a = s.create_page(draft("SQLite reads stall during long writes"), "Codex", None, OnSimilar::Refuse).unwrap();
        let b = s
            .create_page(
                PageDraft {
                    kind: Kind::Pitfall,
                    topic: Some("rust".into()),
                    title: "Scroll viewport collapses to zero height".into(),
                    tags: vec!["rune".into(), "layout".into()],
                    body: "A grown scroll column inside an auto-height column drew nothing.\n\nBound an ancestor's height and put `min_h(0)` on each column between it and the viewport.".into(),
                },
                "Claude Code",
                None,
                OnSimilar::Refuse,
            )
            .unwrap();
        let hits = s.search_pages("scroll layout", &PageFilter::default(), 5).unwrap();
        assert_eq!(hits.first().map(|p| p.id), Some(b.id));
        let by_tag = s.search_pages("", &PageFilter { tag: Some("sqlite".into()), ..PageFilter::default() }, 5).unwrap();
        assert_eq!(by_tag.iter().map(|p| p.id).collect::<Vec<_>>(), [a.id]);
        let by_tag_hash = s.search_pages("", &PageFilter { tag: Some("#SQLite".into()), ..PageFilter::default() }, 5).unwrap();
        assert_eq!(by_tag_hash.iter().map(|p| p.id).collect::<Vec<_>>(), [a.id]);
        assert_eq!(
            markdown_excerpt("Set `busy_timeout` in ~/Library/db and ~~old~~ _italic_ text.", 120),
            "Set busy_timeout in ~/Library/db and old italic text."
        );
        let by_topic = s.search_pages("", &PageFilter { topic: Some("Rust".into()), ..PageFilter::default() }, 5).unwrap();
        assert_eq!(by_topic.len(), 2);
        let substring = s.search_pages("viewpo", &PageFilter::default(), 5).unwrap();
        assert_eq!(substring.first().map(|p| p.id), Some(b.id));

        s.set_page_standing(b.id, Standing::Archived).unwrap();
        assert!(s.search_pages("scroll", &PageFilter::default(), 5).unwrap().is_empty());
        let archived = PageFilter { include_archived: true, ..PageFilter::default() };
        assert_eq!(s.search_pages("scroll", &archived, 5).unwrap().len(), 1);

        assert_eq!(s.mark_page_helped(a.id).unwrap().helped, 1);
        let tags = s.page_tags().unwrap();
        assert_eq!(tags.iter().map(|t| t.tag.as_str()).collect::<Vec<_>>(), ["rust", "sqlite"]);
        let topics = s.page_topics().unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic, "rust");
        s.delete_page(a.id).unwrap();
        assert!(s.page(a.id).unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn kinds_and_markdown_export_work_cleanly() {
        assert_eq!("Bug".parse::<Kind>().unwrap(), Kind::Fix);
        assert_eq!("gotcha".parse::<Kind>().unwrap(), Kind::Pitfall);
        assert_eq!("article".parse::<Kind>().unwrap(), Kind::Note);
        assert!("vibe".parse::<Kind>().is_err());
        let page = Page {
            id: 3,
            kind: Kind::Decision,
            topic: "agents".into(),
            title: "Keep the MCP server stdio-only".into(),
            tags: vec!["mcp".into()],
            body: "Considered an HTTP transport, but a local port would expose the store.\n\n> [!note]\n> Ship **stdio** only.".into(),
            project_id: None,
            agent: "You".into(),
            created_at: 0,
            updated_at: 0,
            revision: 1,
            standing: Standing::Current,
            helped: 2,
            human_edited: false,
        };
        let md = page.to_markdown(Some("Feeder"), 0);
        assert!(md.starts_with("# Keep the MCP server stdio-only\n"));
        assert!(md.contains("Agents & MCP · Decision"));
        assert!(md.contains("Ship **stdio** only."));
        assert!(page.excerpt(80).contains("Considered an HTTP transport"));
    }
}
