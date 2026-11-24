use feeder_core::{FlagKind, Kind, NewFlag, PageFilter, now_ms};
use serde_json::{Value, json};

use super::{Server, ToolOutput, str_arg};
use crate::notebook::{page_json, page_json_with, pages_json, project_name};

pub(super) const TOOLS: [&str; 4] = ["search_notebook", "read_page", "mark_page_helped", "flag_knowledge"];

impl Server {
    pub(super) fn notebook_call(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        match name {
            "search_notebook" => self.search_notebook(args),
            "read_page" => self.read_page(args),
            "mark_page_helped" => self.mark_page_helped(args),
            "flag_knowledge" => self.flag_knowledge(args),
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
            format!(
                "Nothing in the Library matches “{query}”. If you work out a reusable answer, use flag_knowledge with a lesson draft in suggestion for Curator."
            )
        } else {
            let mut out = format!("{} note(s), best match first. Open one with read_page:", hits.len());
            for p in &hits {
                out.push_str(&format!("\n- {}", p.summary_line()));
            }
            out
        };
        let pages = pages_json(&store, &hits)?;
        Ok(ToolOutput::text(text, json!({ "query": query, "pages": pages })))
    }

    fn read_page(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        let page = store.page(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no Library note {id}"))?;
        let project = project_name(&store, &page);
        let text = page.to_markdown(project.as_deref(), now_ms());
        Ok(ToolOutput::text(text, page_json_with(&page, project.as_deref())))
    }

    fn mark_page_helped(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        let page = store.mark_page_helped(id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Noted: page #{} has helped {}×.", page.id, page.helped), page_json(&store, &page)))
    }

    fn flag_knowledge(&self, args: &Value) -> Result<ToolOutput, String> {
        let kind: FlagKind = str_arg(args, "kind").unwrap_or("missing").parse().map_err(|e: feeder_core::Error| e.to_string())?;
        let note = str_arg(args, "note").ok_or("`note` is required: say what's missing or wrong")?.to_string();
        let shared = args.get("shared").and_then(Value::as_bool).unwrap_or(false);
        if shared && (str_arg(args, "project").is_some() || str_arg(args, "project_path").is_some()) {
            return Err("pass either `project`/`project_path` or `shared: true`, not both".into());
        }
        let store = self.open_store()?;
        let project_id = if shared { None } else { self.project(&store, args, false)?.map(|p| p.id) };
        let flag = NewFlag {
            kind,
            page_id: args.get("page_id").and_then(Value::as_i64),
            project_id,
            note,
            suggestion: str_arg(args, "suggestion").map(str::to_string),
        };
        let flag = store.flag_knowledge(flag, &self.resolve_agent(args)).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(
            format!("Flagged #{} ({}) for the next Curator review.", flag.id, flag.kind),
            json!({ "id": flag.id, "kind": flag.kind.as_str(), "page_id": flag.page_id, "project_id": flag.project_id, "status": "open" }),
        ))
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
            "name": "mark_page_helped",
            "title": "Mark a notebook page as helpful",
            "description": "Record that a notebook page helped with the current task, so pages that keep paying off rise in search.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] }
        }),
        json!({
            "name": "flag_knowledge",
            "title": "Flag missing or wrong library knowledge",
            "description": "Search the Library first. Flag missing, incorrect, or outdated knowledge (or a user correction / non-obvious bug fix) instead of writing or archiving shared notes directly. Add the matching `page_id` when there is one and put a full Markdown lesson draft (mistake/symptom, root cause, verified fix or rule) in `suggestion`. Curator reviews the flag. Never include secrets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["missing", "wrong", "outdated"], "description": "What you found (default `missing`)." },
                    "note": { "type": "string", "description": "What's missing or wrong, in a sentence or two (4–2000 chars)." },
                    "page_id": { "type": "integer", "description": "The page that's wrong or outdated, if any." },
                    "suggestion": { "type": "string", "description": "Full Markdown lesson draft (up to 10,000 chars): symptom or mistake, root cause, code snippet or rule for Curator to review." },
                    "project": { "type": "string", "description": "The project you were working in. Defaults to the one the server runs in." },
                    "project_path": { "type": "string", "description": "Folder path of the project when flagging outside its repo." },
                    "shared": { "type": "boolean", "description": "Set true for a cross-project Library gap instead of tying the flag to the current project." }
                },
                "required": ["note"]
            }
        }),
    ]
}
