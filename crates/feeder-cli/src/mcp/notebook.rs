//! The notebook's MCP tools: search before digging, read a page, write or revise one, and say
//! when one helped.

use feeder_core::{Kind, OnSimilar, PageDraft, PageFilter, Standing, now_ms};
use serde_json::{Value, json};

use super::{Server, ToolOutput, str_arg};
use crate::notebook::{page_json, project_name};

/// The tools this module answers, by name.
pub(super) const TOOLS: [&str; 5] = ["search_notebook", "read_page", "write_page", "mark_page_helped", "archive_page"];

impl Server {
    pub(super) fn notebook_call(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        match name {
            "search_notebook" => self.search_notebook(args),
            "read_page" => self.read_page(args),
            "write_page" => self.write_page(args),
            "mark_page_helped" => self.mark_page_helped(args),
            "archive_page" => self.archive_page(args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn search_notebook(&self, args: &Value) -> Result<ToolOutput, String> {
        let query = str_arg(args, "query").unwrap_or("");
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, 25) as usize;
        let kind = str_arg(args, "kind").map(str::parse::<Kind>).transpose().map_err(|e| e.to_string())?;
        let filter = PageFilter {
            kind,
            topic: str_arg(args, "topic").map(str::to_string),
            tag: str_arg(args, "tag").map(str::to_lowercase),
            project_id: None,
            include_archived: args.get("include_archived").and_then(Value::as_bool).unwrap_or(false),
        };
        let store = self.open_store()?;
        let hits = store.search_pages(query, &filter, limit).map_err(|e| e.to_string())?;
        let text = if hits.is_empty() {
            format!("Nothing in the notebook matches “{query}”. If you solve this and the cause wasn't obvious, consider writing a page.")
        } else {
            let mut out = format!("{} page(s), best match first. Open one with read_page:", hits.len());
            for p in &hits {
                out.push_str(&format!("\n- {}", p.summary_line()));
            }
            out
        };
        let pages: Vec<Value> = hits.iter().map(|p| page_json(&store, p)).collect();
        Ok(ToolOutput::text(text, json!({ "query": query, "pages": pages })))
    }

    fn read_page(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        let page = store.page(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no notebook page {id}"))?;
        let text = page.to_markdown(project_name(&store, &page).as_deref(), now_ms());
        Ok(ToolOutput::text(text, page_json(&store, &page)))
    }

    fn write_page(&self, args: &Value) -> Result<ToolOutput, String> {
        let kind: Kind = match str_arg(args, "kind") {
            Some(raw) => raw.parse().map_err(|e: feeder_core::Error| e.to_string())?,
            None => Kind::Note,
        };
        let tags: Vec<String> = match args.get("tags") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => vec![],
        };
        let title = str_arg(args, "title").ok_or("`title` is required")?.to_string();
        let topic = str_arg(args, "topic").map(str::to_string);
        let draft = if let Some(body) = str_arg(args, "body") {
            PageDraft { kind, topic, title, tags, body: body.to_string() }
        } else if args.get("what_happened").is_some() || args.get("do_this").is_some() {
            PageDraft::from_legacy(
                kind,
                topic,
                title,
                tags,
                str_arg(args, "what_happened").unwrap_or_default().to_string(),
                str_arg(args, "why").map(str::to_string),
                str_arg(args, "do_this").unwrap_or_default().to_string(),
            )
        } else {
            return Err("`body` (Markdown) is required".into());
        };
        let author = self.resolve_agent(args);
        let store = self.open_store()?;
        let page = match args.get("id").and_then(Value::as_i64) {
            Some(id) => store.revise_page(id, draft, &author),
            None => {
                let project_id = self.project(&store, args, false)?.map(|p| p.id);
                let on_similar =
                    if args.get("allow_similar").and_then(Value::as_bool).unwrap_or(false) { OnSimilar::Allow } else { OnSimilar::Refuse };
                store.create_page(draft, &author, project_id, on_similar)
            }
        }
        .map_err(|e| e.to_string())?;
        let verb = if page.revision > 1 { "Revised" } else { "Wrote" };
        Ok(ToolOutput::text(format!("{verb} notebook page #{}: {}", page.id, page.title), page_json(&store, &page)))
    }

    fn mark_page_helped(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        let page = store.mark_page_helped(id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Noted: page #{} has helped {}×.", page.id, page.helped), page_json(&store, &page)))
    }

    fn archive_page(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let restore = args.get("restore").and_then(Value::as_bool).unwrap_or(false);
        let standing = if restore { Standing::Current } else { Standing::Archived };
        let store = self.open_store()?;
        store.set_page_standing(id, standing).map_err(|e| e.to_string())?;
        let verb = if restore { "Restored" } else { "Archived" };
        Ok(ToolOutput::text(format!("{verb} notebook page #{id}."), json!({ "id": id, "archived": !restore })))
    }
}

pub(super) fn tools() -> Vec<Value> {
    let kind = json!({ "type": "string", "enum": ["note", "fix", "pitfall", "pattern", "decision"], "description": "Optional kind (`note` default for freeform articles, or `fix`, `pitfall`, `pattern`, `decision`)." });
    let id = json!({ "type": "integer", "description": "Notebook page id." });
    vec![
        json!({
            "name": "search_notebook",
            "title": "Search the notebook",
            "description": "Search Feeder's shared Markdown knowledge library across all projects (notes, fixes, pitfalls, patterns, decisions). Call it before non-trivial debugging, when an error is unfamiliar, or before choosing an approach for something tricky. Returns titles and excerpts; open one in full with read_page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for: the error text, the technology, the symptom." },
                    "kind": kind,
                    "topic": { "type": "string", "description": "Only pages in this category/topic, e.g. `rust`, `react`, `nextjs`, `design`." },
                    "tag": { "type": "string", "description": "Only pages with this tag, e.g. `sqlite`." },
                    "include_archived": { "type": "boolean", "description": "Include archived (outdated) pages." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 25, "description": "How many pages (default 8)." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "read_page",
            "title": "Read a notebook page",
            "description": "Read one notebook article in full Markdown.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "write_page",
            "title": "Write or revise a notebook page",
            "description": "Save a freeform Markdown article to Feeder's cross-project knowledge library (a non-obvious fix, pitfall, reusable pattern, architecture decision, or technical guide). Search first; to update an existing article, pass its `id`. Near-duplicates are refused. Never include secrets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Revise this page instead of adding a new one." },
                    "title": { "type": "string", "description": "Clear, searchable title (2–120 chars)." },
                    "body": { "type": "string", "description": "Freeform Markdown content (headings, fenced code blocks, lists, `- [ ]` checklists, tables, `> [!note]` callouts, links)." },
                    "topic": { "type": "string", "description": "Optional category (`rust`, `react`, `nextjs`, `react-native`, `swift`, `typescript`, `agents`, `data`, `design`, `tooling`, `general`, or a custom slug). Inferred automatically when omitted." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional lowercase tags (0–10)." },
                    "kind": kind,
                    "project": { "type": "string", "description": "The project it came from. Defaults to the one the server runs in." },
                    "allow_similar": { "type": "boolean", "description": "File it even though a similar page exists; only for a genuinely different note." }
                },
                "required": ["title", "body"]
            }
        }),
        json!({
            "name": "mark_page_helped",
            "title": "Mark a notebook page as helpful",
            "description": "Record that a notebook page helped with the current task, so pages that keep paying off rise in search.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] }
        }),
        json!({
            "name": "archive_page",
            "title": "Archive or restore a notebook page",
            "description": "Archive a page that's outdated or wrong (it stays on record but leaves search), or restore one.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": id, "restore": { "type": "boolean", "description": "Bring an archived page back." } },
                "required": ["id"]
            }
        }),
    ]
}
