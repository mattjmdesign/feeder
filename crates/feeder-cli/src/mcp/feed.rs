use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use feeder_core::{
    FeedUpdate, MAX_MEDIA_PER_UPDATE, MAX_PULSE_CHARS, MAX_TODO_CHARS, NewFlag, NewUpdate, PageFilter, Reply, Store, ago, detect_git_meta,
    detect_project, now_ms, parse_attention, secret_in, validate_body,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::library::{dedup_ids, validate_existing_pages, validate_flag_fields};
use super::protocol::{
    ann_destructive, ann_local_idempotent, ann_local_write, ann_read_only, decode_args, non_empty, project_path_schema, project_schema,
};
use super::{ImageBlock, Server, ToolOutput};
use crate::{project_json, todo_json, update_json};

const MAX_INLINE_SCREENSHOTS: usize = 6;
const MAX_INLINE_SCREENSHOT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INLINE_SCREENSHOT_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_BRIEF_CHARS: usize = 600;
const MAX_BATCH_ITEMS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListProjectsArgs {
    filter: Option<String>,
    include_archived: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetFeedArgs {
    project: Option<String>,
    project_path: Option<String>,
    task: Option<String>,
    limit: Option<u64>,
    include_screenshots: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchUpdatesArgs {
    query: Option<String>,
    project: Option<String>,
    project_path: Option<String>,
    include_archived: Option<bool>,
    limit: Option<u64>,
    include_screenshots: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetRecentActivityArgs {
    limit: Option<u64>,
    include_screenshots: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineFlagLesson {
    kind: Option<String>,
    note: String,
    page_id: Option<i64>,
    suggestion: Option<String>,
    shared: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostUpdateArgs {
    summary: String,
    project: Option<String>,
    project_path: Option<String>,
    agent: Option<String>,
    branch: Option<String>,
    diff_stat: Option<String>,
    media: Option<Vec<String>>,
    screenshot_path: Option<String>,
    screenshots: Option<Vec<String>>,
    mark_replies_read: Option<bool>,
    helped_notes: Option<Vec<i64>>,
    complete_todos: Option<Vec<i64>>,
    add_todos: Option<Vec<String>>,
    flag_lesson: Option<InlineFlagLesson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditUpdateArgs {
    id: i64,
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyToUpdateArgs {
    id: i64,
    summary: String,
    agent: Option<String>,
    mark_replies_read: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PulseInput {
    status: Option<String>,
    branch: Option<String>,
    ttl_seconds: Option<u64>,
    clear: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProjectArgs {
    project: Option<String>,
    project_path: Option<String>,
    agent: Option<String>,
    pulse: Option<PulseInput>,
    brief: Option<String>,
    clear_brief: Option<bool>,
    dismiss_attention: Option<bool>,
    dismiss_update_id: Option<i64>,
    mark_replies_read: Option<bool>,
    mark_reply_ids: Option<Vec<i64>>,
    add_todos: Option<Vec<String>>,
    complete_todos: Option<Vec<i64>>,
    reopen_todos: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeleteKind {
    Update,
    Todo,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteItemArgs {
    kind: DeleteKind,
    id: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPendingArgs {
    project: Option<String>,
    project_path: Option<String>,
    mark_read: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDismissArgs {
    id: Option<i64>,
    project: Option<String>,
    project_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMarkRepliesReadArgs {
    ids: Option<Vec<i64>>,
    project: Option<String>,
    project_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyBriefArgs {
    brief: Option<String>,
    clear: Option<bool>,
    project: Option<String>,
    project_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPulseArgs {
    status: Option<String>,
    project: Option<String>,
    project_path: Option<String>,
    agent: Option<String>,
    branch: Option<String>,
    ttl_seconds: Option<u64>,
    clear: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyListTodosArgs {
    project: Option<String>,
    project_path: Option<String>,
    open_only: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAddTodoArgs {
    title: String,
    project: Option<String>,
    project_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyToggleTodoArgs {
    id: i64,
    done: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyIdArgs {
    id: i64,
}

fn validate_todo_titles(titles: Vec<String>) -> Result<Vec<String>, String> {
    if titles.len() > MAX_BATCH_ITEMS {
        return Err(format!("add at most {MAX_BATCH_ITEMS} to-dos per call"));
    }
    let mut cleaned = Vec::with_capacity(titles.len());
    for raw in titles {
        let title: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return Err("to-do cannot be empty".into());
        }
        let chars = title.chars().count();
        if chars > MAX_TODO_CHARS {
            return Err(format!("to-do is {chars} characters; keep it to {MAX_TODO_CHARS} or fewer"));
        }
        if let Some(what) = secret_in(&title) {
            return Err(format!("to-do looks like {what} — remove the secret before saving"));
        }
        cleaned.push(title);
    }
    Ok(cleaned)
}

fn validate_pulse_status(raw: Option<String>) -> Result<String, String> {
    let status = non_empty(raw).unwrap_or_else(|| "Working".into());
    let chars = status.chars().count();
    if chars > MAX_PULSE_CHARS {
        return Err(format!("pulse status is {chars} characters; keep it to {MAX_PULSE_CHARS} or fewer"));
    }
    if let Some(what) = secret_in(&status) {
        return Err(format!("pulse status looks like {what} — remove the secret before saving"));
    }
    Ok(status)
}

fn validate_existing_todos(store: &Store, ids: &[i64], project_id: Option<i64>, label: &str) -> Result<Vec<i64>, String> {
    let ids = dedup_ids(ids.iter().copied(), MAX_BATCH_ITEMS, label)?;
    for &id in &ids {
        let todo = store.todo(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no to-do {id}"))?;
        if let Some(pid) = project_id
            && todo.project_id != pid
        {
            return Err(format!("to-do #{id} belongs to another project"));
        }
    }
    Ok(ids)
}

impl Server {
    pub(super) fn feed_call(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        match name {
            "list_projects" => self.list_projects(args),
            "get_feed" => self.get_feed(args),
            "search_updates" => self.search_updates(args),
            "post_update" => self.post_update(args),
            "reply_to_update" => self.reply_to_update(args),
            "edit_update" => self.edit_update(args),
            "update_project" => self.update_project(args),
            "delete_item" => self.delete_item(args),
            "get_recent_activity" => self.get_recent_activity(args),
            "get_pending_decisions" => self.legacy_get_pending_decisions(args),
            "dismiss_attention" => self.legacy_dismiss_attention(args),
            "mark_replies_read" => self.legacy_mark_replies_read(args),
            "set_project_brief" => self.legacy_set_project_brief(args),
            "pulse" => self.legacy_pulse(args),
            "list_todos" => self.legacy_list_todos(args),
            "add_todo" => self.legacy_add_todo(args),
            "toggle_todo" => self.legacy_toggle_todo(args),
            "delete_todo" => self.legacy_delete_todo(args),
            "delete_update" => self.legacy_delete_update(args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn list_projects(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ListProjectsArgs = decode_args("list_projects", args)?;
        let filter = match non_empty(parsed.filter).as_deref() {
            Some("active") => "active",
            Some("pending") => "pending",
            Some("archived") => "archived",
            Some("all") => "all",
            Some(other) => return Err(format!("invalid filter `{other}`: expected one of active, pending, archived, all")),
            None if parsed.include_archived == Some(true) => "all",
            None => "active",
        };
        let store = self.open_store()?;
        let summaries = store.projects().map_err(|e| e.to_string())?;
        let names: HashMap<i64, String> = summaries.iter().map(|s| (s.project.id, s.project.name.clone())).collect();
        let projects: Vec<_> = summaries
            .into_iter()
            .filter(|p| match filter {
                "active" => !p.project.archived,
                "pending" => !p.project.archived && (p.needs_you || p.unread_replies > 0),
                "archived" => p.project.archived,
                _ => true,
            })
            .collect();
        let now = now_ms();
        let mut text = if projects.is_empty() {
            match filter {
                "pending" => "No pending decisions or unread human replies.".to_string(),
                "archived" => "No archived projects.".to_string(),
                _ => "No projects yet.".to_string(),
            }
        } else {
            String::new()
        };
        let mut list = Vec::new();
        for p in &projects {
            let last = match (p.last_at, &p.last_agent) {
                (Some(at), Some(agent)) => format!("last update {} by {agent}", ago(at, now)),
                _ => "no updates".into(),
            };
            if !text.is_empty() {
                text.push('\n');
            }
            let pin = if p.project.pinned { " [pinned]" } else { "" };
            let needs = if p.needs_you { " [needs you]" } else { "" };
            let pulse = p.active_pulse.as_ref().map(|pu| format!(" [working: {}]", pu.status)).unwrap_or_default();
            let unread_tag = if p.unread_replies > 0 {
                format!(", {} unread repl{}", p.unread_replies, if p.unread_replies == 1 { "y" } else { "ies" })
            } else {
                String::new()
            };
            let todos = if p.open_todos > 0 { format!(", {} open to-do(s)", p.open_todos) } else { String::new() };
            let tag = if p.project.archived { " [archived]" } else { "" };
            text.push_str(&format!("{}{pin}{needs}{pulse}{tag} — {} update(s){unread_tag}{todos}, {last}", p.project.name, p.updates));
            let mut v = project_json(&p.project);
            v["updates"] = json!(p.updates);
            v["last_at"] = json!(p.last_at);
            v["last_agent"] = json!(p.last_agent);
            v["needs_you"] = json!(p.needs_you);
            v["unread_replies"] = json!(p.unread_replies);
            v["open_todos"] = json!(p.open_todos);
            v["active_pulse"] = p
                .active_pulse
                .as_ref()
                .map(|pu| {
                    json!({ "agent": pu.agent, "branch": pu.branch, "status": pu.status, "updated_at": pu.updated_at, "expires_at": pu.expires_at })
                })
                .unwrap_or(Value::Null);
            list.push(v);
        }
        if filter == "pending" {
            let unread = store.unread_human_replies(None).map_err(|e| e.to_string())?;
            if !unread.is_empty() {
                text.push_str(&format!("\n\nUnread human replies ({}):", unread.len()));
                for r in &unread {
                    let pname = names.get(&r.project_id).map(String::as_str).unwrap_or("Unknown");
                    text.push_str(&format!(
                        "\n- reply #{} ({pname}, update #{}, {}): {}",
                        r.id,
                        r.update_id,
                        ago(r.created_at, now),
                        r.body
                    ));
                }
            }
            let flagged: Vec<_> = projects.iter().filter(|p| p.needs_you).collect();
            if !flagged.is_empty() {
                text.push_str(&format!("\n\nOpen Decision/Blocked cards ({}):", flagged.len()));
                for s in &flagged {
                    let uid = s.last_update_id.unwrap_or(0);
                    let when = s.last_at.map(|t| ago(t, now)).unwrap_or_else(|| "recently".into());
                    let body = s.last_body.as_deref().unwrap_or("");
                    text.push_str(&format!("\n- {} #{uid} ({when}): {body}", s.project.name));
                }
            }
            let unread_json: Vec<_> = unread
                .iter()
                .map(|r| {
                    let mut v = reply_json(r);
                    v["project"] = json!(names.get(&r.project_id).cloned().unwrap_or_default());
                    v
                })
                .collect();
            let flagged_json: Vec<_> = flagged
                .iter()
                .filter_map(|s| {
                    Some(json!({
                        "id": s.last_update_id?,
                        "project_id": s.project.id,
                        "project": s.project.name,
                        "agent": s.last_agent,
                        "body": s.last_body.as_deref()?,
                        "created_at": s.last_at?,
                    }))
                })
                .collect();
            return Ok(ToolOutput::text(
                text,
                json!({
                    "filter": filter,
                    "projects": list,
                    "unread_replies": unread_json,
                    "needs_you_updates": flagged_json,
                }),
            ));
        }
        Ok(ToolOutput::text(text, json!({ "filter": filter, "projects": list })))
    }

    fn get_feed(&self, args: &Value) -> Result<ToolOutput, String> {
        if args.get("mark_replies_read").is_some() {
            return Err("`get_feed` is strictly read-only; pass `mark_replies_read: true` to `post_update`, `reply_to_update`, or `update_project` instead.".into());
        }
        let parsed: GetFeedArgs = decode_args("get_feed", args)?;
        let limit = parsed.limit.unwrap_or(5).clamp(1, 50) as usize;
        let include_shots = parsed.include_screenshots.unwrap_or(false);
        let task = non_empty(parsed.task);
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let Some(project) = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)? else {
            let name = match &project_name_arg {
                Some(n) => n.clone(),
                None => detect_project(&self.base(project_path_arg.as_deref())).name,
            };
            let mut data = json!({ "project": null, "todos": [], "updates": [], "unread_replies": [] });
            if task.is_some() {
                data["related_notes"] = json!([]);
            }
            return Ok(ToolOutput::text(format!("{name} has no updates yet."), data));
        };
        let feed = store.feed(project.id, limit, None).map_err(|e| e.to_string())?;
        let ids: Vec<i64> = feed.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let todos = store.todos(project.id).map_err(|e| e.to_string())?;
        let unread = store.unread_human_replies(Some(project.id)).map_err(|e| e.to_string())?;
        let now = now_ms();
        let needs_you = feed.first().is_some_and(|u| {
            u.attention_dismissed_at.is_none()
                && parse_attention(&u.body).is_some()
                && !replies.iter().any(|r| r.update_id == u.id && r.from_human)
        });
        let mut text = if feed.is_empty() {
            format!("{} has no updates yet.", project.name)
        } else {
            format!("Latest {} update(s) for {}, newest first:", feed.len(), project.name)
        };
        if needs_you && let Some(latest) = feed.first() {
            text.push_str(&format!("\n\nStatus: Needs you (open attention on update #{}).", latest.id));
        }
        if let Some(brief) = &project.brief {
            text.push_str(&format!("\n\nPinned brief:\n{brief}"));
        }
        let mut related_notes = Vec::new();
        if let Some(task) = task.as_ref().filter(|task| task.chars().take(160).any(char::is_alphanumeric)) {
            let query: String = task.chars().take(160).collect();
            let notes = store.search_project_notes(project.id, &query, &PageFilter::default(), 3).map_err(|e| e.to_string())?;
            if !notes.is_empty() {
                text.push_str("\n\nLibrary notes for this task (open with read_library):");
            }
            for page in notes {
                let url = format!("feeder://library/{}", page.id);
                let scope = if page.project_id.is_some() { "project" } else { "shared" };
                let excerpt = page.excerpt(160);
                text.push_str(&format!("\n- #{} {} ({scope}) — {excerpt}", page.id, page.title));
                related_notes.push(json!({ "id": page.id, "title": page.title, "excerpt": excerpt, "url": url, "scope": scope }));
            }
        }
        if !todos.is_empty() {
            let open_count = todos.iter().filter(|t| !t.done).count();
            text.push_str(&format!("\n\nTo-dos ({open_count} open / {} total):", todos.len()));
            for t in &todos {
                let mark = if t.done { "x" } else { " " };
                text.push_str(&format!("\n- [{mark}] {} (#{})", t.title, t.id));
            }
        }
        if !unread.is_empty() {
            text.push_str(&format!("\n\nUnread human replies ({}):", unread.len()));
            for r in &unread {
                text.push_str(&format!("\n- reply #{} on update #{} ({}): {}", r.id, r.update_id, ago(r.created_at, now), r.body));
            }
        }
        for u in &feed {
            text.push_str(&format!("\n\n{}", format_update_line(None, u, &replies, now)));
        }
        let images = if include_shots { collect_screenshots(store.dir(), &feed) } else { Vec::new() };
        let updates: Vec<_> = feed
            .iter()
            .map(|u| {
                let mut v = update_json(u);
                v["replies"] = json!(replies.iter().filter(|r| r.update_id == u.id).map(reply_json).collect::<Vec<_>>());
                v
            })
            .collect();
        let todos_json: Vec<_> = todos.iter().map(todo_json).collect();
        let unread_json: Vec<_> = unread.iter().map(reply_json).collect();
        let mut data = json!({
            "project": project_json(&project),
            "needs_you": needs_you,
            "todos": todos_json,
            "updates": updates,
            "unread_replies": unread_json,
        });
        if task.is_some() {
            data["related_notes"] = json!(related_notes);
        }
        Ok(ToolOutput { text, data, images })
    }

    fn search_updates(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: SearchUpdatesArgs = decode_args("search_updates", args)?;
        let query = non_empty(parsed.query);
        let include_archived = parsed.include_archived.unwrap_or(false);
        let limit = parsed.limit.unwrap_or(10).clamp(1, 50) as usize;
        let include_shots = parsed.include_screenshots.unwrap_or(false);
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let pid = if project_name_arg.is_some() || project_path_arg.is_some() {
            let resolved = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)?;
            match (project_name_arg, resolved) {
                (Some(name), None) => return Err(format!("no project called {name}{}", self.known_projects_hint(&store))),
                (_, Some(p)) => Some(p.id),
                (None, None) => return Ok(ToolOutput::text("No updates yet for this folder.".into(), json!({ "updates": [] }))),
            }
        } else {
            None
        };
        let hits = match (&query, pid) {
            (Some(q), _) => store.search_updates(q, pid, include_archived || pid.is_some(), limit).map_err(|e| e.to_string())?,
            (None, Some(id)) => store.feed(id, limit, None).map_err(|e| e.to_string())?,
            (None, None) => store.timeline(limit).map_err(|e| e.to_string())?,
        };
        let ids: Vec<i64> = hits.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let projects = store.project_names().map_err(|e| e.to_string())?;
        let now = now_ms();
        let mut text = match (&query, hits.is_empty()) {
            (Some(q), true) => format!("No updates matching \"{q}\"."),
            (Some(q), false) => format!("{} match(es) for \"{q}\":", hits.len()),
            (None, true) => "No recent updates across active projects.".to_string(),
            (None, false) => format!("Latest {} update(s):", hits.len()),
        };
        let mut list = Vec::new();
        for u in &hits {
            let pname = projects.get(&u.project_id).map(String::as_str).unwrap_or("Unknown");
            text.push_str(&format!("\n\n{}", format_update_line(Some(pname), u, &replies, now)));
            let mut v = update_json(u);
            v["project"] = json!(pname);
            v["replies"] = json!(replies.iter().filter(|r| r.update_id == u.id).map(reply_json).collect::<Vec<_>>());
            list.push(v);
        }
        let images = if include_shots { collect_screenshots(store.dir(), &hits) } else { Vec::new() };
        Ok(ToolOutput { text, data: json!({ "updates": list }), images })
    }

    fn get_recent_activity(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: GetRecentActivityArgs = decode_args("get_recent_activity", args)?;
        self.search_updates(&json!({
            "limit": parsed.limit.unwrap_or(10),
            "include_screenshots": parsed.include_screenshots.unwrap_or(false),
        }))
    }

    fn post_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: PostUpdateArgs = decode_args("post_update", args)?;
        validate_body(&parsed.summary).map_err(|e| e.to_string())?;
        if let Some(kind) = secret_in(&parsed.summary) {
            return Err(format!("update summary looks like {kind} — remove the secret before saving"));
        }
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let base = self.base(project_path_arg.as_deref());
        let resolve_path = |raw: &str| {
            let p = PathBuf::from(raw);
            if p.is_absolute() { p } else { base.join(p) }
        };
        let mut seen = HashSet::new();
        let mut media_paths = Vec::new();
        let raw_paths =
            parsed.media.unwrap_or_default().into_iter().chain(parsed.screenshot_path).chain(parsed.screenshots.unwrap_or_default());
        for raw in raw_paths {
            let t = raw.trim();
            if t.is_empty() {
                continue;
            }
            let path = resolve_path(t);
            if seen.insert(path.clone()) {
                media_paths.push(path);
            }
        }
        if media_paths.len() > MAX_MEDIA_PER_UPDATE {
            return Err(format!("an update can attach at most {MAX_MEDIA_PER_UPDATE} media files (got {})", media_paths.len()));
        }

        let store = self.open_store()?;
        let helped_ids = validate_existing_pages(&store, &parsed.helped_notes.unwrap_or_default())?;
        let add_todos = validate_todo_titles(parsed.add_todos.unwrap_or_default())?;
        let complete_todos = dedup_ids(parsed.complete_todos.unwrap_or_default(), MAX_BATCH_ITEMS, "complete_todos")?;
        let flag_checked = match &parsed.flag_lesson {
            Some(lesson) => {
                if lesson.shared == Some(true) && (project_name_arg.is_some() || project_path_arg.is_some()) {
                    return Err("pass either `project`/`project_path` or `flag_lesson.shared: true`, not both".into());
                }
                let (kind, note, suggestion) = validate_flag_fields(
                    &store,
                    non_empty(lesson.kind.clone()).as_deref(),
                    &lesson.note,
                    lesson.page_id,
                    lesson.suggestion.as_deref(),
                )?;
                Some((kind, note, lesson.page_id, suggestion, lesson.shared.unwrap_or(false)))
            }
            None => None,
        };

        let existing_project = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)?;
        let expected_pid = existing_project.as_ref().map(|p| p.id);
        if !complete_todos.is_empty() && expected_pid.is_none() {
            return Err("no matching project found for `complete_todos`; create or name the project first".into());
        }
        validate_existing_todos(&store, &complete_todos, expected_pid, "complete_todos")?;
        let project = match existing_project {
            Some(p) => p,
            None => self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), true)?.ok_or("no project")?,
        };

        let agent = self.resolve_agent(non_empty(parsed.agent).as_deref());
        let git_dir = if project_path_arg.is_some() || project_name_arg.is_none() {
            Some(base)
        } else {
            project.path.as_deref().map(PathBuf::from).filter(|p| p.exists())
        };
        let meta = git_dir.as_deref().map(detect_git_meta).unwrap_or_default();
        let branch = non_empty(parsed.branch).or(meta.branch);
        let diff_stat = non_empty(parsed.diff_stat).or(meta.diff_stat);
        let commit_sha = meta.commit_sha;

        let posted = store
            .post(
                project.id,
                NewUpdate { agent, body: parsed.summary, screenshot: None, screenshots: media_paths, branch, diff_stat, commit_sha },
            )
            .map_err(|e| e.to_string())?;

        if parsed.mark_replies_read.unwrap_or(false) {
            store.mark_project_replies_read(project.id).map_err(|e| e.to_string())?;
        }
        for &tid in &complete_todos {
            store.set_todo_done(tid, true).map_err(|e| e.to_string())?;
        }
        let mut added_todos = Vec::with_capacity(add_todos.len());
        for title in add_todos {
            added_todos.push(store.add_todo(project.id, &title).map_err(|e| e.to_string())?);
        }
        for &pid in &helped_ids {
            store.mark_page_helped(pid).map_err(|e| e.to_string())?;
        }
        let flagged_id = if let Some((kind, note, page_id, suggestion, shared)) = flag_checked {
            let flag = store
                .flag_knowledge_with_source(
                    NewFlag { kind, page_id, project_id: if shared { None } else { Some(project.id) }, note, suggestion },
                    &posted.agent,
                    Some(posted.id),
                )
                .map_err(|e| e.to_string())?;
            Some(flag.id)
        } else {
            None
        };

        let unread = store.unread_human_replies(Some(project.id)).map_err(|e| e.to_string())?;
        let media_note = match posted.media.len() {
            0 => String::new(),
            1 => " (1 attachment)".to_string(),
            n => format!(" ({n} attachments)"),
        };
        let mut text = format!("Posted update #{} to {}'s feed as {}{media_note}.", posted.id, project.name, posted.agent);
        if !complete_todos.is_empty() {
            text.push_str(&format!(" Completed {} to-do(s).", complete_todos.len()));
        }
        if !added_todos.is_empty() {
            text.push_str(&format!(" Added {} to-do(s).", added_todos.len()));
        }
        if !helped_ids.is_empty() {
            text.push_str(&format!(" Marked {} note(s) helpful.", helped_ids.len()));
        }
        if let Some(fid) = flagged_id {
            text.push_str(&format!(" Flagged lesson #{fid} for Curator."));
        }
        if !unread.is_empty() {
            let now = now_ms();
            text.push_str(&format!(
                "\n\nNote: {} has {} unread human repl{}:",
                project.name,
                unread.len(),
                if unread.len() == 1 { "y" } else { "ies" }
            ));
            for r in &unread {
                text.push_str(&format!("\n- reply #{} on update #{} ({}): {}", r.id, r.update_id, ago(r.created_at, now), r.body));
            }
        }
        let unread_json: Vec<_> = unread.iter().map(reply_json).collect();
        let added_json: Vec<_> = added_todos.iter().map(todo_json).collect();
        Ok(ToolOutput::text(
            text,
            json!({
                "project": project_json(&project),
                "update": update_json(&posted),
                "unread_replies": unread_json,
                "completed_todos": complete_todos,
                "added_todos": added_json,
                "helped_notes": helped_ids,
                "flagged_lesson_id": flagged_id,
            }),
        ))
    }

    fn edit_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: EditUpdateArgs = decode_args("edit_update", args)?;
        let store = self.open_store()?;
        let updated = store.edit_update(parsed.id, &parsed.summary).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Updated #{}.", parsed.id), json!({ "update": update_json(&updated) })))
    }

    fn reply_to_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: ReplyToUpdateArgs = decode_args("reply_to_update", args)?;
        let agent = self.resolve_agent(non_empty(parsed.agent).as_deref());
        let store = self.open_store()?;
        let reply = store.add_reply(parsed.id, &agent, false, &parsed.summary).map_err(|e| e.to_string())?;
        let marked_read = if parsed.mark_replies_read.unwrap_or(true) {
            store.mark_update_replies_read(parsed.id).map_err(|e| e.to_string())?
        } else {
            0
        };
        let text = if marked_read > 0 {
            format!(
                "Replied (#{}) to update #{} as {} (marked {marked_read} human repl{} read).",
                reply.id,
                parsed.id,
                reply.author,
                if marked_read == 1 { "y" } else { "ies" }
            )
        } else {
            format!("Replied (#{}) to update #{} as {}.", reply.id, parsed.id, reply.author)
        };
        Ok(ToolOutput::text(text, json!({ "reply": reply_json(&reply), "marked_replies_read": marked_read })))
    }

    fn update_project(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: UpdateProjectArgs = decode_args("update_project", args)?;
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let agent = self.resolve_agent(non_empty(parsed.agent).as_deref());
        let add_todos = validate_todo_titles(parsed.add_todos.unwrap_or_default())?;
        let complete_todos = dedup_ids(parsed.complete_todos.unwrap_or_default(), MAX_BATCH_ITEMS, "complete_todos")?;
        let reopen_todos = dedup_ids(parsed.reopen_todos.unwrap_or_default(), MAX_BATCH_ITEMS, "reopen_todos")?;
        let mark_reply_ids = dedup_ids(parsed.mark_reply_ids.unwrap_or_default(), 100, "mark_reply_ids")?;
        if complete_todos.iter().any(|id| reopen_todos.contains(id)) {
            return Err("do not pass the same to-do ID in both `complete_todos` and `reopen_todos`".into());
        }
        let clear_brief = parsed.clear_brief.unwrap_or(false);
        if clear_brief && parsed.brief.is_some() {
            return Err("pass either `brief` or `clear_brief: true`, not both".into());
        }
        let validated_brief = match &parsed.brief {
            Some(raw) => {
                let text = raw.trim().to_string();
                if text.is_empty() {
                    return Err("pass non-empty `brief` text or set `clear_brief: true`".into());
                }
                let chars = text.chars().count();
                if chars > MAX_BRIEF_CHARS {
                    return Err(format!("brief is {chars} characters; keep it to {MAX_BRIEF_CHARS} or fewer"));
                }
                if let Some(what) = secret_in(&text) {
                    return Err(format!("brief looks like {what} — remove the secret before saving"));
                }
                Some(text)
            }
            None => None,
        };
        if parsed.dismiss_attention == Some(true) && parsed.dismiss_update_id.is_some() {
            return Err("pass either `dismiss_attention: true` or `dismiss_update_id`, not both".into());
        }
        let pulse_plan = match &parsed.pulse {
            Some(pulse) if pulse.clear.unwrap_or(false) => Some(None),
            Some(pulse) => {
                let status = validate_pulse_status(pulse.status.clone())?;
                Some(Some((status, non_empty(pulse.branch.clone()), pulse.ttl_seconds.unwrap_or(120).clamp(5, 3600))))
            }
            None => None,
        };

        let dismiss_project = parsed.dismiss_attention.unwrap_or(false);
        let mark_project_read = parsed.mark_replies_read.unwrap_or(false);
        let has_any_action = pulse_plan.is_some()
            || validated_brief.is_some()
            || clear_brief
            || dismiss_project
            || parsed.dismiss_update_id.is_some()
            || mark_project_read
            || !mark_reply_ids.is_empty()
            || !add_todos.is_empty()
            || !complete_todos.is_empty()
            || !reopen_todos.is_empty();
        if !has_any_action {
            return Err("pass at least one update field: `pulse`, `brief`, `clear_brief`, `dismiss_attention`, `dismiss_update_id`, `mark_replies_read`, `mark_reply_ids`, `add_todos`, `complete_todos`, or `reopen_todos`".into());
        }

        let store = self.open_store()?;
        if let Some(uid) = parsed.dismiss_update_id {
            store.update(uid).map_err(|e| e.to_string())?.ok_or_else(|| format!("no update {uid}"))?;
        }
        let needs_project_create = matches!(pulse_plan, Some(Some(_))) || validated_brief.is_some() || !add_todos.is_empty();
        let needs_project_existing = matches!(pulse_plan, Some(None)) || clear_brief || dismiss_project || mark_project_read;
        let explicit_project_scope = project_name_arg.is_some() || project_path_arg.is_some();
        let resolved_existing = if needs_project_create || needs_project_existing || explicit_project_scope {
            self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)?
        } else {
            None
        };
        if !needs_project_create && (needs_project_existing || explicit_project_scope) && resolved_existing.is_none() {
            return Err(format!("no matching project found{}", self.known_projects_hint(&store)));
        }
        let scope_pid = resolved_existing.as_ref().map(|p| p.id);
        validate_existing_todos(&store, &complete_todos, scope_pid, "complete_todos")?;
        validate_existing_todos(&store, &reopen_todos, scope_pid, "reopen_todos")?;
        let project_opt = if needs_project_create {
            Some(match resolved_existing {
                Some(p) => p,
                None => {
                    self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), true)?.ok_or("no project")?
                }
            })
        } else {
            resolved_existing
        };

        let mut summary_parts = Vec::new();
        let mut pulse_json = Value::Null;
        if let (Some(plan), Some(proj)) = (&pulse_plan, &project_opt) {
            match plan {
                None => {
                    store.clear_pulse(proj.id, &agent).map_err(|e| e.to_string())?;
                    summary_parts.push(format!("Cleared live pulse for {} ({agent}).", proj.name));
                    pulse_json = json!({ "cleared": true, "agent": agent });
                }
                Some((status, branch_override, ttl_secs)) => {
                    let git_dir = if project_path_arg.is_some() || project_name_arg.is_none() {
                        Some(self.base(project_path_arg.as_deref()))
                    } else {
                        proj.path.as_deref().map(PathBuf::from).filter(|p| p.exists())
                    };
                    let meta = git_dir.as_deref().map(detect_git_meta).unwrap_or_default();
                    let branch = branch_override.clone().or(meta.branch);
                    let ttl_ms = (*ttl_secs * 1000) as i64;
                    let pulse = store.set_pulse(proj.id, &agent, branch.as_deref(), status, ttl_ms).map_err(|e| e.to_string())?;
                    summary_parts.push(format!("Pulse active on {} ({}): {}.", proj.name, pulse.agent, pulse.status));
                    pulse_json = json!({
                        "agent": pulse.agent,
                        "branch": pulse.branch,
                        "status": pulse.status,
                        "updated_at": pulse.updated_at,
                        "expires_at": pulse.expires_at,
                    });
                }
            }
        }
        if let Some(proj) = &project_opt {
            if let Some(brief) = &validated_brief {
                store.set_project_brief(proj.id, Some(brief)).map_err(|e| e.to_string())?;
                summary_parts.push(format!("Updated pinned brief for {}.", proj.name));
            } else if clear_brief {
                store.set_project_brief(proj.id, None).map_err(|e| e.to_string())?;
                summary_parts.push(format!("Cleared pinned brief for {}.", proj.name));
            }
            if dismiss_project {
                let dismissed = store.dismiss_project_attention(proj.id).map_err(|e| e.to_string())?;
                summary_parts.push(if dismissed {
                    format!("Dismissed Needs you for {}.", proj.name)
                } else {
                    format!("{} had no updates to dismiss.", proj.name)
                });
            }
            if mark_project_read {
                store.mark_project_replies_read(proj.id).map_err(|e| e.to_string())?;
                summary_parts.push(format!("Marked unread replies for {} as read.", proj.name));
            }
        }
        if let Some(uid) = parsed.dismiss_update_id {
            store.dismiss_attention(uid).map_err(|e| e.to_string())?;
            summary_parts.push(format!("Dismissed Needs you on update #{uid}."));
        }
        if !mark_reply_ids.is_empty() {
            store.mark_replies_read(&mark_reply_ids).map_err(|e| e.to_string())?;
            summary_parts.push(format!(
                "Marked {} repl{} as read.",
                mark_reply_ids.len(),
                if mark_reply_ids.len() == 1 { "y" } else { "ies" }
            ));
        }
        let mut added_todos = Vec::new();
        if let Some(proj) = &project_opt {
            for title in add_todos {
                added_todos.push(store.add_todo(proj.id, &title).map_err(|e| e.to_string())?);
            }
        }
        if !added_todos.is_empty() {
            summary_parts.push(format!("Added {} to-do(s).", added_todos.len()));
        }
        for &tid in &complete_todos {
            store.set_todo_done(tid, true).map_err(|e| e.to_string())?;
        }
        if !complete_todos.is_empty() {
            summary_parts.push(format!("Completed {} to-do(s).", complete_todos.len()));
        }
        for &tid in &reopen_todos {
            store.set_todo_done(tid, false).map_err(|e| e.to_string())?;
        }
        if !reopen_todos.is_empty() {
            summary_parts.push(format!("Reopened {} to-do(s).", reopen_todos.len()));
        }
        let refreshed_project = match &project_opt {
            Some(p) => store.project(p.id).map_err(|e| e.to_string())?.map(|proj| project_json(&proj)),
            None => None,
        };
        let added_json: Vec<_> = added_todos.iter().map(todo_json).collect();
        Ok(ToolOutput::text(
            summary_parts.join(" "),
            json!({
                "project": refreshed_project.unwrap_or(Value::Null),
                "pulse": pulse_json,
                "added_todos": added_json,
                "completed_todos": complete_todos,
                "reopened_todos": reopen_todos,
                "marked_reply_ids": mark_reply_ids,
                "dismissed_update_id": parsed.dismiss_update_id,
            }),
        ))
    }

    fn delete_item(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: DeleteItemArgs = decode_args("delete_item", args)?;
        let store = self.open_store()?;
        match parsed.kind {
            DeleteKind::Update => {
                store.delete_update(parsed.id).map_err(|e| e.to_string())?;
                Ok(ToolOutput::text(format!("Deleted update #{}.", parsed.id), json!({ "kind": "update", "deleted": parsed.id })))
            }
            DeleteKind::Todo => {
                store.delete_todo(parsed.id).map_err(|e| e.to_string())?;
                Ok(ToolOutput::text(format!("Deleted to-do #{}.", parsed.id), json!({ "kind": "todo", "deleted": parsed.id })))
            }
        }
    }

    fn legacy_get_pending_decisions(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyPendingArgs = decode_args("get_pending_decisions", args)?;
        let store = self.open_store()?;
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let scope_project = if project_name_arg.is_some() || project_path_arg.is_some() {
            self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)?
        } else {
            None
        };
        let pid = scope_project.as_ref().map(|p| p.id);
        let unread = store.unread_human_replies(pid).map_err(|e| e.to_string())?;
        if parsed.mark_read.unwrap_or(false) && !unread.is_empty() {
            let rids: Vec<i64> = unread.iter().map(|r| r.id).collect();
            store.mark_replies_read(&rids).map_err(|e| e.to_string())?;
        }
        let summaries = store.projects().map_err(|e| e.to_string())?;
        let names: HashMap<i64, String> = summaries.iter().map(|s| (s.project.id, s.project.name.clone())).collect();
        let needs_you: Vec<_> =
            summaries.into_iter().filter(|s| !s.project.archived && s.needs_you && pid.is_none_or(|id| s.project.id == id)).collect();
        let now = now_ms();
        let mut text = String::new();
        if unread.is_empty() && needs_you.is_empty() {
            text.push_str("No pending decisions or unread replies.");
        } else {
            if !unread.is_empty() {
                text.push_str(&format!("Unread human replies ({}):", unread.len()));
                for r in &unread {
                    let pname = names.get(&r.project_id).map(String::as_str).unwrap_or("Unknown");
                    text.push_str(&format!(
                        "\n- reply #{} ({pname}, update #{}, {}): {}",
                        r.id,
                        r.update_id,
                        ago(r.created_at, now),
                        r.body
                    ));
                }
            }
            if !needs_you.is_empty() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&format!("Open Decision/Blocked cards ({}):", needs_you.len()));
                for s in &needs_you {
                    let uid = s.last_update_id.unwrap_or(0);
                    let when = s.last_at.map(|t| ago(t, now)).unwrap_or_else(|| "recently".into());
                    let body = s.last_body.as_deref().unwrap_or("");
                    text.push_str(&format!("\n- {} #{uid} ({when}): {body}", s.project.name));
                }
            }
        }
        let unread_json: Vec<_> = unread
            .iter()
            .map(|r| {
                let mut v = reply_json(r);
                v["project"] = json!(names.get(&r.project_id).cloned().unwrap_or_default());
                v
            })
            .collect();
        let flagged_json: Vec<_> = needs_you
            .iter()
            .filter_map(|s| {
                Some(json!({
                    "id": s.last_update_id?,
                    "project_id": s.project.id,
                    "project": s.project.name,
                    "agent": s.last_agent,
                    "body": s.last_body.as_deref()?,
                    "created_at": s.last_at?,
                }))
            })
            .collect();
        Ok(ToolOutput::text(text, json!({ "unread_replies": unread_json, "needs_you_updates": flagged_json })))
    }

    fn legacy_dismiss_attention(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyDismissArgs = decode_args("dismiss_attention", args)?;
        let store = self.open_store()?;
        if let Some(id) = parsed.id {
            store.dismiss_attention(id).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(format!("Dismissed Needs you on update #{id}."), json!({ "dismissed_id": id })));
        }
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let Some(project) = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)? else {
            return Err("no matching project found; pass `id` or a valid `project`/`project_path`".into());
        };
        let dismissed = store.dismiss_project_attention(project.id).map_err(|e| e.to_string())?;
        let text = if dismissed {
            format!("Dismissed Needs you for {}.", project.name)
        } else {
            format!("{} has no updates to dismiss.", project.name)
        };
        Ok(ToolOutput::text(text, json!({ "project": project_json(&project), "dismissed": dismissed })))
    }

    fn legacy_mark_replies_read(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyMarkRepliesReadArgs = decode_args("mark_replies_read", args)?;
        let store = self.open_store()?;
        if let Some(ids) = parsed.ids {
            store.mark_replies_read(&ids).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(
                format!("Marked {} repl{} as read.", ids.len(), if ids.len() == 1 { "y" } else { "ies" }),
                json!({ "marked_ids": ids }),
            ));
        }
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let Some(project) = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)? else {
            return Err("no matching project found; pass `ids` or a valid `project`/`project_path`".into());
        };
        store.mark_project_replies_read(project.id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Marked unread replies for {} as read.", project.name), json!({ "project": project_json(&project) })))
    }

    fn legacy_set_project_brief(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyBriefArgs = decode_args("set_project_brief", args)?;
        let clear = parsed.clear.unwrap_or(false);
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let project =
            self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), !clear)?.ok_or("no project")?;
        let brief = if clear {
            None
        } else {
            let text = non_empty(parsed.brief).ok_or("pass `brief` text or set `clear: true`")?;
            Some(text)
        };
        store.set_project_brief(project.id, brief.as_deref()).map_err(|e| e.to_string())?;
        let updated = store.project(project.id).map_err(|e| e.to_string())?.ok_or("project vanished")?;
        let text = match &updated.brief {
            Some(b) => format!("Updated pinned brief for {}:\n{b}", updated.name),
            None => format!("Cleared pinned brief for {}.", updated.name),
        };
        Ok(ToolOutput::text(text, json!({ "project": project_json(&updated) })))
    }

    fn legacy_pulse(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyPulseArgs = decode_args("pulse", args)?;
        let clear = parsed.clear.unwrap_or(false);
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let agent = self.resolve_agent(non_empty(parsed.agent).as_deref());
        let store = self.open_store()?;
        let project =
            self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), !clear)?.ok_or("no project")?;
        if clear {
            store.clear_pulse(project.id, &agent).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(
                format!("Cleared live pulse for {} ({agent}).", project.name),
                json!({ "cleared": true, "project": project_json(&project), "agent": agent }),
            ));
        }
        let status = validate_pulse_status(parsed.status)?;
        let git_dir = if project_path_arg.is_some() || project_name_arg.is_none() {
            Some(self.base(project_path_arg.as_deref()))
        } else {
            project.path.as_deref().map(PathBuf::from).filter(|p| p.exists())
        };
        let meta = git_dir.as_deref().map(detect_git_meta).unwrap_or_default();
        let branch = non_empty(parsed.branch).or(meta.branch);
        let ttl_secs = parsed.ttl_seconds.unwrap_or(120).clamp(5, 3600);
        let ttl_ms = (ttl_secs * 1000) as i64;
        let pulse = store.set_pulse(project.id, &agent, branch.as_deref(), &status, ttl_ms).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(
            format!("Pulse active on {} ({}): {}", project.name, pulse.agent, pulse.status),
            json!({
                "project": project_json(&project),
                "pulse": {
                    "agent": pulse.agent,
                    "branch": pulse.branch,
                    "status": pulse.status,
                    "updated_at": pulse.updated_at,
                    "expires_at": pulse.expires_at,
                }
            }),
        ))
    }

    fn legacy_list_todos(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyListTodosArgs = decode_args("list_todos", args)?;
        let open_only = parsed.open_only.unwrap_or(false);
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let Some(project) = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), false)? else {
            let name = detect_project(&self.base(project_path_arg.as_deref())).name;
            return Ok(ToolOutput::text(format!("{name} has no to-dos."), json!({ "project": null, "todos": [] })));
        };
        let mut todos = store.todos(project.id).map_err(|e| e.to_string())?;
        if open_only {
            todos.retain(|t| !t.done);
        }
        let text = if todos.is_empty() {
            format!("{} has no{} to-dos.", project.name, if open_only { " open" } else { "" })
        } else {
            let mut out = format!("To-dos for {} ({}):", project.name, todos.len());
            for t in &todos {
                let mark = if t.done { "x" } else { " " };
                out.push_str(&format!("\n- [{mark}] {} (#{})", t.title, t.id));
            }
            out
        };
        let todos_json: Vec<_> = todos.iter().map(todo_json).collect();
        Ok(ToolOutput::text(text, json!({ "project": project_json(&project), "todos": todos_json })))
    }

    fn legacy_add_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyAddTodoArgs = decode_args("add_todo", args)?;
        let project_name_arg = non_empty(parsed.project);
        let project_path_arg = non_empty(parsed.project_path);
        let store = self.open_store()?;
        let project = self.resolve_project(&store, project_name_arg.as_deref(), project_path_arg.as_deref(), true)?.ok_or("no project")?;
        let todo = store.add_todo(project.id, &parsed.title).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(
            format!("Added to-do #{} to {}: {}", todo.id, project.name, todo.title),
            json!({ "project": project_json(&project), "todo": todo_json(&todo) }),
        ))
    }

    fn legacy_toggle_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyToggleTodoArgs = decode_args("toggle_todo", args)?;
        let store = self.open_store()?;
        let done = match parsed.done {
            Some(explicit) => explicit,
            None => !store.todo(parsed.id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no to-do {}", parsed.id))?.done,
        };
        let todo = store.set_todo_done(parsed.id, done).map_err(|e| e.to_string())?;
        let state = if todo.done { "Completed" } else { "Reopened" };
        Ok(ToolOutput::text(format!("{state} to-do #{}: {}", todo.id, todo.title), json!({ "todo": todo_json(&todo) })))
    }

    fn legacy_delete_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyIdArgs = decode_args("delete_todo", args)?;
        let store = self.open_store()?;
        store.delete_todo(parsed.id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Deleted to-do #{}.", parsed.id), json!({ "deleted": parsed.id })))
    }

    fn legacy_delete_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let parsed: LegacyIdArgs = decode_args("delete_update", args)?;
        let store = self.open_store()?;
        store.delete_update(parsed.id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Deleted update #{}.", parsed.id), json!({ "deleted": parsed.id })))
    }
}

fn reply_json(r: &Reply) -> Value {
    json!({
        "id": r.id,
        "update_id": r.update_id,
        "project_id": r.project_id,
        "author": r.author,
        "from_human": r.from_human,
        "body": r.body,
        "created_at": r.created_at,
        "read_at": r.read_at,
    })
}

fn format_update_line(project: Option<&str>, u: &FeedUpdate, replies: &[Reply], now: i64) -> String {
    let branch = u.branch.as_deref().map(|b| format!(" · {b}")).unwrap_or_default();
    let diff = u.diff_stat.as_deref().map(|d| format!(" · {d}")).unwrap_or_default();
    let header = match project {
        Some(p) => format!("── {p} · {} · {}{branch}{diff} (#{})", u.agent, ago(u.created_at, now), u.id),
        None => format!("── {} · {}{branch}{diff} (#{})", u.agent, ago(u.created_at, now), u.id),
    };
    let mut out = format!("{header}\n{}", u.body);
    if !u.media.is_empty() {
        for m in &u.media {
            out.push_str(&format!("\n[media: {}]", m.path.display()));
        }
    } else if let Some(shot) = &u.screenshot {
        out.push_str(&format!("\n[media: {}]", shot.display()));
    }
    for r in replies.iter().filter(|r| r.update_id == u.id) {
        let badge = if r.from_human && r.read_at.is_none() { " [unread]" } else { "" };
        out.push_str(&format!("\n  ↳ {} ({}){badge}: {}", r.author, ago(r.created_at, now), r.body));
    }
    out
}

fn collect_screenshots(store_dir: &Path, updates: &[FeedUpdate]) -> Vec<ImageBlock> {
    let Ok(media_root) = store_dir.join("media").canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut total_bytes = 0usize;
    for u in updates {
        let paths: Vec<&Path> = if !u.media.is_empty() {
            u.media.iter().map(|m| m.path.as_path()).collect()
        } else {
            u.screenshot.as_deref().into_iter().collect()
        };
        for path in paths {
            let remaining = MAX_INLINE_SCREENSHOT_TOTAL_BYTES.saturating_sub(total_bytes);
            if out.len() >= MAX_INLINE_SCREENSHOTS || remaining == 0 {
                return out;
            }
            if let Some((block, size)) = read_image_block(&media_root, path, remaining) {
                total_bytes += size;
                out.push(block);
            }
        }
    }
    out
}

fn read_image_block(media_root: &Path, path: &Path, max_total_remaining: usize) -> Option<(ImageBlock, usize)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mime_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    let link_meta = std::fs::symlink_metadata(path).ok()?;
    if link_meta.file_type().is_symlink() || !link_meta.is_file() {
        return None;
    }
    let canon = path.canonicalize().ok()?;
    if !canon.starts_with(media_root) {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(&canon).ok()?;
    let meta = file.metadata().ok()?;
    let cap = MAX_INLINE_SCREENSHOT_BYTES.min(u64::try_from(max_total_remaining).unwrap_or(u64::MAX));
    if !meta.is_file() || meta.len() > cap {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(cap + 1).read_to_end(&mut bytes).ok()?;
    if (bytes.len() as u64) > cap {
        return None;
    }
    let raw_len = bytes.len();
    Some((ImageBlock { data_base64: base64_encode(&bytes), mime_type }, raw_len))
}

pub(super) fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}

pub(super) fn tools() -> Value {
    let project = project_schema();
    let project_path = project_path_schema();
    json!([
        {
            "name": "list_projects",
            "title": "List projects",
            "description": "List Feeder projects with update counts, unread replies, open to-dos, active pulses, and attention state. Pass `filter: \"pending\"` for unread human replies and open Decision/Blocked updates.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "filter": {
                        "type": "string",
                        "enum": ["active", "pending", "archived", "all"]
                    }
                }
            },
            "annotations": ann_read_only()
        },
        {
            "name": "get_feed",
            "title": "Read a project's feed",
            "description": "Read a project's latest updates, brief, to-dos, and unread replies. Pass `task` keywords when starting work to get up to 3 relevant Library notes.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "project": project,
                    "project_path": project_path,
                    "task": { "type": "string", "maxLength": 160, "description": "Task keywords to match Library notes." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 5." },
                    "include_screenshots": { "type": "boolean", "description": "Inline PNG/JPEG/WEBP/GIF blocks (4 MB total cap)." }
                }
            },
            "annotations": ann_read_only()
        },
        {
            "name": "search_updates",
            "title": "Search or list Feed updates",
            "description": "Search updates across or within projects. Omit `query` for recent activity.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": { "type": "string" },
                    "project": project,
                    "project_path": project_path,
                    "include_archived": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 10." },
                    "include_screenshots": { "type": "boolean" }
                }
            },
            "annotations": ann_read_only()
        },
        {
            "name": "post_update",
            "title": "Post a Feed update",
            "description": "Post a 1-3 block Markdown update (<= 900 chars: Done / Why / Next or Decision needed / Blocked) to a project feed, clearing active pulse. Can clear unread replies, update to-dos, mark helpful notes, and flag a lesson in one call.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "summary": { "type": "string", "description": "1-3 Markdown blocks (<= 900 chars)." },
                    "project": project,
                    "project_path": project_path,
                    "agent": { "type": "string" },
                    "branch": { "type": "string" },
                    "diff_stat": { "type": "string" },
                    "media": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": MAX_MEDIA_PER_UPDATE,
                        "description": "Up to 8 screenshot/clip paths."
                    },
                    "mark_replies_read": { "type": "boolean" },
                    "helped_notes": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_BATCH_ITEMS },
                    "complete_todos": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_BATCH_ITEMS },
                    "add_todos": { "type": "array", "items": { "type": "string" }, "maxItems": MAX_BATCH_ITEMS },
                    "flag_lesson": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "kind": { "type": "string", "enum": ["missing", "wrong", "outdated"] },
                            "note": { "type": "string" },
                            "page_id": { "type": "integer" },
                            "suggestion": { "type": "string", "description": "Full Markdown draft (<= 10,000 chars)." },
                            "shared": { "type": "boolean" }
                        },
                        "required": ["note"]
                    }
                },
                "required": ["summary"]
            },
            "annotations": ann_local_write()
        },
        {
            "name": "reply_to_update",
            "title": "Reply to an update",
            "description": "Post an agent reply on an update and mark unread human replies on that update read by default.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "integer" },
                    "summary": { "type": "string" },
                    "agent": { "type": "string" },
                    "mark_replies_read": { "type": "boolean", "description": "Default true." }
                },
                "required": ["id", "summary"]
            },
            "annotations": ann_local_write()
        },
        {
            "name": "edit_update",
            "title": "Edit an update",
            "description": "Replace an update's Markdown body by ID.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "integer" },
                    "summary": { "type": "string" }
                },
                "required": ["id", "summary"]
            },
            "annotations": ann_local_idempotent()
        },
        {
            "name": "update_project",
            "title": "Update project state",
            "description": "Batch pulse, brief, attention dismissal, reply acks, and checklist updates for a project.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "project": project,
                    "project_path": project_path,
                    "agent": { "type": "string" },
                    "pulse": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "status": { "type": "string" },
                            "branch": { "type": "string" },
                            "ttl_seconds": { "type": "integer", "minimum": 5, "maximum": 3600 },
                            "clear": { "type": "boolean" }
                        }
                    },
                    "brief": { "type": "string", "description": "Pinned Goal/Focus/Commands (<= 600 chars)." },
                    "clear_brief": { "type": "boolean" },
                    "dismiss_attention": { "type": "boolean" },
                    "dismiss_update_id": { "type": "integer" },
                    "mark_replies_read": { "type": "boolean" },
                    "mark_reply_ids": { "type": "array", "items": { "type": "integer" } },
                    "add_todos": { "type": "array", "items": { "type": "string" }, "maxItems": MAX_BATCH_ITEMS },
                    "complete_todos": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_BATCH_ITEMS },
                    "reopen_todos": { "type": "array", "items": { "type": "integer" }, "maxItems": MAX_BATCH_ITEMS }
                }
            },
            "annotations": ann_local_idempotent()
        },
        {
            "name": "delete_item",
            "title": "Delete an update or to-do",
            "description": "Permanently delete a Feed update (and its media) or a project to-do.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "enum": ["update", "todo"] },
                    "id": { "type": "integer" }
                },
                "required": ["kind", "id"]
            },
            "annotations": ann_destructive()
        }
    ])
}
