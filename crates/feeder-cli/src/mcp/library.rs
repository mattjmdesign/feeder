use std::collections::HashSet;

use feeder_core::{FLAG_NOTE_CHARS, FLAG_SUGGESTION_MAX_CHARS, FlagKind, Kind, NewFlag, PageFilter, Store, now_ms, secret_in};
use serde::Deserialize;
use serde_json::{Value, json};

use super::protocol::{ann_local_write, ann_read_only, decode_args, non_empty, project_path_schema, project_schema};
use super::{Server, ToolOutput};
use crate::notebook::{page_json_with, page_summaries_json, project_name};

pub(super) const TOOLS: [&str; 6] =
    ["search_library", "read_library", "flag_knowledge", "search_notebook", "read_page", "mark_page_helped"];

const MAX_READ_NOTES: usize = 5;
const MAX_BATCH_IDS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchLibraryArgs {
    query: Option<String>,
    kind: Option<String>,
    topic: Option<String>,
    tag: Option<String>,
    project: Option<String>,
    project_path: Option<String>,
    include_archived: Option<bool>,
    limit: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadLibraryArgs {
    id: Option<i64>,
    ids: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlagKnowledgeArgs {
    kind: Option<String>,
    note: String,
    page_id: Option<i64>,
    update_id: Option<i64>,
    suggestion: Option<String>,
    project: Option<String>,
    project_path: Option<String>,
    shared: Option<bool>,
    helped_notes: Option<Vec<i64>>,
    agent: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkPageHelpedArgs {
    id: i64,
}

pub(super) fn dedup_ids(ids: impl IntoIterator<Item = i64>, max: usize, label: &str) -> Result<Vec<i64>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if seen.insert(id) {
            out.push(id);
            if out.len() > max {
                return Err(format!("pass at most {max} {label} per call"));
            }
        }
    }
    Ok(out)
}

pub(super) fn validate_existing_pages(store: &Store, ids: &[i64]) -> Result<Vec<i64>, String> {
    let ids = dedup_ids(ids.iter().copied(), MAX_BATCH_IDS, "helped_notes")?;
    for &id in &ids {
        store.page(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no Library note {id}"))?;
    }
    Ok(ids)
}

pub(super) fn validate_flag_fields(
    store: &Store,
    kind_raw: Option<&str>,
    note_raw: &str,
    page_id: Option<i64>,
    suggestion_raw: Option<&str>,
) -> Result<(FlagKind, String, Option<String>), String> {
    let kind: FlagKind = kind_raw.unwrap_or("missing").parse().map_err(|e: feeder_core::Error| e.to_string())?;
    let note = note_raw.trim().to_string();
    let count = note.chars().count();
    if !FLAG_NOTE_CHARS.contains(&count) {
        return Err(format!("`note` must be {}-{} characters (got {count})", FLAG_NOTE_CHARS.start(), FLAG_NOTE_CHARS.end()));
    }
    if let Some(what) = secret_in(&note) {
        return Err(format!("`note` looks like it contains {what}; leave secrets out"));
    }
    let suggestion = suggestion_raw.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(s) = &suggestion {
        if s.chars().count() > FLAG_SUGGESTION_MAX_CHARS {
            return Err(format!("`suggestion` must be under {FLAG_SUGGESTION_MAX_CHARS} characters"));
        }
        if let Some(what) = secret_in(s) {
            return Err(format!("`suggestion` looks like it contains {what}; leave secrets out"));
        }
    }
    if let Some(id) = page_id {
        store.page(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no Library note {id}"))?;
    }
    Ok((kind, note, suggestion))
}

impl Server {
    pub(super) fn library_call(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        match name {
            "search_library" | "search_notebook" => self.search_library(name, args),
            "read_library" | "read_page" => self.read_library(name, args),
            "flag_knowledge" => self.flag_knowledge(args),
            "mark_page_helped" => self.mark_page_helped(args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn search_library(&self, tool_name: &str, args: &Value) -> Result<ToolOutput, String> {
        let parsed: SearchLibraryArgs = decode_args(tool_name, args)?;
        let query = non_empty(parsed.query).unwrap_or_default();
        let limit = parsed.limit.unwrap_or(8).clamp(1, 25) as usize;
        let kind = non_empty(parsed.kind).map(|k| k.parse::<Kind>()).transpose().map_err(|e| e.to_string())?;
        let topic = non_empty(parsed.topic);
        let tag = non_empty(parsed.tag).map(|t| t.to_lowercase());
        let project = non_empty(parsed.project);
        let project_path = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let scoped_project = if project.is_some() || project_path.is_some() {
            self.resolve_project(&store, project.as_deref(), project_path.as_deref(), false)?
        } else {
            None
        };
        let filter = PageFilter { kind, topic, tag, project_id: None, include_archived: parsed.include_archived.unwrap_or(false) };
        let hits = match &scoped_project {
            Some(p) => store.search_project_notes(p.id, &query, &filter, limit),
            None => store.search_pages(&query, &filter, limit),
        }
        .map_err(|e| e.to_string())?;
        let text = if hits.is_empty() {
            if query.is_empty() {
                "No Library notes match those filters. Use flag_knowledge with a lesson draft in suggestion for Curator.".to_string()
            } else {
                format!(
                    "Nothing in the Library matches “{query}”. If you work out a reusable answer, use flag_knowledge with a lesson draft in suggestion for Curator."
                )
            }
        } else {
            let mut out = format!("{} note(s), best match first. Open one with read_library:", hits.len());
            for p in &hits {
                out.push_str(&format!("\n- {}", p.summary_line()));
            }
            out
        };
        let pages = page_summaries_json(&store, &hits)?;
        Ok(ToolOutput::text(
            text,
            json!({
                "query": query,
                "project": scoped_project.as_ref().map(|p| p.name.as_str()),
                "pages": pages,
            }),
        ))
    }

    fn read_library(&self, tool_name: &str, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ReadLibraryArgs = decode_args(tool_name, args)?;
        let used_ids_array = parsed.ids.is_some();
        let combined = parsed.id.into_iter().chain(parsed.ids.unwrap_or_default());
        let ids = dedup_ids(combined, MAX_READ_NOTES, "Library note IDs")?;
        if ids.is_empty() {
            return Err("pass `id` or non-empty `ids` (up to 5 Library note IDs)".into());
        }
        let store = self.open_store()?;
        let now = now_ms();
        let mut rendered = Vec::with_capacity(ids.len());
        let mut data_pages = Vec::with_capacity(ids.len());
        for id in ids {
            let page = store.page(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no Library note {id}"))?;
            let project = project_name(&store, &page);
            rendered.push(page.to_markdown(project.as_deref(), now));
            data_pages.push(page_json_with(&page, project.as_deref()));
        }
        if !used_ids_array && data_pages.len() == 1 {
            let text = rendered.into_iter().next().unwrap_or_default();
            let data = data_pages.into_iter().next().unwrap_or_default();
            return Ok(ToolOutput::text(text, data));
        }
        Ok(ToolOutput::text(rendered.join("\n\n---\n\n"), json!({ "pages": data_pages })))
    }

    fn mark_page_helped(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: MarkPageHelpedArgs = decode_args("mark_page_helped", args)?;
        let store = self.open_store()?;
        let page = store.mark_page_helped(parsed.id).map_err(|e| e.to_string())?;
        let project = project_name(&store, &page);
        Ok(ToolOutput::text(format!("Noted: page #{} has helped {}×.", page.id, page.helped), page_json_with(&page, project.as_deref())))
    }

    fn flag_knowledge(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: FlagKnowledgeArgs = decode_args("flag_knowledge", args)?;
        let shared = parsed.shared.unwrap_or(false);
        let project = non_empty(parsed.project);
        let project_path = non_empty(parsed.project_path);
        if shared && (project.is_some() || project_path.is_some()) {
            return Err("pass either `project`/`project_path` or `shared: true`, not both".into());
        }
        let store = self.open_store()?;
        let (kind, note, suggestion) =
            validate_flag_fields(&store, non_empty(parsed.kind).as_deref(), &parsed.note, parsed.page_id, parsed.suggestion.as_deref())?;
        let helped_ids = validate_existing_pages(&store, &parsed.helped_notes.unwrap_or_default())?;
        let update_project_id = match parsed.update_id {
            Some(uid) => Some(store.update(uid).map_err(|e| e.to_string())?.ok_or_else(|| format!("no Feed update {uid}"))?.project_id),
            None => None,
        };
        let project_id = if shared {
            None
        } else if project.is_some() || project_path.is_some() {
            self.resolve_project(&store, project.as_deref(), project_path.as_deref(), false)?.map(|p| p.id)
        } else if let Some(pid) = update_project_id {
            Some(pid)
        } else {
            self.resolve_project(&store, None, None, false)?.map(|p| p.id)
        };
        let agent = self.resolve_agent(non_empty(parsed.agent).as_deref());
        let flag = NewFlag { kind, page_id: parsed.page_id, project_id, note, suggestion };
        let flag = store.flag_knowledge_with_source(flag, &agent, parsed.update_id).map_err(|e| e.to_string())?;
        for &pid in &helped_ids {
            store.mark_page_helped(pid).map_err(|e| e.to_string())?;
        }
        let mut text = format!("Flagged #{} ({}) for the next Curator review.", flag.id, flag.kind);
        if !helped_ids.is_empty() {
            text.push_str(&format!(" Marked {} note(s) helpful.", helped_ids.len()));
        }
        Ok(ToolOutput::text(
            text,
            json!({
                "id": flag.id,
                "kind": flag.kind.as_str(),
                "page_id": flag.page_id,
                "project_id": flag.project_id,
                "source_update_id": flag.source_update_id,
                "helped_notes": helped_ids,
                "status": "open"
            }),
        ))
    }
}

pub(super) fn tools() -> Vec<Value> {
    let kind = json!({
        "type": "string",
        "enum": ["note", "fix", "pitfall", "pattern", "decision"]
    });
    let id = json!({ "type": "integer" });
    vec![
        json!({
            "name": "search_library",
            "title": "Search the Library",
            "description": "Search Feeder's shared Markdown Library across projects (or filter to one project plus shared notes). Omit query to browse. Returns titles and excerpts only; open full notes with read_library.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": { "type": "string" },
                    "kind": kind,
                    "topic": { "type": "string" },
                    "tag": { "type": "string" },
                    "project": project_schema(),
                    "project_path": project_path_schema(),
                    "include_archived": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 25, "description": "Default 8." }
                }
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "read_library",
            "title": "Read Library note(s)",
            "description": "Read one (`id`) or up to 5 (`ids`) Library notes in full Markdown.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": id,
                    "ids": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_READ_NOTES }
                }
            },
            "annotations": ann_read_only()
        }),
        json!({
            "name": "flag_knowledge",
            "title": "Flag Library gap or lesson",
            "description": "Search the Library first. Flag missing, wrong, or outdated knowledge (or a user correction or non-obvious fix) for Curator instead of writing notes directly. Put a full Markdown draft in `suggestion`.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "enum": ["missing", "wrong", "outdated"] },
                    "note": { "type": "string", "description": "What is missing or wrong (4-2000 chars)." },
                    "page_id": { "type": "integer" },
                    "update_id": { "type": "integer" },
                    "suggestion": { "type": "string", "description": "Full Markdown lesson draft (<= 10,000 chars)." },
                    "project": project_schema(),
                    "project_path": project_path_schema(),
                    "shared": { "type": "boolean" },
                    "helped_notes": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_BATCH_IDS },
                    "agent": { "type": "string" }
                },
                "required": ["note"]
            },
            "annotations": ann_local_write()
        }),
    ]
}
