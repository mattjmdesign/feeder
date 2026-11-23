use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use feeder_core::{
    FeedUpdate, GUIDE, MAX_CHARS, MAX_MEDIA_PER_UPDATE, MAX_PARAGRAPHS, NOTEBOOK_GUIDE, NewUpdate, PageFilter, Project, Reply, Store,
    agent_display_name, agent_from_env, ago, detect_git_meta, detect_project, now_ms, parse_attention,
};
use serde_json::{Value, json};

use crate::{project_json, todo_json, update_json};

mod gardener;
mod notebook;

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_INLINE_SCREENSHOTS: usize = 6;
const MAX_INLINE_SCREENSHOT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RPC_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Agent,
    Gardener(i64),
}

struct Server {
    client: Option<String>,
    cwd: PathBuf,
    store_dir: Option<PathBuf>,
    store: RefCell<Option<Store>>,
    role: Role,
}

struct ToolOutput {
    text: String,
    data: Value,
    images: Vec<ImageBlock>,
}

struct ImageBlock {
    data_base64: String,
    mime_type: &'static str,
}

impl ToolOutput {
    fn text(text: String, data: Value) -> Self {
        Self { text, data, images: Vec::new() }
    }
}

pub fn serve(role: Role) -> Result<(), String> {
    let mut server = Server::new(None, std::env::current_dir().map_err(|e| e.to_string())?, None);
    server.role = role;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = read_line_capped(&mut reader, &mut buf, MAX_RPC_LINE_BYTES).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let reply = if buf.len() > MAX_RPC_LINE_BYTES {
            Some(error(Value::Null, -32600, "request exceeds maximum size"))
        } else {
            let line = String::from_utf8_lossy(&buf);
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    let replies: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                    (!replies.is_empty()).then_some(Value::Array(replies))
                }
                Ok(msg) => server.handle(&msg),
                Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
            }
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn read_line_capped<R: BufRead>(reader: &mut R, out: &mut Vec<u8>, max_bytes: usize) -> std::io::Result<usize> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total);
        }
        let (consume, done) = match available.iter().position(|&b| b == b'\n') {
            Some(pos) => (pos + 1, true),
            None => (available.len(), false),
        };
        if out.len() <= max_bytes {
            let keep = consume.min(max_bytes + 1 - out.len());
            out.extend_from_slice(&available[..keep]);
        }
        reader.consume(consume);
        total += consume;
        if done {
            return Ok(total);
        }
    }
}

impl Server {
    fn new(client: Option<String>, cwd: PathBuf, store_dir: Option<PathBuf>) -> Self {
        Self { client, cwd, store_dir, store: RefCell::new(None), role: Role::Agent }
    }

    fn open_store(&self) -> Result<Ref<'_, Store>, String> {
        if self.store.borrow().is_none() {
            let opened = match &self.store_dir {
                Some(dir) => Store::open(dir),
                None => Store::open_default(),
            }
            .map_err(|e| e.to_string())?;
            *self.store.borrow_mut() = Some(opened);
        }
        Ok(Ref::map(self.store.borrow(), |opt| opt.as_ref().expect("store initialized")))
    }

    fn resolve_agent(&self, args: &Value) -> String {
        str_arg(args, "agent")
            .map(agent_display_name)
            .or_else(agent_from_env)
            .or_else(|| self.client.as_deref().map(agent_display_name))
            .unwrap_or_else(|| "Unknown agent".into())
    }

    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?;
        let result = match method {
            "initialize" => self.initialize(&params).map_err(|e| (-32603, e)),
            "ping" => Ok(json!({})),
            "tools/list" => match self.role {
                Role::Agent => Ok(json!({ "tools": all_tools() })),
                Role::Gardener(run) => {
                    self.curator_mode(run).map(|mode| json!({ "tools": gardener::tools(mode) })).map_err(|e| (-32603, e))
                }
            },
            "tools/call" => Ok(self.call(&params)),
            "resources/list" if self.role == Role::Agent => self.resources_list().map_err(|e| (-32603, e)),
            "resources/templates/list" if self.role == Role::Agent => Ok(json!({
                "resourceTemplates": [{
                    "uriTemplate": "feeder://project/{slug}/feed",
                    "name": "Project feed",
                    "description": "Latest updates for a Feeder project in Markdown.",
                    "mimeType": "text/markdown"
                }, {
                    "uriTemplate": "feeder://notebook/{id}",
                    "name": "Notebook page",
                    "description": "A single Feeder notebook lesson in Markdown.",
                    "mimeType": "text/markdown"
                }]
            })),
            "resources/read" if self.role == Role::Agent => self.resources_read(&params),
            "prompts/list" if self.role == Role::Agent => Ok(json!({ "prompts": [{
                "name": "update",
                "title": "Post a Feeder update",
                "description": "Write this project's update the Feeder way and post it."
            }, {
                "name": "lesson",
                "title": "Flag a lesson for Curator",
                "description": "Flag missing or incorrect Notebook knowledge uncovered in this session."
            }] })),
            "prompts/get" if self.role == Role::Agent => match params.get("name").and_then(Value::as_str) {
                Some("update") => Ok(json!({
                    "description": "Write this project's update the Feeder way and post it.",
                    "messages": [{ "role": "user", "content": { "type": "text", "text": format!(
                        "{GUIDE}\n\nNow look at what changed in this project during this session (and `git log`/`git status` if useful), decide whether a screenshot or clip helps, then call post_update."
                    ) } }]
                })),
                Some("lesson") => Ok(json!({
                    "description": "Flag missing or incorrect Notebook knowledge uncovered in this session.",
                    "messages": [{ "role": "user", "content": { "type": "text", "text": format!(
                        "{NOTEBOOK_GUIDE}\n\nLook back over this session for a reusable lesson. Search the Notebook first. If you find a gap or a correction, call flag_knowledge with the verified fix in suggestion when available. If the Notebook already covers it accurately, skip the flag."
                    ) } }]
                })),
                _ => Err((-32602, "unknown prompt".to_string())),
            },
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, String> {
        self.client = params.pointer("/clientInfo/name").and_then(Value::as_str).map(str::to_string);
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSIONS[1]);
        let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[1] };
        let (capabilities, instructions) = match self.role {
            Role::Agent => (
                json!({ "tools": {}, "prompts": {}, "resources": {} }),
                format!(
                    "Feeder is the user's local project update centre. Post an update with post_update when you finish a meaningful piece of work or when asked, and call get_feed before picking a project back up. Search the shared Notebook before non-trivial debugging. Flag gaps and suggest fixes with flag_knowledge. Curator maintains the shared notes.\n\n{GUIDE}\n\n{NOTEBOOK_GUIDE}"
                ),
            ),
            Role::Gardener(run) => (json!({ "tools": {} }), self.curator_mode(run)?.instructions().to_string()),
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": capabilities,
            "serverInfo": { "name": "feeder", "version": env!("CARGO_PKG_VERSION") },
            "instructions": instructions
        }))
    }

    fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let outcome = match (self.role, name) {
            (Role::Gardener(run), name) => self.gardener_call(run, name, &args),
            (Role::Agent, name) => self.agent_call(name, &args),
        };
        tool_result(outcome)
    }

    fn agent_call(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        let args = args.clone();
        match name {
            "post_update" => self.post_update(&args),
            "edit_update" => self.edit_update(&args),
            "delete_update" => self.delete_update(&args),
            "get_feed" => self.get_feed(&args),
            "get_recent_activity" => self.get_recent_activity(&args),
            "search_updates" => self.search_updates(&args),
            "list_projects" => self.list_projects(&args),
            "reply_to_update" => self.reply_to_update(&args),
            "get_pending_decisions" => self.get_pending_decisions(&args),
            "dismiss_attention" => self.dismiss_attention(&args),
            "mark_replies_read" => self.mark_replies_read(&args),
            "set_project_brief" => self.set_project_brief(&args),
            "pulse" => self.pulse(&args),
            "list_todos" => self.list_todos(&args),
            "add_todo" => self.add_todo(&args),
            "toggle_todo" => self.toggle_todo(&args),
            "delete_todo" => self.delete_todo(&args),
            other if notebook::TOOLS.contains(&other) => self.notebook_call(other, &args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }
}

fn tool_result(outcome: Result<ToolOutput, String>) -> Value {
    {
        match outcome {
            Ok(out) => {
                let mut content = vec![json!({ "type": "text", "text": out.text })];
                for img in out.images {
                    content.push(json!({ "type": "image", "data": img.data_base64, "mimeType": img.mime_type }));
                }
                json!({ "content": content, "structuredContent": out.data, "isError": false })
            }
            Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
        }
    }
}

impl Server {
    fn resources_list(&self) -> Result<Value, String> {
        let store = self.open_store()?;
        let projects = store.projects().map_err(|e| e.to_string())?;
        let mut resources = vec![
            json!({
                "uri": "feeder://projects",
                "name": "Projects",
                "description": "All Feeder projects with update counts, attention status, active pulses, and last activity (JSON).",
                "mimeType": "application/json"
            }),
            json!({
                "uri": "feeder://notebook",
                "name": "Notebook",
                "description": "Shared index of current Feeder notebook lessons across all projects (Markdown).",
                "mimeType": "text/markdown"
            }),
        ];
        for sum in projects {
            if sum.project.archived {
                continue;
            }
            resources.push(json!({
                "uri": format!("feeder://project/{}/feed", sum.project.slug),
                "name": format!("{} feed", sum.project.name),
                "description": format!("Latest updates for {}", sum.project.name),
                "mimeType": "text/markdown"
            }));
        }
        Ok(json!({ "resources": resources }))
    }

    fn resources_read(&self, params: &Value) -> Result<Value, (i64, String)> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or((-32602, "`uri` is required".to_string()))?;
        let store = self.open_store().map_err(|e| (-32603, e))?;
        if uri == "feeder://projects" {
            let now = now_ms();
            let list: Vec<Value> = store
                .projects()
                .map_err(|e| (-32603, e.to_string()))?
                .into_iter()
                .map(|p| {
                    let mut v = project_json(&p.project);
                    v["updates"] = json!(p.updates);
                    v["last_at"] = json!(p.last_at);
                    v["last_ago"] = json!(p.last_at.map(|at| ago(at, now)));
                    v["last_agent"] = json!(p.last_agent);
                    v["needs_you"] = json!(p.needs_you);
                    v["unread_replies"] = json!(p.unread_replies);
                    v["open_todos"] = json!(p.open_todos);
                    v["active_pulse"] = p
                        .active_pulse
                        .as_ref()
                        .map(|pulse| json!({ "agent": pulse.agent, "branch": pulse.branch, "status": pulse.status }))
                        .unwrap_or(Value::Null);
                    v
                })
                .collect();
            let text = serde_json::to_string_pretty(&list).map_err(|e| (-32603, e.to_string()))?;
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "application/json", "text": text }] }));
        }
        if uri == "feeder://notebook" {
            let pages = store.pages(&PageFilter::default()).map_err(|e| (-32603, e.to_string()))?;
            let mut md = String::from("# Feeder Notebook\n");
            if pages.is_empty() {
                md.push_str("\nNo notebook pages yet.\n");
            } else {
                for p in &pages {
                    md.push_str(&format!("\n- {}\n", p.summary_line()));
                }
            }
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": md }] }));
        }
        if let Some(id_str) = uri.strip_prefix("feeder://notebook/") {
            let id = id_str.parse::<i64>().map_err(|_| (-32002, format!("resource not found: {uri}")))?;
            let page = store.page(id).map_err(|e| (-32603, e.to_string()))?.ok_or((-32002, format!("notebook page not found: {id}")))?;
            let md = page.to_markdown(crate::notebook::project_name(&store, &page).as_deref(), now_ms());
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": md }] }));
        }
        if let Some(rest) = uri.strip_prefix("feeder://project/")
            && let Some(slug) = rest.strip_suffix("/feed")
            && !slug.is_empty()
        {
            let project =
                store.find_project(slug).map_err(|e| (-32603, e.to_string()))?.ok_or((-32002, format!("project not found: {slug}")))?;
            let feed = store.feed(project.id, 20, None).map_err(|e| (-32603, e.to_string()))?;
            let ids: Vec<i64> = feed.iter().map(|u| u.id).collect();
            let replies = store.replies_for_updates(&ids).map_err(|e| (-32603, e.to_string()))?;
            let todos = store.todos(project.id).map_err(|e| (-32603, e.to_string()))?;
            let now = now_ms();
            let mut md = format!("# {}\n", project.name);
            if let Some(brief) = &project.brief {
                md.push_str(&format!("\n> **Brief:** {}\n", brief.replace('\n', "\n> ")));
            }
            if !todos.is_empty() {
                md.push_str("\n## To-dos\n\n");
                for t in &todos {
                    let mark = if t.done { "x" } else { " " };
                    md.push_str(&format!("- [{mark}] {} (#{})\n", t.title, t.id));
                }
            }
            if feed.is_empty() {
                md.push_str("\nNo updates yet.\n");
            } else {
                for u in feed {
                    let branch = u.branch.as_deref().map(|b| format!(" · `{b}`")).unwrap_or_default();
                    let diff = u.diff_stat.as_deref().map(|d| format!(" · `{d}`")).unwrap_or_default();
                    md.push_str(&format!("\n## {} · {}{branch}{diff} (#{})\n\n{}\n", u.agent, ago(u.created_at, now), u.id, u.body));
                    if !u.media.is_empty() {
                        for m in &u.media {
                            md.push_str(&format!("\n[media: {}]\n", m.path.display()));
                        }
                    } else if let Some(shot) = &u.screenshot {
                        md.push_str(&format!("\n[media: {}]\n", shot.display()));
                    }
                    for r in replies.iter().filter(|r| r.update_id == u.id) {
                        md.push_str(&format!("\n- **{}** ({}): {}\n", r.author, ago(r.created_at, now), r.body));
                    }
                }
            }
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": md }] }));
        }
        Err((-32002, format!("resource not found: {uri}")))
    }

    fn post_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let summary = str_arg(args, "summary").ok_or("`summary` is required")?;
        let store = self.open_store()?;
        let project = self.project(&store, args, true)?.ok_or("no project")?;
        let agent = self.resolve_agent(args);
        let base = self.base(args);
        let resolve_path = |raw: &str| {
            let p = PathBuf::from(raw);
            if p.is_absolute() { p } else { base.join(p) }
        };
        let screenshot = str_arg(args, "screenshot_path").map(resolve_path);
        let screenshots: Vec<PathBuf> = args
            .get("screenshots")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(resolve_path).collect())
            .unwrap_or_default();
        let meta = detect_git_meta(&base);
        let branch = str_arg(args, "branch").map(str::to_string).or(meta.branch);
        let diff_stat = str_arg(args, "diff_stat").map(str::to_string).or(meta.diff_stat);
        let commit_sha = meta.commit_sha;
        let posted = store
            .post(project.id, NewUpdate { agent, body: summary.to_string(), screenshot, screenshots, branch, diff_stat, commit_sha })
            .map_err(|e| e.to_string())?;
        let unread = store.unread_human_replies(Some(project.id)).map_err(|e| e.to_string())?;
        let media_note = match posted.media.len() {
            0 => String::new(),
            1 => " (1 attachment)".to_string(),
            n => format!(" ({n} attachments)"),
        };
        let mut text = format!("Posted update #{} to {}'s feed as {}{media_note}.", posted.id, project.name, posted.agent);
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
        Ok(ToolOutput::text(
            text,
            json!({ "project": project_json(&project), "update": update_json(&posted), "unread_replies": unread_json }),
        ))
    }

    fn edit_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let summary = str_arg(args, "summary").ok_or("`summary` is required")?;
        let store = self.open_store()?;
        let updated = store.edit_update(id, summary).map_err(|e| e.to_string())?;
        let text = format!("Updated #{id}.");
        Ok(ToolOutput::text(text, json!({ "update": update_json(&updated) })))
    }

    fn delete_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        store.delete_update(id).map_err(|e| e.to_string())?;
        let text = format!("Deleted update #{id}.");
        Ok(ToolOutput::text(text, json!({ "deleted": id })))
    }

    fn get_feed(&self, args: &Value) -> Result<ToolOutput, String> {
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5).clamp(1, 50) as usize;
        let include_shots = args.get("include_screenshots").and_then(Value::as_bool).unwrap_or(false);
        let mark_read = args.get("mark_replies_read").and_then(Value::as_bool).unwrap_or(false);
        let task = str_arg(args, "task");
        let store = self.open_store()?;
        let Some(project) = self.project(&store, args, false)? else {
            let name = detect_project(&self.base(args)).name;
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
        if mark_read && !unread.is_empty() {
            let rids: Vec<i64> = unread.iter().map(|r| r.id).collect();
            store.mark_replies_read(&rids).map_err(|e| e.to_string())?;
        }
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
        if let Some(task) = task.filter(|task| task.trim().chars().take(160).any(char::is_alphanumeric)) {
            let query: String = task.trim().chars().take(160).collect();
            let notes = store.search_project_notes(project.id, &query, &PageFilter::default(), 3).map_err(|e| e.to_string())?;
            if !notes.is_empty() {
                text.push_str("\n\nNotebook notes for this task (open with read_page):");
            }
            for page in notes {
                let url = format!("feeder://notebook/{}", page.id);
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
        let images = if include_shots { collect_screenshots(&feed) } else { Vec::new() };
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

    fn get_recent_activity(&self, args: &Value) -> Result<ToolOutput, String> {
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, 50) as usize;
        let include_shots = args.get("include_screenshots").and_then(Value::as_bool).unwrap_or(false);
        let store = self.open_store()?;
        let timeline = store.timeline(limit).map_err(|e| e.to_string())?;
        let ids: Vec<i64> = timeline.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let projects: HashMap<i64, Project> =
            store.projects().map_err(|e| e.to_string())?.into_iter().map(|s| (s.project.id, s.project)).collect();
        let now = now_ms();
        let mut text = if timeline.is_empty() {
            "No recent updates across active projects.".to_string()
        } else {
            format!("Latest {} update(s) across active projects:", timeline.len())
        };
        let mut list = Vec::new();
        for u in &timeline {
            let pname = projects.get(&u.project_id).map(|p| p.name.as_str()).unwrap_or("Unknown");
            text.push_str(&format!("\n\n{}", format_update_line(Some(pname), u, &replies, now)));
            let mut v = update_json(u);
            v["project"] = json!(pname);
            v["replies"] = json!(replies.iter().filter(|r| r.update_id == u.id).map(reply_json).collect::<Vec<_>>());
            list.push(v);
        }
        let images = if include_shots { collect_screenshots(&timeline) } else { Vec::new() };
        Ok(ToolOutput { text, data: json!({ "updates": list }), images })
    }

    fn search_updates(&self, args: &Value) -> Result<ToolOutput, String> {
        let query = str_arg(args, "query").ok_or("`query` is required")?;
        let include_archived = args.get("include_archived").and_then(Value::as_bool).unwrap_or(false);
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, 50) as usize;
        let store = self.open_store()?;
        let pid = match str_arg(args, "project") {
            Some(name) => Some(store.find_project(name).map_err(|e| e.to_string())?.ok_or_else(|| format!("no project called {name}"))?.id),
            None => None,
        };
        let hits = store.search_updates(query, pid, include_archived || pid.is_some(), limit).map_err(|e| e.to_string())?;
        let ids: Vec<i64> = hits.iter().map(|u| u.id).collect();
        let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
        let projects: HashMap<i64, Project> =
            store.projects().map_err(|e| e.to_string())?.into_iter().map(|s| (s.project.id, s.project)).collect();
        let now = now_ms();
        let mut text = if hits.is_empty() {
            format!("No updates matching \"{query}\".")
        } else {
            format!("{} match(es) for \"{query}\":", hits.len())
        };
        let mut list = Vec::new();
        for u in &hits {
            let pname = projects.get(&u.project_id).map(|p| p.name.as_str()).unwrap_or("Unknown");
            text.push_str(&format!("\n\n{}", format_update_line(Some(pname), u, &replies, now)));
            let mut v = update_json(u);
            v["project"] = json!(pname);
            list.push(v);
        }
        Ok(ToolOutput::text(text, json!({ "updates": list })))
    }

    fn list_projects(&self, args: &Value) -> Result<ToolOutput, String> {
        let include_archived = args.get("include_archived").and_then(Value::as_bool).unwrap_or(false);
        let store = self.open_store()?;
        let projects: Vec<_> =
            store.projects().map_err(|e| e.to_string())?.into_iter().filter(|p| include_archived || !p.project.archived).collect();
        let now = now_ms();
        let mut text = if projects.is_empty() { "No projects yet.".to_string() } else { String::new() };
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
            let todos = if p.open_todos > 0 { format!(", {} open to-do(s)", p.open_todos) } else { String::new() };
            let tag = if p.project.archived { " [archived]" } else { "" };
            text.push_str(&format!("{}{pin}{needs}{pulse}{tag} — {} update(s){todos}, {last}", p.project.name, p.updates));
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
        Ok(ToolOutput::text(text, json!({ "projects": list })))
    }

    fn reply_to_update(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let summary = str_arg(args, "summary").ok_or("`summary` is required")?;
        let agent = self.resolve_agent(args);
        let store = self.open_store()?;
        let reply = store.add_reply(id, &agent, false, summary).map_err(|e| e.to_string())?;
        let text = format!("Replied (#{}) to update #{id} as {}.", reply.id, reply.author);
        Ok(ToolOutput::text(text, json!({ "reply": reply_json(&reply) })))
    }

    fn get_pending_decisions(&self, args: &Value) -> Result<ToolOutput, String> {
        let mark_read = args.get("mark_read").and_then(Value::as_bool).unwrap_or(false);
        let store = self.open_store()?;
        let scope_project =
            if args.get("project").is_some() || args.get("project_path").is_some() { self.project(&store, args, false)? } else { None };
        let pid = scope_project.as_ref().map(|p| p.id);
        let unread = store.unread_human_replies(pid).map_err(|e| e.to_string())?;
        if mark_read && !unread.is_empty() {
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
                let id = s.last_update_id?;
                let body = s.last_body.as_deref()?;
                let created_at = s.last_at?;
                Some(json!({
                    "id": id,
                    "project_id": s.project.id,
                    "project": s.project.name,
                    "agent": s.last_agent,
                    "body": body,
                    "created_at": created_at,
                }))
            })
            .collect();
        Ok(ToolOutput::text(text, json!({ "unread_replies": unread_json, "needs_you_updates": flagged_json })))
    }

    fn dismiss_attention(&self, args: &Value) -> Result<ToolOutput, String> {
        let store = self.open_store()?;
        if let Some(id) = args.get("id").and_then(Value::as_i64) {
            store.dismiss_attention(id).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(format!("Dismissed Needs you on update #{id}."), json!({ "dismissed_id": id })));
        }
        let Some(project) = self.project(&store, args, false)? else {
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

    fn mark_replies_read(&self, args: &Value) -> Result<ToolOutput, String> {
        let store = self.open_store()?;
        if let Some(arr) = args.get("ids").and_then(Value::as_array) {
            let ids: Vec<i64> = arr.iter().filter_map(Value::as_i64).collect();
            store.mark_replies_read(&ids).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(
                format!("Marked {} repl{} as read.", ids.len(), if ids.len() == 1 { "y" } else { "ies" }),
                json!({ "marked_ids": ids }),
            ));
        }
        let Some(project) = self.project(&store, args, false)? else {
            return Err("no matching project found; pass `ids` or a valid `project`/`project_path`".into());
        };
        store.mark_project_replies_read(project.id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Marked unread replies for {} as read.", project.name), json!({ "project": project_json(&project) })))
    }

    fn set_project_brief(&self, args: &Value) -> Result<ToolOutput, String> {
        let store = self.open_store()?;
        let project = self.project(&store, args, true)?.ok_or("no project")?;
        let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
        let brief = if clear { None } else { str_arg(args, "brief") };
        store.set_project_brief(project.id, brief).map_err(|e| e.to_string())?;
        let updated = store.project(project.id).map_err(|e| e.to_string())?.ok_or("project vanished")?;
        let text = match &updated.brief {
            Some(b) => format!("Updated pinned brief for {}:\n{b}", updated.name),
            None => format!("Cleared pinned brief for {}.", updated.name),
        };
        Ok(ToolOutput::text(text, json!({ "project": project_json(&updated) })))
    }

    fn pulse(&self, args: &Value) -> Result<ToolOutput, String> {
        let store = self.open_store()?;
        let project = self.project(&store, args, true)?.ok_or("no project")?;
        let agent = self.resolve_agent(args);
        let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
        if clear {
            store.clear_pulse(project.id, &agent).map_err(|e| e.to_string())?;
            return Ok(ToolOutput::text(
                format!("Cleared live pulse for {} ({agent}).", project.name),
                json!({ "cleared": true, "project": project_json(&project), "agent": agent }),
            ));
        }
        let status = str_arg(args, "status").unwrap_or("Working");
        let meta = detect_git_meta(&self.base(args));
        let branch = str_arg(args, "branch").map(str::to_string).or(meta.branch);
        let ttl_secs = args.get("ttl_seconds").and_then(Value::as_u64).unwrap_or(120).clamp(5, 3600);
        let ttl_ms = (ttl_secs * 1000) as i64;
        let pulse = store.set_pulse(project.id, &agent, branch.as_deref(), status, ttl_ms).map_err(|e| e.to_string())?;
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

    fn list_todos(&self, args: &Value) -> Result<ToolOutput, String> {
        let open_only = args.get("open_only").and_then(Value::as_bool).unwrap_or(false);
        let store = self.open_store()?;
        let Some(project) = self.project(&store, args, false)? else {
            let name = detect_project(&self.base(args)).name;
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

    fn add_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let title = str_arg(args, "title").ok_or("`title` is required")?;
        let store = self.open_store()?;
        let project = self.project(&store, args, true)?.ok_or("no project")?;
        let todo = store.add_todo(project.id, title).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(
            format!("Added to-do #{} to {}: {}", todo.id, project.name, todo.title),
            json!({ "project": project_json(&project), "todo": todo_json(&todo) }),
        ))
    }

    fn toggle_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        let done = match args.get("done").and_then(Value::as_bool) {
            Some(explicit) => explicit,
            None => {
                let current = store.todo(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no to-do {id}"))?;
                !current.done
            }
        };
        let todo = store.set_todo_done(id, done).map_err(|e| e.to_string())?;
        let state = if todo.done { "Completed" } else { "Reopened" };
        Ok(ToolOutput::text(format!("{state} to-do #{}: {}", todo.id, todo.title), json!({ "todo": todo_json(&todo) })))
    }

    fn delete_todo(&self, args: &Value) -> Result<ToolOutput, String> {
        let id = args.get("id").and_then(Value::as_i64).ok_or("`id` is required")?;
        let store = self.open_store()?;
        store.delete_todo(id).map_err(|e| e.to_string())?;
        Ok(ToolOutput::text(format!("Deleted to-do #{id}."), json!({ "deleted": id })))
    }

    fn base(&self, args: &Value) -> PathBuf {
        str_arg(args, "project_path").map(PathBuf::from).unwrap_or_else(|| self.cwd.clone())
    }

    fn project(&self, store: &Store, args: &Value, create: bool) -> Result<Option<Project>, String> {
        let found = match (str_arg(args, "project"), create) {
            (Some(name), true) => store.project_named(name, None).map(Some),
            (Some(name), false) => store.find_project(name),
            (None, true) => store.project_at(&detect_project(&self.base(args))).map(Some),
            (None, false) => store.find_detected(&detect_project(&self.base(args))),
        };
        found.map_err(|e| e.to_string())
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

fn collect_screenshots(updates: &[FeedUpdate]) -> Vec<ImageBlock> {
    let mut out = Vec::new();
    for u in updates {
        let paths: Vec<&Path> = if !u.media.is_empty() {
            u.media.iter().map(|m| m.path.as_path()).collect()
        } else {
            u.screenshot.as_deref().into_iter().collect()
        };
        for path in paths {
            if out.len() >= MAX_INLINE_SCREENSHOTS {
                return out;
            }
            if let Some(block) = read_image_block(path) {
                out.push(block);
            }
        }
    }
    out
}

fn read_image_block(path: &Path) -> Option<ImageBlock> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mime_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INLINE_SCREENSHOT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(ImageBlock { data_base64: base64_encode(&bytes), mime_type })
}

fn base64_encode(input: &[u8]) -> String {
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

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn all_tools() -> Value {
    let mut list = tools();
    if let Value::Array(items) = &mut list {
        items.extend(notebook::tools());
    }
    list
}

fn tools() -> Value {
    let project = json!({ "type": "string", "description": "Project name. Omit it to use the git repository of project_path (or of the folder the server runs in)." });
    let project_path =
        json!({ "type": "string", "description": "Absolute path of the project folder, when it isn't the folder the server runs in." });
    json!([
        {
            "name": "post_update",
            "title": "Post a project update",
            "description": format!(
                "Add an update to this project's feed in Feeder, the user's local project update centre. \
                 Write up to {MAX_PARAGRAPHS} short blocks (under {MAX_CHARS} characters total) covering: \
                 (1) Done:/Shipped:/Fixed: what was done + verification, (2) Why:/Benefit: why it was done \
                 and the benefit, and (3) Next:/Decision needed: what to do next and why. Markdown renders: \
                 **bold** sparingly for the key takeaway, `code` for files, branches and commands. Attach \
                 a screenshot or short clip (`screenshot_path`), or up to {MAX_MEDIA_PER_UPDATE} files in a carousel \
                 (`screenshots`) whenever the work changed multiple screens or states."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "The update in Markdown: up to 3 short blocks separated by blank lines, e.g. \"Done: …\\n\\nWhy: …\\n\\nNext: …\"." },
                    "project": project,
                    "project_path": project_path,
                    "agent": { "type": "string", "description": "Leave unset: Feeder detects the tool you're running in." },
                    "branch": { "type": "string", "description": "Optional git branch. Leave unset: Feeder detects it from project_path." },
                    "diff_stat": { "type": "string", "description": "Optional diff stat override (auto-detected from git when omitted)." },
                    "screenshot_path": { "type": "string", "description": "Optional path to one png, jpg, webp, gif screenshot or mp4/mov/webm clip. Copied into Feeder." },
                    "screenshots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": MAX_MEDIA_PER_UPDATE,
                        "description": format!("Optional list of up to {MAX_MEDIA_PER_UPDATE} screenshot/video paths to render as an interactive carousel on the update card.")
                    }
                },
                "required": ["summary"]
            }
        },
        {
            "name": "edit_update",
            "title": "Edit an existing update",
            "description": "Replace the text body of an existing update by id (returned by post_update, get_feed, or search_updates).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Update id." },
                    "summary": { "type": "string", "description": "Replacement update body in Markdown." }
                },
                "required": ["id", "summary"]
            }
        },
        {
            "name": "delete_update",
            "title": "Delete an update",
            "description": "Permanently delete a single update and its stored media by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Update id." }
                },
                "required": ["id"]
            },
            "annotations": { "destructiveHint": true }
        },
        {
            "name": "get_feed",
            "title": "Read a project's feed",
            "description": "Read this project's latest updates, pinned brief, to-dos and human replies. When starting a task, supply task keywords to also get a short list of relevant Notebook notes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "project_path": project_path,
                    "task": { "type": "string", "maxLength": 160, "description": "Optional search keywords for the task, such as 'appcast signing'. Returns up to three current notes from this project or the shared Notebook." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "How many updates (default 5)." },
                    "include_screenshots": { "type": "boolean", "description": format!("When true, include up to {MAX_INLINE_SCREENSHOTS} attached screenshots as base64 image blocks.") },
                    "mark_replies_read": { "type": "boolean", "description": "When true, mark returned unread human replies for this project as read." }
                }
            }
        },
        {
            "name": "get_recent_activity",
            "title": "Read recent cross-project updates",
            "description": "Read the latest updates across all active projects in Feeder, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "How many updates (default 10)." },
                    "include_screenshots": { "type": "boolean", "description": format!("When true, include up to {MAX_INLINE_SCREENSHOTS} attached screenshots as base64 image blocks.") }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "search_updates",
            "title": "Search Feeder updates",
            "description": "Search past update summaries, agent names, and branches across projects (or within one project).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text to match in update bodies, agent names, or branch names." },
                    "project": project,
                    "include_archived": { "type": "boolean", "description": "Set true to include archived projects." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum matches to return (default 10)." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "list_projects",
            "title": "List Feeder projects",
            "description": "List projects in Feeder with update counts, open to-do counts, active pulses, attention status, and last activity.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_archived": { "type": "boolean", "description": "Set true to include archived projects." }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "reply_to_update",
            "title": "Reply to an update card",
            "description": "Post an agent reply under an existing update card (for answering the human or continuing a thread).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Update id." },
                    "summary": { "type": "string", "description": "Reply text." },
                    "agent": { "type": "string", "description": "Optional agent display name override." }
                },
                "required": ["id", "summary"]
            }
        },
        {
            "name": "get_pending_decisions",
            "title": "Read unread human replies & open decisions",
            "description": "Fetch unread human replies and updates flagged `Decision needed:` or `Blocked:`, optionally filtered to one project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "project_path": project_path,
                    "mark_read": { "type": "boolean", "description": "When true, mark the returned unread human replies as read." }
                }
            }
        },
        {
            "name": "dismiss_attention",
            "title": "Dismiss an open Needs you flag",
            "description": "Clear an open `Needs you` badge on a specific update id or on the latest update for a project without posting a reply.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Specific update id to dismiss. Omit to dismiss the project's latest update." },
                    "project": project,
                    "project_path": project_path
                }
            },
            "annotations": { "idempotentHint": true }
        },
        {
            "name": "mark_replies_read",
            "title": "Mark human replies as read",
            "description": "Mark specific reply IDs (or all unread human replies for a project) as read after acting on them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "integer" }, "description": "Specific reply IDs to mark read." },
                    "project": project,
                    "project_path": project_path
                }
            }
        },
        {
            "name": "set_project_brief",
            "title": "Set or clear a project's pinned brief",
            "description": "Update the pinned project brief (`Goal / Focus / Commands`) displayed at the top of a project's feed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "brief": { "type": "string", "description": "Short brief text (up to 600 chars). Omit with clear=true to remove." },
                    "clear": { "type": "boolean", "description": "Set true to clear the pinned brief." },
                    "project": project,
                    "project_path": project_path
                }
            }
        },
        {
            "name": "pulse",
            "title": "Set or clear a live working pulse",
            "description": "Show a subtle live 'working' halo on this project's orb while working (auto-expires after ttl_seconds and auto-clears on post_update).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "description": "Short activity label, e.g. \"Refactoring auth\"." },
                    "project": project,
                    "project_path": project_path,
                    "agent": { "type": "string", "description": "Optional agent display name override." },
                    "branch": { "type": "string", "description": "Optional branch override." },
                    "ttl_seconds": { "type": "integer", "minimum": 5, "maximum": 3600, "description": "Expiry in seconds (default 120)." },
                    "clear": { "type": "boolean", "description": "Set true to clear the pulse immediately." }
                }
            }
        },
        {
            "name": "list_todos",
            "title": "List a project's to-dos",
            "description": "List optional checklist items for a project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "project_path": project_path,
                    "open_only": { "type": "boolean", "description": "When true, return only unchecked to-dos." }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "add_todo",
            "title": "Add a project to-do",
            "description": "Append a short to-do item (up to 240 chars) to a project's optional checklist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short to-do item text." },
                    "project": project,
                    "project_path": project_path
                },
                "required": ["title"]
            }
        },
        {
            "name": "toggle_todo",
            "title": "Complete or reopen a to-do",
            "description": "Mark a to-do item done or open by id. Omit `done` to toggle its current state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "To-do id." },
                    "done": { "type": "boolean", "description": "Explicit done state; omit to toggle." }
                },
                "required": ["id"]
            }
        },
        {
            "name": "delete_todo",
            "title": "Delete a to-do",
            "description": "Remove a to-do item from a project's checklist by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "To-do id." }
                },
                "required": ["id"]
            },
            "annotations": { "destructiveHint": true }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("feeder-mcp-test-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn mcp_roundtrip_tools_and_resources() {
        let store_dir = temp_dir("store");
        let work_dir = temp_dir("work");
        let shot = work_dir.join("shot.png");
        std::fs::write(&shot, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]).unwrap();

        let mut server = Server::new(None, work_dir.clone(), Some(store_dir.clone()));
        let init = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2025-11-25", "clientInfo": { "name": "codex" } }
            }))
            .unwrap();
        assert!(init.pointer("/result/capabilities/resources").is_some());

        let post = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "post_update",
                    "arguments": {
                        "project": "Alpha",
                        "summary": "Changed: first pass",
                        "branch": "feat/mcp",
                        "screenshot_path": shot
                    }
                }
            }))
            .unwrap();
        assert_eq!(post.pointer("/result/isError"), Some(&json!(false)));
        let uid = post.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();
        assert_eq!(post.pointer("/result/structuredContent/update/branch"), Some(&json!("feat/mcp")));

        let feed = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "get_feed",
                    "arguments": { "project": "Alpha", "include_screenshots": true }
                }
            }))
            .unwrap();
        let content = feed.pointer("/result/content").and_then(Value::as_array).unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/png");

        let list = server.handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" })).unwrap();
        let uris: Vec<&str> = list
            .pointer("/result/resources")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|r| r.get("uri").and_then(Value::as_str))
            .collect();
        assert!(uris.contains(&"feeder://projects"));
        assert!(uris.contains(&"feeder://project/alpha/feed"));

        let read = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "resources/read",
                "params": { "uri": "feeder://project/alpha/feed" }
            }))
            .unwrap();
        let md = read.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
        assert!(md.contains("feat/mcp"));
        assert!(md.contains("Changed: first pass"));

        let edited = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "edit_update",
                    "arguments": { "id": uid, "summary": "Changed: edited pass" }
                }
            }))
            .unwrap();
        assert_eq!(edited.pointer("/result/structuredContent/update/body"), Some(&json!("Changed: edited pass")));

        let deleted = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "delete_update",
                    "arguments": { "id": uid }
                }
            }))
            .unwrap();
        assert_eq!(deleted.pointer("/result/structuredContent/deleted"), Some(&json!(uid)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_replies_brief_and_pulse_roundtrip() {
        let store_dir = temp_dir("store2");
        let work_dir = temp_dir("work2");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let brief = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "set_project_brief",
                    "arguments": { "project": "Beta", "brief": "Goal: fast local UI\nFocus: replies" }
                }
            }))
            .unwrap();
        assert_eq!(brief.pointer("/result/structuredContent/project/brief"), Some(&json!("Goal: fast local UI\nFocus: replies")));

        let pulse = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "pulse",
                    "arguments": { "project": "Beta", "status": "Running clippy", "ttl_seconds": 90 }
                }
            }))
            .unwrap();
        assert_eq!(pulse.pointer("/result/structuredContent/pulse/status"), Some(&json!("Running clippy")));

        let post = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "post_update",
                    "arguments": { "project": "Beta", "summary": "Done: tests\n\nDecision needed: ship today?" }
                }
            }))
            .unwrap();
        let uid = post.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();

        let open_before = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 31,
                "method": "tools/call",
                "params": { "name": "get_pending_decisions", "arguments": { "project": "Beta" } }
            }))
            .unwrap();
        assert_eq!(open_before.pointer("/result/structuredContent/needs_you_updates/0/id"), Some(&json!(uid)));

        let store = Store::open(&store_dir).unwrap();
        let human_reply = store.add_reply(uid, "You", true, "Yes, ship today!").unwrap();

        let pending = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "get_pending_decisions", "arguments": { "project": "Beta" } }
            }))
            .unwrap();
        assert_eq!(pending.pointer("/result/structuredContent/unread_replies/0/body"), Some(&json!("Yes, ship today!")));
        assert_eq!(pending.pointer("/result/structuredContent/needs_you_updates").and_then(Value::as_array).map(Vec::len), Some(0));

        let marked = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "mark_replies_read", "arguments": { "ids": [human_reply.id] } }
            }))
            .unwrap();
        assert_eq!(marked.pointer("/result/isError"), Some(&json!(false)));

        let reply = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": { "name": "reply_to_update", "arguments": { "id": uid, "summary": "Shipping now.", "agent": "Codex" } }
            }))
            .unwrap();
        assert_eq!(reply.pointer("/result/structuredContent/reply/author"), Some(&json!("Codex")));
        assert_eq!(reply.pointer("/result/structuredContent/reply/from_human"), Some(&json!(false)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_todos_roundtrip_and_feed_integration() {
        let store_dir = temp_dir("store-todos");
        let work_dir = temp_dir("work-todos");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let added1 = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "add_todo", "arguments": { "project": "Gamma", "title": "Ship release" } }
            }))
            .unwrap();
        assert_eq!(added1.pointer("/result/isError"), Some(&json!(false)));
        let tid1 = added1.pointer("/result/structuredContent/todo/id").and_then(Value::as_i64).unwrap();

        let added2 = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "add_todo", "arguments": { "project": "Gamma", "title": "Write docs" } }
            }))
            .unwrap();
        let tid2 = added2.pointer("/result/structuredContent/todo/id").and_then(Value::as_i64).unwrap();

        let toggled = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "toggle_todo", "arguments": { "id": tid1 } }
            }))
            .unwrap();
        assert_eq!(toggled.pointer("/result/structuredContent/todo/done"), Some(&json!(true)));

        let open_list = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "list_todos", "arguments": { "project": "Gamma", "open_only": true } }
            }))
            .unwrap();
        let open_arr = open_list.pointer("/result/structuredContent/todos").and_then(Value::as_array).unwrap();
        assert_eq!(open_arr.len(), 1);
        assert_eq!(open_arr[0]["id"], json!(tid2));

        let projects = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "list_projects", "arguments": {} }
            }))
            .unwrap();
        assert_eq!(projects.pointer("/result/structuredContent/projects/0/open_todos"), Some(&json!(1)));

        let feed = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": { "name": "get_feed", "arguments": { "project": "Gamma" } }
            }))
            .unwrap();
        let feed_text = feed.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
        assert!(feed_text.contains("To-dos (1 open / 2 total):"));
        assert!(feed_text.contains("[x] Ship release"));
        assert!(feed_text.contains("[ ] Write docs"));

        let res = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "resources/read",
                "params": { "uri": "feeder://project/gamma/feed" }
            }))
            .unwrap();
        let md = res.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
        assert!(md.contains("## To-dos"));
        assert!(md.contains("- [ ] Write docs"));

        let deleted = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "tools/call",
                "params": { "name": "delete_todo", "arguments": { "id": tid1 } }
            }))
            .unwrap();
        assert_eq!(deleted.pointer("/result/structuredContent/deleted"), Some(&json!(tid1)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_dismiss_attention_and_notebook_resources() {
        let store_dir = temp_dir("store-dismiss-nb");
        let work_dir = temp_dir("work-dismiss-nb");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let posted = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "post_update",
                    "arguments": { "project": "Delta", "summary": "Done: built API.\n\nDecision needed: keep v1?" }
                }
            }))
            .unwrap();
        let uid = posted.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();

        let feed_before = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "get_feed", "arguments": { "project": "Delta" } }
            }))
            .unwrap();
        assert_eq!(feed_before.pointer("/result/structuredContent/needs_you"), Some(&json!(true)));

        let dismissed = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "dismiss_attention", "arguments": { "id": uid } }
            }))
            .unwrap();
        assert_eq!(dismissed.pointer("/result/isError"), Some(&json!(false)));

        let feed_after = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "get_feed", "arguments": { "project": "Delta" } }
            }))
            .unwrap();
        assert_eq!(feed_after.pointer("/result/structuredContent/needs_you"), Some(&json!(false)));

        let pid = Store::open(&store_dir)
            .unwrap()
            .create_page(
                feeder_core::PageDraft::new(
                    "SQLite WAL checkpoints need busy timeout",
                    "Set PRAGMA busy_timeout = 3000 on every connection.",
                ),
                feeder_core::HUMAN_AUTHOR,
                None,
                feeder_core::OnSimilar::Refuse,
            )
            .unwrap()
            .id;

        let index = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "resources/read",
                "params": { "uri": "feeder://notebook" }
            }))
            .unwrap();
        let index_md = index.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
        assert!(index_md.contains("SQLite WAL checkpoints need busy timeout"));

        let page_res = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "resources/read",
                "params": { "uri": format!("feeder://notebook/{pid}") }
            }))
            .unwrap();
        let page_md = page_res.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
        assert!(page_md.contains("PRAGMA busy_timeout = 3000"));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_carousel_and_freeform_notebook_roundtrip() {
        let store_dir = temp_dir("store-carousel-md");
        let work_dir = temp_dir("work-carousel-md");
        let shot1 = work_dir.join("a.png");
        let shot2 = work_dir.join("b.png");
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        std::fs::write(&shot1, png).unwrap();
        std::fs::write(&shot2, png).unwrap();

        let mut server = Server::new(Some("claude-code".into()), work_dir.clone(), Some(store_dir.clone()));

        let posted = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "post_update",
                    "arguments": {
                        "project": "Orbit",
                        "summary": "Done: redesigned onboarding flow and settings drawer.",
                        "screenshots": [shot1, shot2]
                    }
                }
            }))
            .unwrap();
        assert_eq!(posted.pointer("/result/isError"), Some(&json!(false)));
        let media = posted.pointer("/result/structuredContent/update/media").and_then(Value::as_array).unwrap();
        assert_eq!(media.len(), 2);

        let feed = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "get_feed",
                    "arguments": { "project": "Orbit", "include_screenshots": true }
                }
            }))
            .unwrap();
        let content = feed.pointer("/result/content").and_then(Value::as_array).unwrap();
        assert_eq!(content.len(), 3);

        let draft = feeder_core::PageDraft {
            topic: Some("Rust".into()),
            ..feeder_core::PageDraft::new(
                "SQLite multi-image cascade rules",
                "## Pattern\n\nAlways copy files before opening the transaction.\n\n- [ ] Verify rollback cleanup",
            )
        };
        let pid = Store::open(&store_dir)
            .unwrap()
            .create_page(draft, feeder_core::HUMAN_AUTHOR, None, feeder_core::OnSimilar::Refuse)
            .unwrap()
            .id;

        let page = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "read_page", "arguments": { "id": pid } }
            }))
            .unwrap();
        assert_eq!(page.pointer("/result/structuredContent/topic"), Some(&json!("rust")));
        assert!(
            page.pointer("/result/structuredContent/body").and_then(Value::as_str).is_some_and(|b| b.contains("Verify rollback cleanup"))
        );

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn a_feed_handoff_suggests_only_current_project_and_shared_notes() {
        use feeder_core::{HUMAN_AUTHOR, OnSimilar, PageDraft, Standing};

        let dir = temp_dir("task-notes");
        let store = Store::open(&dir).unwrap();
        let alpha = store.project_named("Alpha", None).unwrap();
        let beta = store.project_named("Beta", None).unwrap();
        let page = |title, project_id| {
            store.create_page(PageDraft::new(title, "Use the signing checklist."), HUMAN_AUTHOR, project_id, OnSimilar::Allow).unwrap()
        };
        let local = page("Signing Alpha", Some(alpha.id));
        let shared = page("Signing checklist", None);
        page("Signing Beta", Some(beta.id));
        let archived = page("Signing old builds", Some(alpha.id));
        store.set_page_standing(archived.id, Standing::Archived).unwrap();
        let server = Server::new(None, dir.clone(), Some(dir.clone()));
        let handoff = |project, task| server.call(&json!({ "name": "get_feed", "arguments": { "project": project, "task": task } }));
        let result = handoff("Alpha", Some("signing"));
        let notes = result.pointer("/structuredContent/related_notes").unwrap().as_array().unwrap();
        assert_eq!(notes.len(), 2);
        for expected in [local.id, shared.id] {
            assert!(notes.iter().any(|note| note["url"] == format!("feeder://notebook/{expected}")));
        }
        assert!(!result.to_string().contains("Signing Beta"));
        assert!(!result.to_string().contains("Signing old builds"));
        for query in ["--", "no match"] {
            assert_eq!(handoff("Alpha", Some(query)).pointer("/structuredContent/related_notes"), Some(&json!([])));
        }
        assert!(store.flags(None, 10).unwrap().is_empty(), "suggestions don't file flags");
        assert!(handoff("Alpha", None).pointer("/structuredContent/related_notes").is_none());
        assert_eq!(handoff("Unknown", Some("signing")).pointer("/structuredContent/related_notes"), Some(&json!([])));
        drop(server);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn everyday_agents_can_read_and_flag_but_cannot_edit_notebook_pages() {
        use feeder_core::{HUMAN_AUTHOR, OnSimilar, PageDraft, Standing};

        let store_dir = temp_dir("store-agent-notebook");
        let store = Store::open(&store_dir).unwrap();
        let page = store
            .create_page(PageDraft::new("Release checklist", "Run the release checks."), HUMAN_AUTHOR, None, OnSimilar::Refuse)
            .unwrap();
        let mut server = Server::new(Some("Codex".into()), store_dir.clone(), Some(store_dir.clone()));
        let listed = server.handle(&json!({ "id": 1, "method": "tools/list" })).unwrap();
        let project_tools = tools();
        let project_tools = project_tools.as_array().unwrap();
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tool| !project_tools.contains(tool))
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        let call = |name, arguments| server.call(&json!({ "name": name, "arguments": arguments }));

        let read = call("read_page", json!({ "id": page.id }));
        let helped = call("mark_page_helped", json!({ "id": page.id }));
        let write = call("write_page", json!({ "id": page.id, "title": page.title, "body": "Overwrite this note." }));
        let archive = call("archive_page", json!({ "id": page.id }));
        let current = store.page(page.id).unwrap().unwrap();
        drop(server);
        drop(store);
        std::fs::remove_dir_all(store_dir).unwrap();

        assert_eq!(names, ["search_notebook", "read_page", "mark_page_helped", "flag_knowledge"]);
        assert_eq!(read.pointer("/structuredContent/body"), Some(&json!(page.body)));
        assert_eq!(helped["isError"], false);
        assert_eq!(current.helped, page.helped + 1);
        assert_eq!(write["isError"], true);
        assert_eq!(archive["isError"], true);
        assert_eq!(current.standing, Standing::Current);
        assert_eq!(current.body, page.body);
        assert_eq!(current.revision, page.revision);
    }

    #[test]
    fn empty_searches_and_flag_knowledge_file_flags_for_the_gardener() {
        let store_dir = temp_dir("store-flags");
        let work_dir = temp_dir("work-flags");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));
        let call = |server: &mut Server, id: i64, name: &str, arguments: Value| {
            server
                .handle(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }))
                .unwrap()
        };

        for id in 1..=2 {
            let miss = call(&mut server, id, "search_notebook", json!({ "query": "notarize the dmg" }));
            let text = miss.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
            assert!(text.contains("flag_knowledge"), "the empty result asks for a flag: {text}");
        }
        let flagged = call(
            &mut server,
            3,
            "flag_knowledge",
            json!({ "kind": "wrong", "note": "The release page says release.sh is in the root", "suggestion": "It moved to scripts/" }),
        );
        assert_eq!(flagged.pointer("/result/structuredContent/kind"), Some(&json!("wrong")));
        let no_suggestion = call(&mut server, 4, "flag_knowledge", json!({ "note": "No instructions for rotating the appcast key" }));
        assert_eq!(no_suggestion.pointer("/result/isError"), Some(&json!(false)));

        let store = Store::open(&store_dir).unwrap();
        let open = store.flags(Some(feeder_core::FlagStatus::Open), 10).unwrap();
        assert_eq!(open.len(), 3, "two explicit flags and one from the repeated empty search");
        assert!(open.iter().any(|f| f.note.contains("notarize the dmg") && f.kind == feeder_core::FlagKind::Missing));
        assert!(open.iter().any(|f| f.suggestion.as_deref() == Some("It moved to scripts/")));

        let refused = call(&mut server, 5, "flag_knowledge", json!({ "note": "no" }));
        assert_eq!(refused.pointer("/result/isError"), Some(&json!(true)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn the_gardener_role_offers_only_guarded_tools_and_caps_its_edits() {
        let store_dir = temp_dir("store-gardener");
        let work_dir = temp_dir("work-gardener");
        let mut server = Server::new(Some("claude-code".into()), work_dir.clone(), Some(store_dir.clone()));
        let run = Store::open(&store_dir).unwrap().start_gardener_run(feeder_core::gardener::Backend::ClaudeCode).unwrap();
        server.role = Role::Gardener(run);
        let call = |server: &mut Server, id: i64, name: &str, arguments: Value| {
            server
                .handle(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }))
                .unwrap()
        };

        let listed = server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).unwrap();
        let names: Vec<&str> =
            listed.pointer("/result/tools").and_then(Value::as_array).unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, gardener::tool_names(feeder_core::gardener::SessionMode::Review), "only the gardener's tools");
        let post = call(&mut server, 2, "post_update", json!({ "project": "Alpha", "summary": "Done: x" }));
        assert_eq!(post.pointer("/result/isError"), Some(&json!(true)), "feed tools are out of reach");

        let flag = {
            let store = Store::open(&store_dir).unwrap();
            let new = feeder_core::NewFlag {
                kind: feeder_core::FlagKind::Missing,
                page_id: None,
                project_id: None,
                note: "Nothing on release checks".into(),
                suggestion: Some("Run scripts/changelog.sh --check".into()),
            };
            store.flag_knowledge(new, "Codex").unwrap().id
        };
        let unsourced = call(
            &mut server,
            3,
            "write_page",
            json!({ "title": "Release checks", "body": "Run the changelog check.", "reason": "Useful" }),
        );
        assert_eq!(unsourced.pointer("/result/isError"), Some(&json!(true)));
        let sourced = call(
            &mut server,
            4,
            "write_page",
            json!({ "title": "Release checks", "body": "Run `scripts/changelog.sh --check`.", "reason": "Flag asked for it", "flags": [flag] }),
        );
        assert_eq!(sourced.pointer("/result/isError"), Some(&json!(false)), "{sourced}");
        assert_eq!(sourced.pointer("/result/structuredContent/page/agent"), Some(&json!("Gardener")));

        let store = Store::open(&store_dir).unwrap();
        let titles = ["Signing builds", "Rotating logs", "Caching fonts", "Pinning toolchains"];
        for (n, title) in (10_i64..).zip(titles) {
            let f = store
                .flag_knowledge(
                    feeder_core::NewFlag {
                        kind: feeder_core::FlagKind::Missing,
                        page_id: None,
                        project_id: None,
                        note: format!("Gap about {title}"),
                        suggestion: None,
                    },
                    "Codex",
                )
                .unwrap()
                .id;
            let ok =
                call(&mut server, n, "write_page", json!({ "title": title, "body": "Do the step.", "reason": "Flagged", "flags": [f] }));
            assert_eq!(ok.pointer("/result/isError"), Some(&json!(false)), "{ok}");
        }
        let f = store
            .flag_knowledge(
                feeder_core::NewFlag {
                    kind: feeder_core::FlagKind::Missing,
                    page_id: None,
                    project_id: None,
                    note: "One more gap".into(),
                    suggestion: None,
                },
                "Codex",
            )
            .unwrap()
            .id;
        let over =
            call(&mut server, 20, "write_page", json!({ "title": "One more page", "body": "Body.", "reason": "Flagged", "flags": [f] }));
        assert_eq!(over.pointer("/result/isError"), Some(&json!(true)), "the sixth change is refused");

        let missing = call(&mut server, 21, "search_notebook", json!({ "query": "zzz nothing" }));
        assert_eq!(missing.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(
            store.flags(Some(feeder_core::FlagStatus::Open), 50).unwrap().len(),
            1,
            "the gardener's empty searches don't file flags"
        );

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn asking_curator_only_reads_notebook_pages_in_scope() {
        use feeder_core::gardener::{Backend, Scope, SessionMode};

        let store_dir = temp_dir("store-curator-ask");
        let work_dir = temp_dir("work-curator-ask");
        let store = Store::open(&store_dir).expect("open scratch store");
        let alpha = store.project_named("Alpha", None).expect("add Alpha");
        let beta = store.project_named("Beta", None).expect("add Beta");
        let write = |project: &Project| {
            store
                .create_page(
                    feeder_core::PageDraft::new(format!("{} deployment", project.name), "Use the deployment checklist."),
                    feeder_core::HUMAN_AUTHOR,
                    Some(project.id),
                    feeder_core::OnSimilar::Refuse,
                )
                .expect("write a scratch page")
                .id
        };
        let own_id = write(&alpha);
        let other_id = write(&beta);
        let shared_id = store
            .create_page(
                feeder_core::PageDraft::new("Shared deployment preferences", "Prefer a short manual smoke test."),
                feeder_core::HUMAN_AUTHOR,
                None,
                feeder_core::OnSimilar::Allow,
            )
            .unwrap()
            .id;
        let run = store.start_curator_run(Backend::Codex, Scope::Project(alpha.id), SessionMode::Ask).expect("start an Ask session");
        let mut server = Server::new(None, work_dir.clone(), Some(store_dir.clone()));
        server.role = Role::Gardener(run);
        let request = |server: &mut Server, method: &str, params: Value| {
            server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).expect("receive a reply")
        };
        let listed = request(&mut server, "tools/list", json!({}));
        let names: Vec<&str> =
            listed.pointer("/result/tools").and_then(Value::as_array).unwrap().iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(names, ["search_notebook", "read_page"]);
        let call = |name: &str, arguments: Value| server.call(&json!({ "name": name, "arguments": arguments }));
        for query in ["deployment", ""] {
            let result = call("search_notebook", json!({ "query": query }));
            let pages = result.pointer("/structuredContent/pages").unwrap().as_array().unwrap();
            assert_eq!(pages.len(), 2);
            assert!(pages.iter().any(|page| page["id"] == own_id));
            assert!(pages.iter().any(|page| page["id"] == shared_id));
        }
        assert_eq!(call("read_page", json!({ "id": own_id }))["isError"], false);
        assert_eq!(call("read_page", json!({ "id": shared_id }))["isError"], false);
        assert_eq!(call("read_page", json!({ "id": other_id }))["isError"], true);
        for tool in ["read_traces", "list_flags", "write_page", "archive_page", "dismiss_flag"] {
            let result = call(tool, json!({ "id": own_id }));
            assert_eq!(result["isError"], true, "{tool} was not refused");
            assert!(result.to_string().contains("tool unavailable in this Curator session"), "{result}");
        }
        let resource = request(&mut server, "resources/read", json!({ "uri": format!("feeder://notebook/{other_id}") }));
        assert_eq!(resource.pointer("/error/code"), Some(&json!(-32601)));
        assert_eq!(store.gardener_run(run).expect("read run").expect("run exists").edits, 0);

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn a_project_gardener_turns_a_thread_correction_into_a_revertible_note() {
        use feeder_core::gardener::{Backend, Scope};

        let store_dir = temp_dir("store-thread");
        let work_dir = temp_dir("work-thread");
        let store = Store::open(&store_dir).expect("open a scratch store");
        let alpha = store.project_named("Alpha", None).expect("add Alpha");
        let beta = store.project_named("Beta", None).expect("add Beta");
        let post = |project: &Project, body: &str| {
            store
                .post(project.id, NewUpdate { agent: "Codex".into(), body: body.into(), ..NewUpdate::default() })
                .expect("post a scratch update")
        };
        let update = post(&alpha, "Done: Added an appcast check.");
        let unrelated = post(&beta, "Done: Drafted a database guide.");
        let correction = "Run the appcast check after generating the feed.";
        store.add_reply(update.id, "You", true, correction).expect("correct the agent");
        let flag = store.flag_update_for_notebook(update.id, "You").expect("flag the thread");
        assert_eq!(flag.id, store.flag_update_for_notebook(update.id, "You").expect("flag again").id);
        let other_flag = store.flag_update_for_notebook(unrelated.id, "You").expect("flag a different project");

        let run = store.start_scoped_gardener_run(Backend::Codex, Scope::Project(alpha.id)).expect("start scoped session");
        let mut gardener = Server::new(None, work_dir.clone(), Some(store_dir.clone()));
        gardener.role = Role::Gardener(run);
        let call = |server: &Server, name: &str, arguments: Value| server.call(&json!({ "name": name, "arguments": arguments }));

        let flags = call(&gardener, "list_flags", json!({}));
        assert_eq!(flags.pointer("/structuredContent/flags/0/source_update_id"), Some(&json!(update.id)));
        assert!(flags.pointer("/structuredContent/flags/1").is_none());
        let traces = call(&gardener, "read_traces", json!({}));
        assert_eq!(traces.pointer("/structuredContent/traces/0/replies/0/body"), Some(&json!(correction)));
        assert!(traces.pointer("/structuredContent/traces/1").is_none());
        let other_trace = call(&gardener, "read_traces", json!({ "update_id": unrelated.id }));
        assert_eq!(other_trace["isError"], true);
        let refused = call(&gardener, "dismiss_flag", json!({ "id": other_flag.id, "reason": "Wrong scope" }));
        assert_eq!(refused["isError"], true);

        let title = "When to run the appcast check";
        let write = |flags: Vec<i64>, updates: Vec<i64>| {
            call(
                &gardener,
                "write_page",
                json!({
                    "title": title,
                    "body": correction,
                    "reason": "The person corrected the order in the Feed thread.",
                    "flags": flags,
                    "updates": updates,
                }),
            )
        };
        assert_eq!(write(vec![other_flag.id], vec![update.id])["isError"], true);
        assert_eq!(write(vec![flag.id], vec![unrelated.id])["isError"], true);
        let saved = write(vec![flag.id], vec![]);
        assert_eq!(saved["isError"], false, "{saved}");
        assert_eq!(saved.pointer("/structuredContent/source_updates/0"), Some(&json!(update.id)));
        let change_id = saved.pointer("/structuredContent/change_id").and_then(Value::as_i64).expect("change is returned");
        let page_id = saved.pointer("/structuredContent/page/id").and_then(Value::as_i64).expect("page is returned");
        assert_eq!(store.page(page_id).expect("load the page").expect("the page exists").project_id, Some(alpha.id));

        let worker = Server::new(Some("claude-code".into()), work_dir.clone(), Some(store_dir.clone()));
        let lookup = call(&worker, "search_notebook", json!({ "query": "appcast check" }));
        assert_eq!(lookup.pointer("/structuredContent/pages/0/title"), Some(&json!(title)));
        assert_eq!(store.changes_from_update(update.id).expect("read linked changes")[0].id, change_id);
        assert_eq!(store.gardener_run_changes(run).expect("read session changes")[0].id, change_id);
        store.finish_gardener_run(run, None, None).expect("finish the session");
        assert_eq!(store.gardener_run(run).expect("read the run").expect("run exists").edits, 1);
        assert_eq!(write(vec![flag.id], vec![])["isError"], true, "an ended session cannot keep writing");

        assert!(store.revert_change(change_id, "You").expect("revert from session history").page.is_archived());
        assert_eq!(store.flag(flag.id).expect("read the flag").expect("flag still exists").status, feeder_core::FlagStatus::Open);
        let change = &store.changes_from_update(update.id).expect("retain attribution after Revert")[0];
        assert!(change.is_reverted());
        assert_eq!(change.source_flags, vec![flag.id]);
        let lookup = call(&worker, "search_notebook", json!({ "query": "appcast check" }));
        assert!(lookup.pointer("/structuredContent/pages/0").is_none());

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn the_edit_cap_spans_every_server_in_one_session() {
        let store_dir = temp_dir("store-cap");
        let work_dir = temp_dir("work-cap");
        let store = Store::open(&store_dir).unwrap();
        let run = store.start_gardener_run(feeder_core::gardener::Backend::Codex).unwrap();
        let titles = ["Signing builds", "Rotating logs", "Caching fonts", "Pinning toolchains", "Tagging releases", "Pruning branches"];
        let mut refused = 0;
        for (turn, pair) in titles.chunks(2).enumerate() {
            let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));
            server.role = Role::Gardener(run);
            for (n, title) in pair.iter().enumerate() {
                let new = feeder_core::NewFlag {
                    kind: feeder_core::FlagKind::Missing,
                    page_id: None,
                    project_id: None,
                    note: format!("Gap about {title}"),
                    suggestion: None,
                };
                let flag = store.flag_knowledge(new, "Codex").unwrap().id;
                let id = i64::try_from(turn * 2 + n).unwrap();
                let out = server.handle(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "write_page", "arguments": { "title": title, "body": "Do it.", "reason": "Flagged", "flags": [flag] } } })).unwrap();
                if out.pointer("/result/isError") == Some(&json!(true)) {
                    refused += 1;
                }
            }
        }
        assert_eq!(refused, 1, "five changes per session, however many servers it starts");
        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
