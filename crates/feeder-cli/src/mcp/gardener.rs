//! The gardener's MCP tools (`feeder mcp --gardener`): read flags, Feed traces and the notebook,
//! and change the notebook only through [`Store::curate`], which enforces the gardener's rules
//! (cite a source, only add to hand-written pages, never delete). Every server started for the
//! same session shares its scope and edit limit.

use feeder_core::gardener::{Scope, SessionMode};
use feeder_core::{Curation, FlagStatus, Kind, PageDraft, PageFilter, Provenance, Store, ago, now_ms};
use serde_json::{Value, json};

use super::{Server, ToolOutput, str_arg};
use crate::notebook::page_json;

/// The tools this module answers, by name.
pub(super) fn tool_names(mode: SessionMode) -> &'static [&'static str] {
    mode.mcp_tools()
}

/// How many traces `read_traces` returns at most, and how much of each body.
const MAX_TRACES: usize = 40;
const TRACE_CHARS: usize = 1_200;

impl Server {
    pub(super) fn curator_mode(&self, run_id: i64) -> Result<SessionMode, String> {
        self.open_store()?.active_gardener_run(run_id).map(|run| run.mode).map_err(|e| e.to_string())
    }

    pub(super) fn gardener_call(&self, run_id: i64, name: &str, args: &Value) -> Result<ToolOutput, String> {
        let store = self.open_store()?;
        let run = store.active_gardener_run(run_id).map_err(|e| e.to_string())?;
        if !tool_names(run.mode).contains(&name) {
            return Err(format!("tool unavailable in this Curator session: {name}"));
        }
        let scope = run.scope;
        match name {
            "search_notebook" => Self::gardener_search(&store, scope, args),
            "read_page" => Self::gardener_read(&store, scope, args),
            "list_flags" => Self::list_flags(&store, scope),
            "read_traces" => Self::read_traces(&store, scope, args),
            "write_page" => Self::gardener_write(&store, run_id, args),
            "archive_page" => Self::gardener_archive(&store, run_id, args),
            "dismiss_flag" => Self::dismiss_flag(&store, run_id, args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn gardener_search(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let query = str_arg(args, "query").unwrap_or("");
        let filter = PageFilter {
            include_archived: args.get("include_archived").and_then(Value::as_bool).unwrap_or(false),
            ..PageFilter::default()
        };
        let hits = match scope {
            Scope::Notebook => store.search_pages(query, &filter, 15),
            Scope::Project(id) => store.search_project_notes(id, query, &filter, 15),
        }
        .map_err(|e| e.to_string())?;
        let text = if hits.is_empty() {
            format!("Nothing matches “{query}”.")
        } else {
            hits.iter()
                .map(|p| format!("- {}{}", p.summary_line(), if p.human_edited { " [human_edited]" } else { "" }))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let pages: Vec<Value> = hits.iter().map(|p| gardener_page_json(store, p)).collect();
        Ok(ToolOutput::text(text, json!({ "query": query, "pages": pages })))
    }

    fn gardener_read(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let page = store
            .page(id)
            .map_err(|e| e.to_string())?
            .filter(|p| p.project_id.is_none() || scope.includes(p.project_id))
            .ok_or_else(|| format!("no Notebook page {id} in this session"))?;
        let note = if !scope.includes(page.project_id) {
            "\n\n(Shared note. This project session can read it but cannot edit it.)"
        } else if page.human_edited {
            "\n\n(human_edited: only add to the end of this page.)"
        } else {
            ""
        };
        Ok(ToolOutput::text(format!("{}{note}", page.to_markdown(None, now_ms())), gardener_page_json(store, &page)))
    }

    fn list_flags(store: &Store, scope: Scope) -> Result<ToolOutput, String> {
        let flags = store.flags_in_scope(Some(FlagStatus::Open), scope, 50).map_err(|e| e.to_string())?;
        let now = now_ms();
        let text = if flags.is_empty() {
            "No open flags.".to_string()
        } else {
            flags
                .iter()
                .map(|f| {
                    let page = f.page_id.map(|id| format!(" (page #{id})")).unwrap_or_default();
                    let suggestion = f.suggestion.as_deref().map(|s| format!("\n  suggestion: {s}")).unwrap_or_default();
                    let update = f.source_update_id.map(|id| format!(" (Feed update #{id})")).unwrap_or_default();
                    format!("- flag #{} {}{page}{update}: {} — {}, {}{suggestion}", f.id, f.kind, f.note, f.agent, ago(f.created_at, now))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let data: Vec<Value> = flags
            .iter()
            .map(|f| json!({ "id": f.id, "kind": f.kind.as_str(), "page_id": f.page_id, "source_update_id": f.source_update_id, "agent": f.agent, "note": f.note, "suggestion": f.suggestion }))
            .collect();
        Ok(ToolOutput::text(text, json!({ "flags": data })))
    }

    fn read_traces(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let limit = args.get("limit").and_then(Value::as_u64).map_or(20, |n| usize::try_from(n).unwrap_or(MAX_TRACES)).clamp(1, MAX_TRACES);
        let updates = match args.get("update_id").and_then(Value::as_i64) {
            Some(id) => {
                let update = store
                    .update(id)
                    .map_err(|e| e.to_string())?
                    .filter(|u| scope.includes(Some(u.project_id)))
                    .ok_or_else(|| format!("no Feed update {id} in this session"))?;
                vec![update]
            }
            None => match scope {
                Scope::Notebook => store.timeline(limit),
                Scope::Project(id) => store.feed(id, limit, None),
            }
            .map_err(|e| e.to_string())?,
        };
        let ids: Vec<i64> = updates.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let projects = store.projects().map_err(|e| e.to_string())?;
        let now = now_ms();
        let mut text = String::new();
        let mut data = Vec::new();
        for u in &updates {
            let project = projects.iter().find(|p| p.project.id == u.project_id).map_or_else(String::new, |p| p.project.name.clone());
            let rs: Vec<_> = replies.iter().filter(|r| r.update_id == u.id).collect();
            text.push_str(&format!(
                "\n## update #{} · {project} · {} · {}\n{}\n",
                u.id,
                u.agent,
                ago(u.created_at, now),
                clip(&u.body, TRACE_CHARS)
            ));
            for r in &rs {
                let who = if r.from_human { "the person" } else { r.author.as_str() };
                text.push_str(&format!("> reply from {who}: {}\n", clip(&r.body, TRACE_CHARS)));
            }
            let rs_json: Vec<Value> =
                rs.iter().map(|r| json!({ "from_person": r.from_human, "author": r.author, "body": clip(&r.body, TRACE_CHARS) })).collect();
            data.push(
                json!({ "update_id": u.id, "project": project, "agent": u.agent, "body": clip(&u.body, TRACE_CHARS), "replies": rs_json }),
            );
        }
        if text.is_empty() {
            text = "No Feed updates yet.".into();
        }
        Ok(ToolOutput::text(text.trim_start().to_string(), json!({ "traces": data })))
    }

    fn gardener_write(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let draft = PageDraft {
            kind: str_arg(args, "kind").map(str::parse::<Kind>).transpose().map_err(|e| e.to_string())?.unwrap_or(Kind::Note),
            topic: str_arg(args, "topic").map(str::to_string),
            title: str_arg(args, "title").ok_or("`title` is required")?.to_string(),
            tags: string_list(args, "tags"),
            body: str_arg(args, "body").ok_or("`body` is required")?.to_string(),
        };
        let curation = match args.get("id").and_then(Value::as_i64) {
            Some(page_id) => Curation::Revise { page_id, draft },
            None => Curation::Create { draft, project_id: None },
        };
        Self::curate(store, run_id, curation, args)
    }

    fn gardener_archive(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let page_id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        Self::curate(store, run_id, Curation::Archive { page_id }, args)
    }

    fn curate(store: &Store, run_id: i64, curation: Curation, args: &Value) -> Result<ToolOutput, String> {
        let provenance = Provenance {
            reason: str_arg(args, "reason").unwrap_or("").to_string(),
            flags: id_list(args, "flags"),
            updates: id_list(args, "updates"),
        };
        let done = store.curate_gardener_run(run_id, curation, &provenance).map_err(|e| e.to_string())?;
        let text = format!(
            "{} page #{}: {}. Change #{} can be reverted by the person.",
            done.change.action.label(),
            done.page.id,
            done.page.title,
            done.change.id
        );
        Ok(ToolOutput::text(
            text,
            json!({
                "change_id": done.change.id,
                "action": done.change.action.as_str(),
                "page": gardener_page_json(store, &done.page),
                "source_updates": done.change.source_updates,
                "source_flags": done.change.source_flags,
            }),
        ))
    }

    fn dismiss_flag(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let reason = str_arg(args, "reason").ok_or("`reason` is required: say why it isn't a real gap")?.to_string();
        let flag = store.dismiss_gardener_flag(run_id, id, reason).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Dismissed flag #{}.", flag.id), json!({ "id": flag.id, "status": flag.status.as_str() })))
    }
}

fn gardener_page_json(store: &Store, p: &feeder_core::Page) -> Value {
    let mut v = page_json(store, p);
    v["human_edited"] = json!(p.human_edited);
    v
}

fn id_list(args: &Value, key: &str) -> Vec<i64> {
    args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()
}

fn string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

pub(super) fn tools(mode: SessionMode) -> Vec<Value> {
    let id = json!({ "type": "integer", "description": "Notebook page id." });
    let ids = |what: &str| json!({ "type": "array", "items": { "type": "integer" }, "description": what });
    let reason = json!({ "type": "string", "description": "One line on why, for the person reading the change log." });
    let mut tools = vec![
        json!({
            "name": "search_notebook",
            "title": "Search the notebook",
            "description": "Search notebook pages by words in the title, tags and body. Results mark human_edited pages.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "include_archived": { "type": "boolean" }
            }, "required": ["query"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "read_page",
            "title": "Read a notebook page",
            "description": "Read one page in full.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "list_flags",
            "title": "List open flags",
            "description": "What agents reported missing, wrong or outdated, with any suggested fix.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "read_traces",
            "title": "Read recent Feed updates",
            "description": "Agent updates in the session scope with the person's replies. Pass update_id to read the thread linked to a flag.",
            "inputSchema": { "type": "object", "properties": {
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_TRACES },
                "update_id": { "type": "integer" }
            } },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "write_page",
            "title": "Write or revise a page",
            "description": "Add a page, or revise one with `id` (send its full new title and body). Must cite at least one flag or update. On human_edited pages, keep the title and existing body and only append.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "title": { "type": "string" },
                "body": { "type": "string", "description": "Markdown. Link useful current notes from this project or the shared Notebook as [title](feeder://notebook/ID)." },
                "topic": { "type": "string", "description": "Short slug such as rust, design, tooling or general. Inferred when omitted." },
                "tags": { "type": "array", "items": { "type": "string" } },
                "kind": { "type": "string", "enum": ["note", "fix", "pitfall", "pattern", "decision"] },
                "reason": reason,
                "flags": ids("Open flag ids this fixes; they are resolved with it."),
                "updates": ids("Feed update ids this draws on.")
            }, "required": ["title", "body", "reason"] }
        }),
        json!({
            "name": "archive_page",
            "title": "Archive a page",
            "description": "Take an outdated page out of search (kept for the record). Not allowed on human_edited pages. Must cite a flag or update.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "reason": reason,
                "flags": ids("Open flag ids this fixes."),
                "updates": ids("Feed update ids this draws on.")
            }, "required": ["id", "reason"] }
        }),
        json!({
            "name": "dismiss_flag",
            "title": "Dismiss a flag",
            "description": "Close a flag that isn't a real gap, with the reason.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "integer", "description": "Flag id." },
                "reason": { "type": "string" }
            }, "required": ["id", "reason"] }
        }),
    ];
    tools.retain(|tool| tool["name"].as_str().is_some_and(|name| tool_names(mode).contains(&name)));
    tools
}
