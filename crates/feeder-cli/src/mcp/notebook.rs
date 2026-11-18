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
        let kind: Kind = str_arg(args, "kind")
            .ok_or("`kind` is required: fix, pitfall, pattern or decision")?
            .parse()
            .map_err(|e: feeder_core::Error| e.to_string())?;
        let tags: Vec<String> = match args.get("tags") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => vec![],
        };
        let draft = PageDraft {
            kind,
            topic: str_arg(args, "topic").map(str::to_string),
            title: str_arg(args, "title").ok_or("`title` is required")?.to_string(),
            tags,
            context: str_arg(args, "what_happened").ok_or("`what_happened` is required")?.to_string(),
            cause: str_arg(args, "why").map(str::to_string),
            rule: str_arg(args, "do_this").ok_or("`do_this` is required")?.to_string(),
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
    let kind = json!({ "type": "string", "enum": ["fix", "pitfall", "pattern", "decision"], "description": "fix: a bug and how it was solved. pitfall: something that failed or broke, and why to avoid it. pattern: an approach worth reusing. decision: a settled choice and its reasoning." });
    let id = json!({ "type": "integer", "description": "Notebook page id." });
    vec![
        json!({
            "name": "search_notebook",
            "title": "Search the notebook",
            "description": "Search Feeder's shared notebook of lessons from all of the user's projects (bug fixes, pitfalls, patterns, decisions). Call it before non-trivial debugging, when an error is unfamiliar, or before choosing an approach for something tricky. Returns titles and rules; open one with read_page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for: the error text, the technology, the symptom." },
                    "kind": kind,
                    "topic": { "type": "string", "description": "Only pages in this high-level topic, e.g. `rust`, `react`, `nextjs`, `design`." },
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
            "description": "Read one notebook page in full: what happened, why, and the rule to follow.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "write_page",
            "title": "Write or revise a notebook page",
            "description": "Record a lesson worth keeping across projects: a non-obvious bug fix, an approach that failed, a pattern that works, or a settled decision. Search first; to improve an existing page, pass its `id`. Near-duplicates are refused. Write it so it helps in another project; never include secrets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Revise this page instead of adding a new one." },
                    "kind": kind,
                    "topic": { "type": "string", "description": "High-level topic (`rust`, `react`, `nextjs`, `react-native`, `swift`, `typescript`, `agents`, `data`, `design`, `tooling`, or a short custom slug). Inferred from tags when omitted." },
                    "title": { "type": "string", "description": "The situation in plain words, as someone would search for it (6–90 chars)." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "1–6 short lowercase tags: language, framework, area (`rust`, `sqlite`, `layout`)." },
                    "what_happened": { "type": "string", "description": "The symptom or situation, with the exact error text if there was one (12–700 chars)." },
                    "why": { "type": "string", "description": "The root cause or the reasoning (up to 500 chars)." },
                    "do_this": { "type": "string", "description": "The rule to follow next time, one or two imperative sentences (12–320 chars)." },
                    "project": { "type": "string", "description": "The project it came from. Defaults to the one the server runs in." },
                    "allow_similar": { "type": "boolean", "description": "File it even though a similar page exists; only for a genuinely different lesson." }
                },
                "required": ["kind", "title", "tags", "what_happened", "do_this"]
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
