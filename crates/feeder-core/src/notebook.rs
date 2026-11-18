//! The notebook: one shared, searchable record of what was learned while building, across every
//! project and every agent. Each page is a single lesson in four parts (title, what happened,
//! why, do this), filed as a fix, a pitfall, a pattern or a decision, and tagged by area.
//!
//! The bar is deliberately high so the notebook stays worth reading: every page is checked for
//! shape and length, near-duplicates are refused in favour of revising the page that exists, and
//! anything that looks like a secret is turned away.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Error, Result, Store, ago, now_ms};

/// Title length, in characters.
pub const TITLE_CHARS: std::ops::RangeInclusive<usize> = 6..=90;
/// "What happened" length, in characters.
pub const CONTEXT_CHARS: std::ops::RangeInclusive<usize> = 12..=700;
/// "Do this" length, in characters: one or two sentences.
pub const RULE_CHARS: std::ops::RangeInclusive<usize> = 12..=320;
/// "Why" is optional, but no longer than this.
pub const MAX_CAUSE_CHARS: usize = 500;
/// Tags per page.
pub const TAG_COUNT: std::ops::RangeInclusive<usize> = 1..=6;
/// Characters per tag.
pub const TAG_CHARS: std::ops::RangeInclusive<usize> = 2..=24;
/// Title-word overlap (Jaccard) at which two pages count as the same lesson.
const SIMILAR_TITLE: f32 = 0.6;

/// What sort of lesson a page records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
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
    pub const ALL: [Kind; 4] = [Kind::Fix, Kind::Pitfall, Kind::Pattern, Kind::Decision];

    /// The stored and typed form: `fix`, `pitfall`, `pattern`, `decision`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fix => "fix",
            Kind::Pitfall => "pitfall",
            Kind::Pattern => "pattern",
            Kind::Decision => "decision",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Fix => "Fix",
            Kind::Pitfall => "Pitfall",
            Kind::Pattern => "Pattern",
            Kind::Decision => "Decision",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            Kind::Fix => "Fixes",
            Kind::Pitfall => "Pitfalls",
            Kind::Pattern => "Patterns",
            Kind::Decision => "Decisions",
        }
    }

    /// One line on when to file a page under this kind.
    pub fn hint(self) -> &'static str {
        match self {
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
            "fix" | "fixes" | "bug" | "bugfix" => Kind::Fix,
            "pitfall" | "pitfalls" | "trap" | "gotcha" | "avoid" => Kind::Pitfall,
            "pattern" | "patterns" | "practice" | "technique" => Kind::Pattern,
            "decision" | "decisions" | "choice" => Kind::Decision,
            other => {
                return Err(Error::Invalid(format!("unknown kind `{other}`; use one of: fix, pitfall, pattern, decision")));
            }
        };
        Ok(found)
    }
}

/// Maximum characters for a custom topic slug.
pub const TOPIC_MAX_CHARS: usize = 32;

/// A canonical high-level topic in the notebook's two-tier hierarchy (`topic` -> sub-`tags`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicSpec {
    pub slug: &'static str,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
}

/// Curated top-level topics ordered by stack specificity for inference (e.g. Next.js before React).
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
];

/// Normalizes a typed or stored topic into a canonical slug (or custom slug).
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

/// Human-readable label for a topic slug.
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

/// Picks the best high-level topic from tags first, then title/context words.
pub fn infer_topic(tags: &[String], title: &str, context: &str) -> String {
    let norm_tags: Vec<String> = tags.iter().map(|t| crate::slugify(t.trim().trim_start_matches('#'))).collect();
    for spec in CANONICAL_TOPICS {
        if norm_tags.iter().any(|t| t == spec.slug || spec.aliases.contains(&t.as_str())) {
            return spec.slug.to_string();
        }
    }
    let words: HashSet<String> = title
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '.')
        .chain(context.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '.'))
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

/// One lesson in the notebook.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub id: i64,
    pub kind: Kind,
    /// High-level grouping (`rust`, `react`, `nextjs`, `design`, ...).
    pub topic: String,
    pub title: String,
    pub tags: Vec<String>,
    /// What happened: the situation or symptom, with the exact error text if there was one.
    pub context: String,
    /// Why: the root cause or the reasoning.
    pub cause: Option<String>,
    /// Do this: the rule to follow next time.
    pub rule: String,
    /// The project the lesson came from, if any.
    pub project_id: Option<i64>,
    /// Who wrote the latest revision.
    pub agent: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
    pub standing: Standing,
    /// How many times an agent or the user marked the page as having helped.
    pub helped: i64,
}

impl Page {
    pub fn is_archived(&self) -> bool {
        self.standing == Standing::Archived
    }

    pub fn topic_label(&self) -> String {
        topic_label(&self.topic)
    }

    /// The page as Markdown, for the CLI, MCP and the app's Copy.
    pub fn to_markdown(&self, project: Option<&str>, now: i64) -> String {
        let mut meta = vec![self.topic_label(), self.kind.label().to_string()];
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
        let mut out = format!("## {} (#{})\n_{}_\n\n**What happened:** {}\n", self.title, self.id, meta.join(" · "), self.context);
        if let Some(cause) = &self.cause {
            out.push_str(&format!("\n**Why:** {cause}\n"));
        }
        out.push_str(&format!("\n**Do this:** {}\n", self.rule));
        out
    }

    /// One line for search results: id, topic, kind, title and the rule.
    pub fn summary_line(&self) -> String {
        let tags = self.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
        let helped = if self.helped > 0 { format!(" · helped {}×", self.helped) } else { String::new() };
        format!("#{} [{} · {}] {} — Do this: {} ({tags}{helped})", self.id, self.topic_label(), self.kind.label(), self.title, self.rule)
    }
}

/// What a page says, before it's filed. [`PageDraft::checked`] normalizes it and enforces the
/// notebook's bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageDraft {
    pub kind: Kind,
    /// Optional high-level topic; inferred from tags and prose when omitted.
    pub topic: Option<String>,
    pub title: String,
    pub tags: Vec<String>,
    pub context: String,
    pub cause: Option<String>,
    pub rule: String,
}

impl PageDraft {
    /// Trims every part, normalizes topic and tags to short lowercase slugs and checks every limit.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the first part that falls short of the bar, worded so an agent
    /// can fix it and try again.
    pub fn checked(self) -> Result<PageDraft> {
        let title = squash(&self.title).trim_end_matches('.').to_string();
        let context = self.context.trim().to_string();
        let rule = self.rule.trim().to_string();
        let cause = self.cause.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let tags = normalize_tags(&self.tags);
        let topic = self.topic.as_deref().and_then(normalize_topic).unwrap_or_else(|| infer_topic(&tags, &title, &context));

        within("title", &title, TITLE_CHARS, "name the situation as someone would search for it")?;
        within("what happened", &context, CONTEXT_CHARS, "describe the symptom or situation, with the exact error text if there was one")?;
        within("do this", &rule, RULE_CHARS, "state the rule to follow next time in one or two sentences")?;
        if let Some(c) = &cause
            && c.chars().count() > MAX_CAUSE_CHARS
        {
            return Err(Error::Invalid(format!("`why` is {} characters; keep the root cause under {MAX_CAUSE_CHARS}", c.chars().count())));
        }
        if !TAG_COUNT.contains(&tags.len()) {
            return Err(Error::Invalid(format!(
                "a page needs {} to {} tags (language, framework or area, e.g. `rust`, `sqlite`, `layout`); got {}",
                TAG_COUNT.start(),
                TAG_COUNT.end(),
                tags.len()
            )));
        }
        for text in [&topic, &title, &context, &rule].into_iter().chain(cause.as_ref()) {
            if let Some(what) = secret_in(text) {
                return Err(Error::Invalid(format!(
                    "this page looks like it contains {what}; the notebook is shared across projects, so leave secrets out"
                )));
            }
        }
        Ok(PageDraft { kind: self.kind, topic: Some(topic), title, tags, context, cause, rule })
    }

    pub fn resolved_topic(&self) -> String {
        self.topic.as_deref().and_then(normalize_topic).unwrap_or_else(|| infer_topic(&self.tags, &self.title, &self.context))
    }
}

/// What to do when a new page reads like one already in the notebook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnSimilar {
    /// Refuse, naming the existing page, so the writer revises it instead (agents).
    Refuse,
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

/// A high-level topic summary, with its current page count and scoped tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicSummary {
    pub topic: String,
    pub label: String,
    pub pages: usize,
    pub tags: Vec<TagCount>,
}

const PAGE_COLS: &str =
    "id, kind, topic, title, tags, context, cause, rule, project_id, agent, created_at, updated_at, revision, standing, helped";

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

    /// Pages matching `query`, best first: title and tags count most, then the rule, then the
    /// rest. An empty query lists the filter's pages.
    pub fn search_pages(&self, query: &str, filter: &PageFilter, limit: usize) -> Result<Vec<Page>> {
        let limit = limit.max(1);
        let Some(fts) = fts_terms(query) else {
            let mut all = self.pages(filter)?;
            all.truncate(limit);
            return Ok(all);
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM notebook_pages p
             JOIN notebook_fts ON notebook_fts.rowid = p.id
             WHERE notebook_fts MATCH ?1
             ORDER BY bm25(notebook_fts, 4.0, 6.0, 4.0, 3.0, 1.0, 1.0) - (p.helped * 0.15), p.updated_at DESC",
            PAGE_COLS.split(", ").map(|c| format!("p.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let rows = stmt.query_map(params![fts], page_row)?;
        let mut hits: Vec<Page> = rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter(|p| passes(p, filter)).collect();
        if hits.is_empty() {
            // FTS needs whole-token prefixes; fall back to substrings so "busy_time" still finds pages.
            let needle = query.trim().to_lowercase();
            hits = self
                .pages(filter)?
                .into_iter()
                .filter(|p| {
                    [&p.topic, &p.title, &p.context, &p.rule].iter().any(|t| t.to_lowercase().contains(&needle))
                        || p.topic_label().to_lowercase().contains(&needle)
                        || p.cause.as_ref().is_some_and(|c| c.to_lowercase().contains(&needle))
                        || p.tags.iter().any(|t| t.contains(&needle))
                })
                .collect();
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Current pages that read like `draft`: the same lesson by title, best match first.
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

    /// Files a new page.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the draft falls short of the bar, or, with [`OnSimilar::Refuse`],
    /// when a current page already covers the same lesson (the message names it so the writer
    /// can revise it instead).
    pub fn create_page(&self, draft: PageDraft, author: &str, project_id: Option<i64>, on_similar: OnSimilar) -> Result<Page> {
        let draft = draft.checked()?;
        let topic = draft.resolved_topic();
        if on_similar == OnSimilar::Refuse
            && let Some(existing) = self.similar_pages(&draft)?.into_iter().next()
        {
            return Err(Error::Invalid(format!(
                "the notebook already has a page on this: #{} \"{}\". Revise it (pass id {}) rather than adding another, or file this as new only if it's a genuinely different lesson",
                existing.id, existing.title, existing.id
            )));
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO notebook_pages (kind, topic, title, tags, context, cause, rule, project_id, agent, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                draft.kind.as_str(),
                topic,
                draft.title,
                draft.tags.join(" "),
                draft.context,
                draft.cause,
                draft.rule,
                project_id,
                author_name(author),
                now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.page(id)?.ok_or_else(|| Error::NotFound(format!("page {id} vanished")))
    }

    /// Rewrites a page with a better or corrected version of the same lesson. The project it came
    /// from stays; the revision count goes up.
    pub fn revise_page(&self, id: i64, draft: PageDraft, author: &str) -> Result<Page> {
        let draft = draft.checked()?;
        let topic = draft.resolved_topic();
        let changed = self.conn.execute(
            "UPDATE notebook_pages
             SET kind = ?1, topic = ?2, title = ?3, tags = ?4, context = ?5, cause = ?6, rule = ?7, agent = ?8,
                 updated_at = ?9, revision = revision + 1
             WHERE id = ?10",
            params![
                draft.kind.as_str(),
                topic,
                draft.title,
                draft.tags.join(" "),
                draft.context,
                draft.cause,
                draft.rule,
                author_name(author),
                now_ms(),
                id
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("no notebook page {id}")));
        }
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

    /// High-level topics across current pages, the largest first.
    pub fn page_topics(&self) -> Result<Vec<TopicSummary>> {
        Ok(topic_counts(&self.pages(&PageFilter::default())?))
    }
}

/// High-level topics across current `pages`, sorted by page count descending then label A–Z.
pub fn topic_counts(pages: &[Page]) -> Vec<TopicSummary> {
    let mut grouped: HashMap<String, Vec<&Page>> = HashMap::new();
    for p in pages.iter().filter(|p| !p.is_archived()) {
        let slug = if p.topic.trim().is_empty() { infer_topic(&p.tags, &p.title, &p.context) } else { p.topic.clone() };
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

fn passes(p: &Page, f: &PageFilter) -> bool {
    (f.include_archived || !p.is_archived())
        && f.kind.is_none_or(|k| p.kind == k)
        && f.topic.as_deref().is_none_or(|t| {
            let want = normalize_topic(t).unwrap_or_else(|| t.to_ascii_lowercase());
            p.topic == want
        })
        && f.tag.as_deref().is_none_or(|t| p.tags.iter().any(|pt| pt == t))
        && f.project_id.is_none_or(|id| p.project_id == Some(id))
}

fn within(part: &str, text: &str, range: std::ops::RangeInclusive<usize>, advice: &str) -> Result<()> {
    let n = text.chars().count();
    if n < *range.start() {
        return Err(Error::Invalid(format!("`{part}` is too short ({n} characters); {advice}")));
    }
    if n > *range.end() {
        return Err(Error::Invalid(format!("`{part}` is {n} characters; keep it under {} so the page stays a quick read", range.end())));
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

/// Words that carry meaning in a title, for telling two pages on the same lesson apart.
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
fn secret_in(text: &str) -> Option<&'static str> {
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
    let context: String = r.get(5)?;
    let topic = normalize_topic(&raw_topic).unwrap_or_else(|| infer_topic(&tags, &title, &context));
    let standing: String = r.get(13)?;
    Ok(Page {
        id: r.get(0)?,
        kind: kind.parse().unwrap_or(Kind::Pattern),
        topic,
        title,
        tags,
        context,
        cause: r.get(6)?,
        rule: r.get(7)?,
        project_id: r.get(8)?,
        agent: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        revision: r.get(12)?,
        standing: if standing == "archived" { Standing::Archived } else { Standing::Current },
        helped: r.get(14)?,
    })
}

/// Creates the notebook's table and its full-text index (kept in step by triggers).
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notebook_pages (
            id INTEGER PRIMARY KEY,
            kind TEXT NOT NULL,
            topic TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '',
            context TEXT NOT NULL,
            cause TEXT,
            rule TEXT NOT NULL,
            project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            agent TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            revision INTEGER NOT NULL DEFAULT 1,
            standing TEXT NOT NULL DEFAULT 'current',
            helped INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS notebook_by_updated ON notebook_pages(updated_at DESC, id DESC);",
    )?;

    let mut has_topic_col = false;
    {
        let mut stmt = conn.prepare("PRAGMA table_info(notebook_pages)")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == "topic" {
                has_topic_col = true;
                break;
            }
        }
    }
    if !has_topic_col {
        conn.execute("ALTER TABLE notebook_pages ADD COLUMN topic TEXT NOT NULL DEFAULT ''", [])?;
    }

    // Backfill any page whose topic is still empty (either freshly migrated or legacy writes).
    let needs_backfill: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, tags, title, context FROM notebook_pages WHERE trim(topic) = ''")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, tags_raw, title, context) in needs_backfill {
        let tags: Vec<String> = tags_raw.split_whitespace().map(str::to_string).collect();
        let topic = infer_topic(&tags, &title, &context);
        conn.execute("UPDATE notebook_pages SET topic = ?1 WHERE id = ?2", params![topic, id])?;
    }

    let fts_sql: Option<String> =
        conn.query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notebook_fts'", [], |r| r.get(0)).optional()?;
    let needs_fts_rebuild = match fts_sql {
        None => true,
        Some(sql) => !sql.contains("topic"),
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
            topic, title, tags, rule, context, cause,
            content='notebook_pages', content_rowid='id', tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS notebook_ai AFTER INSERT ON notebook_pages BEGIN
            INSERT INTO notebook_fts(rowid, topic, title, tags, rule, context, cause)
            VALUES (new.id, new.topic, new.title, new.tags, new.rule, new.context, COALESCE(new.cause, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS notebook_ad AFTER DELETE ON notebook_pages BEGIN
            INSERT INTO notebook_fts(notebook_fts, rowid, topic, title, tags, rule, context, cause)
            VALUES ('delete', old.id, old.topic, old.title, old.tags, old.rule, old.context, COALESCE(old.cause, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS notebook_au AFTER UPDATE OF topic, title, tags, rule, context, cause ON notebook_pages BEGIN
            INSERT INTO notebook_fts(notebook_fts, rowid, topic, title, tags, rule, context, cause)
            VALUES ('delete', old.id, old.topic, old.title, old.tags, old.rule, old.context, COALESCE(old.cause, ''));
            INSERT INTO notebook_fts(rowid, topic, title, tags, rule, context, cause)
            VALUES (new.id, new.topic, new.title, new.tags, new.rule, new.context, COALESCE(new.cause, ''));
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
            kind: Kind::Fix,
            topic: None,
            title: title.into(),
            tags: vec!["sqlite".into(), "Rust".into()],
            context: "Readers in the app stalled for seconds while the CLI posted an update.".into(),
            cause: Some("A write transaction held the lock while copying a large screenshot.".into()),
            rule: "Copy media before opening the write transaction, and keep transactions to the insert itself.".into(),
        }
    }

    #[test]
    fn drafts_are_normalized_and_held_to_the_bar() {
        let d = PageDraft {
            title: "  SQLite   reads stall during long writes. ".into(),
            tags: vec!["#Rust, sqlite".into(), "rust".into()],
            ..draft("x")
        };
        let ok = d.checked().expect("a good page passes");
        assert_eq!(ok.title, "SQLite reads stall during long writes");
        assert_eq!(ok.tags, ["rust", "sqlite"]);
        assert_eq!(ok.topic.as_deref(), Some("rust"));

        let explicit = PageDraft { topic: Some("Next.js".into()), ..draft("App Router streaming falls back") }.checked().unwrap();
        assert_eq!(explicit.topic.as_deref(), Some("nextjs"));
        let custom = PageDraft { topic: Some("GraphQL API".into()), ..draft("Cache headers break hydration") }.checked().unwrap();
        assert_eq!(custom.topic.as_deref(), Some("graphql-api"));
        assert_eq!(topic_label("graphql-api"), "Graphql Api");

        let short = PageDraft { rule: "Fix it.".into(), ..draft("SQLite reads stall during long writes") };
        assert!(short.checked().unwrap_err().to_string().contains("`do this` is too short"));
        let untagged = PageDraft { tags: vec![], ..draft("SQLite reads stall during long writes") };
        assert!(untagged.checked().unwrap_err().to_string().contains("tags"));
        let leaky = PageDraft {
            context: "Called the API with sk-abcdefghijklmnopqrstuvwxyz123456 and it failed loudly.".into(),
            ..draft("Webhook calls fail with stale keys")
        };
        assert!(leaky.checked().unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn topic_inference_prefers_specific_frameworks_and_legacy_db_migrates() {
        assert_eq!(infer_topic(&["react".into(), "nextjs".into()], "Server actions", "context"), "nextjs");
        assert_eq!(infer_topic(&["react".into(), "expo".into()], "Scroll jitter", "context"), "react-native");
        assert_eq!(infer_topic(&["hooks".into()], "Effect cleanup", "context"), "react");

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
                INSERT INTO notebook_pages (id, kind, title, tags, context, rule, agent, created_at, updated_at)
                VALUES (1, 'pitfall', 'Reanimated worklets drop closures', 'expo react-native', 'Shared value broke.', 'Mark callbacks with worklet.', 'Codex', 1, 1);",
            )
            .unwrap();
        }
        let s = Store::open(&dir).unwrap();
        let page = s.page(1).unwrap().unwrap();
        assert_eq!(page.topic, "react-native");
        let hits = s.search_pages("react-native", &PageFilter::default(), 5).unwrap();
        assert_eq!(hits.len(), 1);
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
                    rule: "Keep write transactions to the insert; copy files first.".into(),
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
                    context: "A grown scroll column inside an auto-height column drew nothing at all.".into(),
                    cause: None,
                    rule: "Bound an ancestor's height and put min_h(0) on each column between it and the viewport.".into(),
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
    fn kinds_parse_from_what_people_type() {
        assert_eq!("Bug".parse::<Kind>().unwrap(), Kind::Fix);
        assert_eq!("gotcha".parse::<Kind>().unwrap(), Kind::Pitfall);
        assert!("vibe".parse::<Kind>().is_err());
        let md = Page {
            id: 3,
            kind: Kind::Decision,
            topic: "agents".into(),
            title: "Keep the MCP server stdio-only".into(),
            tags: vec!["mcp".into()],
            context: "Considered an HTTP transport.".into(),
            cause: Some("A port would expose the store.".into()),
            rule: "Ship stdio only.".into(),
            project_id: None,
            agent: "You".into(),
            created_at: 0,
            updated_at: 0,
            revision: 1,
            standing: Standing::Current,
            helped: 2,
        }
        .to_markdown(Some("Feeder"), 0);
        assert!(md.starts_with("## Keep the MCP server stdio-only (#3)"));
        assert!(md.contains("Agents & MCP · Decision"));
        assert!(md.contains("**Do this:** Ship stdio only."));
    }
}
