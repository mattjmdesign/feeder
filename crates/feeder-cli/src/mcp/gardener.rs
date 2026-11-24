use feeder_core::gardener::{Scope, SessionMode};
use feeder_core::{Curation, FlagStatus, Kind, PageDraft, PageFilter, Provenance, Store, ago, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};

use super::protocol::{ann_local_write, ann_read_only, decode_args, non_empty};
use super::{Server, ToolOutput};
use crate::notebook::{page_json_with, page_summary_json_with, project_name};

pub(super) fn tool_names(mode: SessionMode) -> &'static [&'static str] {
    mode.mcp_tools()
}

const MAX_TRACES: usize = 40;
const TRACE_CHARS: usize = 1_200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchNotebookArgs {
    query: Option<String>,
    kind: Option<String>,
    topic: Option<String>,
    tag: Option<String>,
    include_archived: Option<bool>,
    limit: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadPageArgs {
    id: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFlagsArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadTracesArgs {
    limit: Option<u64>,
    update_id: Option<i64>,
    project: Option<String>,
    query: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WritePageArgs {
    id: Option<i64>,
    title: String,
    body: String,
    topic: Option<String>,
    tags: Option<Vec<String>>,
    kind: Option<String>,
    shared: Option<bool>,
    reason: Option<String>,
    flags: Option<Vec<i64>>,
    updates: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivePageArgs {
    id: i64,
    reason: Option<String>,
    flags: Option<Vec<i64>>,
    updates: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DismissFlagArgs {
    id: i64,
    reason: String,
}

impl Server {
    pub(super) fn curator_mode(&self, run_id: i64) -> Result<(SessionMode, bool), String> {
        let store = self.open_store()?;
        let run = store.active_gardener_run(run_id).map_err(|e| e.to_string())?;
        let is_chat = store.is_curator_chat_run(run_id).map_err(|e| e.to_string())?;
        Ok((run.mode, is_chat))
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
            "list_flags" => Self::list_flags(&store, scope, args),
            "read_traces" => Self::read_traces(&store, scope, args),
            "write_page" => Self::gardener_write(&store, run_id, args),
            "archive_page" => Self::gardener_archive(&store, run_id, args),
            "dismiss_flag" => Self::dismiss_flag(&store, run_id, args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn gardener_search(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let parsed: SearchNotebookArgs = decode_args("search_notebook", args)?;
        let query = non_empty(parsed.query).unwrap_or_default();
        let kind = non_empty(parsed.kind).map(|k| k.parse::<Kind>()).transpose().map_err(|e| e.to_string())?;
        let limit = parsed.limit.unwrap_or(15).clamp(1, 25) as usize;
        let filter = PageFilter {
            kind,
            topic: non_empty(parsed.topic),
            tag: non_empty(parsed.tag).map(|t| t.to_lowercase()),
            project_id: None,
            include_archived: parsed.include_archived.unwrap_or(false),
        };
        let hits = match scope {
            Scope::Notebook => store.search_pages(&query, &filter, limit),
            Scope::Project(id) => store.search_project_notes(id, &query, &filter, limit),
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
        let projects = store.project_names().map_err(|e| e.to_string())?;
        let pages: Vec<Value> =
            hits.iter().map(|p| gardener_page_summary_json(p, p.project_id.and_then(|id| projects.get(&id).map(String::as_str)))).collect();
        Ok(ToolOutput::text(text, json!({ "query": query, "pages": pages })))
    }

    fn gardener_read(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ReadPageArgs = decode_args("read_page", args)?;
        let page = store
            .page(parsed.id)
            .map_err(|e| e.to_string())?
            .filter(|p| p.project_id.is_none() || scope.includes(p.project_id))
            .ok_or_else(|| format!("no Library note {} in this session", parsed.id))?;
        let note = if !scope.includes(page.project_id) {
            "\n\n(Shared note. This project session can read it but cannot edit it.)"
        } else if page.human_edited {
            "\n\n(human_edited: only add to the end of this page.)"
        } else {
            ""
        };
        let project = project_name(store, &page);
        Ok(ToolOutput::text(
            format!("{}{note}", page.to_markdown(project.as_deref(), now_ms())),
            gardener_page_json(&page, project.as_deref()),
        ))
    }

    fn list_flags(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let _: ListFlagsArgs = decode_args("list_flags", args)?;
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
            .map(|f| {
                json!({
                    "id": f.id,
                    "kind": f.kind.as_str(),
                    "page_id": f.page_id,
                    "source_update_id": f.source_update_id,
                    "agent": f.agent,
                    "note": f.note,
                    "suggestion": f.suggestion,
                })
            })
            .collect();
        Ok(ToolOutput::text(text, json!({ "flags": data })))
    }

    fn read_traces(store: &Store, scope: Scope, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ReadTracesArgs = decode_args("read_traces", args)?;
        let limit = parsed.limit.map_or(20, |n| usize::try_from(n).unwrap_or(MAX_TRACES)).clamp(1, MAX_TRACES);
        let project_filter = match non_empty(parsed.project) {
            Some(name) => Some(store.find_project(&name).map_err(|e| e.to_string())?.ok_or_else(|| format!("no project called {name}"))?),
            None => None,
        };
        if let (Scope::Project(scoped_id), Some(proj)) = (scope, &project_filter)
            && proj.id != scoped_id
        {
            return Err(format!("project `{}` is outside this Curator session scope", proj.name));
        }
        let target_pid = project_filter.as_ref().map(|p| p.id).or_else(|| scope.project_id());
        let query = non_empty(parsed.query);
        let updates = match parsed.update_id {
            Some(id) => {
                let update = store
                    .update(id)
                    .map_err(|e| e.to_string())?
                    .filter(|u| scope.includes(Some(u.project_id)) && target_pid.is_none_or(|pid| u.project_id == pid))
                    .ok_or_else(|| format!("no Feed update {id} in this session"))?;
                vec![update]
            }
            None => match (&query, target_pid) {
                (Some(q), pid) => store.search_updates(q, pid, pid.is_some(), limit).map_err(|e| e.to_string())?,
                (None, Some(pid)) => store.feed(pid, limit, None).map_err(|e| e.to_string())?,
                (None, None) => store.timeline(limit).map_err(|e| e.to_string())?,
            },
        };
        let ids: Vec<i64> = updates.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let projects = store.projects().map_err(|e| e.to_string())?;
        let now = now_ms();
        let mut text = String::new();
        let mut data = Vec::new();
        for u in &updates {
            let project_sum = projects.iter().find(|p| p.project.id == u.project_id);
            let project = project_sum.map_or_else(String::new, |p| p.project.name.clone());
            let brief = project_sum.and_then(|p| p.project.brief.as_deref()).map(str::trim).filter(|b| !b.is_empty());
            let rs: Vec<_> = replies.iter().filter(|r| r.update_id == u.id).collect();
            text.push_str(&format!(
                "\n## update #{} · {project} · {} · {}\n{}\n",
                u.id,
                u.agent,
                ago(u.created_at, now),
                clip(&u.body, TRACE_CHARS)
            ));
            if let Some(brief) = brief {
                text.push_str(&format!("_Project brief: {}_\n", clip(brief, 400)));
            }
            for r in &rs {
                let who = if r.from_human { "the person" } else { r.author.as_str() };
                text.push_str(&format!("> reply from {who}: {}\n", clip(&r.body, TRACE_CHARS)));
            }
            let rs_json: Vec<Value> =
                rs.iter().map(|r| json!({ "from_person": r.from_human, "author": r.author, "body": clip(&r.body, TRACE_CHARS) })).collect();
            data.push(json!({
                "update_id": u.id,
                "project": project,
                "project_brief": brief,
                "agent": u.agent,
                "body": clip(&u.body, TRACE_CHARS),
                "replies": rs_json,
            }));
        }
        if text.is_empty() {
            text = "No Feed updates yet.".into();
        }
        Ok(ToolOutput::text(text.trim_start().to_string(), json!({ "traces": data })))
    }

    fn gardener_write(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let parsed: WritePageArgs = decode_args("write_page", args)?;
        let kind = non_empty(parsed.kind).map(|k| k.parse::<Kind>()).transpose().map_err(|e| e.to_string())?.unwrap_or(Kind::Note);
        let tags = parsed.tags.unwrap_or_default().into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        let draft = PageDraft {
            kind,
            topic: non_empty(parsed.topic),
            title: parsed.title.trim().to_string(),
            tags,
            body: parsed.body.trim().to_string(),
        };
        let shared = parsed.shared.unwrap_or(false);
        let curation = match (parsed.id, shared) {
            (Some(_), true) => return Err("`shared` only applies when creating a new note without `id`".into()),
            (Some(page_id), false) => Curation::Revise { page_id, draft },
            (None, true) => Curation::CreateShared { draft },
            (None, false) => Curation::Create { draft, project_id: None },
        };
        let provenance = Provenance {
            reason: non_empty(parsed.reason).unwrap_or_default(),
            flags: parsed.flags.unwrap_or_default(),
            updates: parsed.updates.unwrap_or_default(),
        };
        Self::curate(store, run_id, curation, &provenance)
    }

    fn gardener_archive(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ArchivePageArgs = decode_args("archive_page", args)?;
        let provenance = Provenance {
            reason: non_empty(parsed.reason).unwrap_or_default(),
            flags: parsed.flags.unwrap_or_default(),
            updates: parsed.updates.unwrap_or_default(),
        };
        Self::curate(store, run_id, Curation::Archive { page_id: parsed.id }, &provenance)
    }

    fn curate(store: &Store, run_id: i64, curation: Curation, provenance: &Provenance) -> Result<ToolOutput, String> {
        if store.is_curator_chat_run(run_id).map_err(|e| e.to_string())? {
            let proposal = store.propose_gardener_run(run_id, curation, provenance).map_err(|e| e.to_string())?;
            let action_phrase = match proposal.action {
                feeder_core::ChangeAction::Created => "creating",
                feeder_core::ChangeAction::Revised => "revising",
                feeder_core::ChangeAction::Archived => "archiving",
                feeder_core::ChangeAction::Restored => "restoring",
            };
            let text = format!(
                "Proposed {action_phrase} “{}” (proposal #{}). The person can apply or dismiss this in chat.",
                proposal.title, proposal.id
            );
            return Ok(ToolOutput::text(
                text,
                json!({
                    "proposal_id": proposal.id,
                    "status": proposal.status.as_str(),
                    "action": proposal.action.as_str(),
                    "title": proposal.title,
                    "page_id": proposal.page_id,
                    "source_updates": proposal.source_updates,
                    "source_flags": proposal.source_flags,
                }),
            ));
        }
        let done = store.curate_gardener_run(run_id, curation, provenance).map_err(|e| e.to_string())?;
        let text = format!(
            "{} page #{}: {}. Change #{} can be reverted by the person.",
            done.change.action.label(),
            done.page.id,
            done.page.title,
            done.change.id
        );
        let page_json = gardener_page_json(&done.page, project_name(store, &done.page).as_deref());
        Ok(ToolOutput::text(
            text,
            json!({
                "change_id": done.change.id,
                "action": done.change.action.as_str(),
                "page": page_json,
                "source_updates": done.change.source_updates,
                "source_flags": done.change.source_flags,
            }),
        ))
    }

    fn dismiss_flag(store: &Store, run_id: i64, args: &Value) -> Result<ToolOutput, String> {
        let parsed: DismissFlagArgs = decode_args("dismiss_flag", args)?;
        let reason = parsed.reason.trim().to_string();
        if reason.is_empty() {
            return Err("`reason` is required: say why it isn't a real gap".into());
        }
        let flag = store.dismiss_gardener_flag(run_id, parsed.id, reason).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Dismissed flag #{}.", flag.id), json!({ "id": flag.id, "status": flag.status.as_str() })))
    }
}

fn gardener_page_json(p: &feeder_core::Page, project: Option<&str>) -> Value {
    let mut v = page_json_with(p, project);
    v["human_edited"] = json!(p.human_edited);
    v
}

fn gardener_page_summary_json(p: &feeder_core::Page, project: Option<&str>) -> Value {
    let mut v = page_summary_json_with(p, project);
    v["human_edited"] = json!(p.human_edited);
    v
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

pub(super) fn tools(mode: SessionMode, is_chat: bool) -> Vec<Value> {
    let id = json!({ "type": "integer", "description": "Library page id." });
    let ids = |what: &str| json!({ "type": "array", "items": { "type": "integer" }, "description": what });
    let reason = json!({ "type": "string", "description": "One line on why, for the person reading the change log." });
    let write_desc = if is_chat {
        "Propose adding a page or revising one with `id` (send its full new title and body). Must cite at least one flag or update. On human_edited pages, keep the title and existing body and only append. Creates a proposal card for the person to apply or dismiss."
    } else {
        "Add a page, or revise one with `id` (send its full new title and body). Must cite at least one flag or update. On human_edited pages, keep the title and existing body and only append."
    };
    let archive_desc = if is_chat {
        "Propose archiving an outdated page out of search (kept for the record). Not allowed on human_edited pages. Must cite a flag or update. Creates a proposal card for the person to apply or dismiss."
    } else {
        "Take an outdated page out of search (kept for the record). Not allowed on human_edited pages. Must cite a flag or update."
    };
    let mut tools = vec![
        json!({
            "name": "search_notebook",
            "title": "Search the Library",
            "description": "Search Library pages by words, kind, topic, or tag. Returns titles, excerpts, and human_edited markers; open full pages with read_page.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": { "type": "string", "description": "Words to match in title, tags, or body." },
                    "kind": { "type": "string", "enum": ["note", "fix", "pitfall", "pattern", "decision"] },
                    "topic": { "type": "string" },
                    "tag": { "type": "string" },
                    "include_archived": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 25 }
                }
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "read_page",
            "title": "Read a Library page",
            "description": "Read one Library page in full.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": id },
                "required": ["id"]
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "list_flags",
            "title": "List open flags",
            "description": "What agents reported missing, wrong or outdated, with any suggested fix.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "read_traces",
            "title": "Read recent Feed updates",
            "description": "Agent updates in the session scope with the person's replies. Filter with update_id, project (in global sessions), or query.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_TRACES },
                    "update_id": { "type": "integer" },
                    "project": { "type": "string" },
                    "query": { "type": "string" }
                }
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "write_page",
            "title": "Write or revise a page",
            "description": write_desc,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": id,
                    "title": { "type": "string" },
                    "body": { "type": "string", "description": "Markdown. Link useful current notes as [title](feeder://library/ID)." },
                    "topic": { "type": "string", "description": "Short slug such as rust, design, tooling or general. Inferred when omitted." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "kind": { "type": "string", "enum": ["note", "fix", "pitfall", "pattern", "decision"] },
                    "shared": { "type": "boolean", "description": "Set true in a Library-wide session to file a shared cross-project note instead of inferring a project from sources." },
                    "reason": reason,
                    "flags": ids("Open flag ids this fixes; they are resolved with it."),
                    "updates": ids("Feed update ids this draws on.")
                },
                "required": ["title", "body", "reason"]
            },
            "annotations": ann_local_write()
        }),
        json!({
            "name": "archive_page",
            "title": "Archive a page",
            "description": archive_desc,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": id,
                    "reason": reason,
                    "flags": ids("Open flag ids this fixes."),
                    "updates": ids("Feed update ids this draws on.")
                },
                "required": ["id", "reason"]
            },
            "annotations": ann_local_write()
        }),
        json!({
            "name": "dismiss_flag",
            "title": "Dismiss a flag",
            "description": "Close a flag that isn't a real gap, with the reason.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "integer", "description": "Flag id." },
                    "reason": { "type": "string" }
                },
                "required": ["id", "reason"]
            },
            "annotations": ann_local_write()
        }),
    ];
    tools.retain(|tool| tool["name"].as_str().is_some_and(|name| tool_names(mode).contains(&name)));
    tools
}
