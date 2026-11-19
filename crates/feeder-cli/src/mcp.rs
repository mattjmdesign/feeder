//! A Model Context Protocol server on stdio: newline-delimited JSON-RPC 2.0.
//!
//! The project defaults to the git repository of the folder the server was started in, which is
//! the agent's project folder for stdio servers. The agent defaults to the client's own name from
//! `initialize` (`claude-code` shows as "Claude Code").

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

mod notebook;

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_INLINE_SCREENSHOTS: usize = 6;
const MAX_INLINE_SCREENSHOT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RPC_LINE_BYTES: usize = 16 * 1024 * 1024;

struct Server {
    client: Option<String>,
    cwd: PathBuf,
    store_dir: Option<PathBuf>,
    store: RefCell<Option<Store>>,
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

pub fn serve() -> Result<(), String> {
    let mut server = Server::new(None, std::env::current_dir().map_err(|e| e.to_string())?, None);
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
        Self { client, cwd, store_dir, store: RefCell::new(None) }
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

    /// The reply to one message, or None for a notification.
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?; // notifications (initialized, cancelled) need no answer
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": all_tools() })),
            "tools/call" => Ok(self.call(&params)),
            "resources/list" => self.resources_list().map_err(|e| (-32603, e)),
            "resources/templates/list" => Ok(json!({
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
            "resources/read" => self.resources_read(&params),
            "prompts/list" => Ok(json!({ "prompts": [{
                "name": "update",
                "title": "Post a Feeder update",
                "description": "Write this project's update the Feeder way and post it."
            }, {
                "name": "lesson",
                "title": "Save a lesson to the notebook",
                "description": "Turn what was learned this session into a notebook page, if anything is worth keeping."
            }] })),
            "prompts/get" => match params.get("name").and_then(Value::as_str) {
                Some("update") => Ok(json!({
                    "description": "Write this project's update the Feeder way and post it.",
                    "messages": [{ "role": "user", "content": { "type": "text", "text": format!(
                        "{GUIDE}\n\nNow look at what changed in this project during this session (and `git log`/`git status` if useful), decide whether a screenshot or clip helps, then call post_update."
                    ) } }]
                })),
                Some("lesson") => Ok(json!({
                    "description": "Turn what was learned this session into a notebook page, if anything is worth keeping.",
                    "messages": [{ "role": "user", "content": { "type": "text", "text": format!(
                        "{NOTEBOOK_GUIDE}\n\nLook back over this session. If it produced a lesson that clears the bar above (a non-obvious fix, a dead end, a reusable pattern or a settled decision), search the notebook for it first, then write a new page or revise the existing one. If nothing clears the bar, say so and write nothing."
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

    fn initialize(&mut self, params: &Value) -> Value {
        self.client = params.pointer("/clientInfo/name").and_then(Value::as_str).map(str::to_string);
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSIONS[1]);
        let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[1] };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {}, "prompts": {}, "resources": {} },
            "serverInfo": { "name": "feeder", "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!("Feeder is the user's local project update centre (this server runs on their Mac; nothing leaves it). Post an update with post_update when you finish a meaningful piece of work or when asked, and call get_feed before picking a project back up. It also keeps a shared notebook of lessons across all projects: search it (search_notebook) before non-trivial debugging, and write a page (write_page) when you learn something that would save time in another project.\n\n{GUIDE}\n\n{NOTEBOOK_GUIDE}")
        })
    }

    fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let outcome = match name {
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
        };
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
        let store = self.open_store()?;
        let Some(project) = self.project(&store, args, false)? else {
            let name = detect_project(&self.base(args)).name;
            return Ok(ToolOutput::text(
                format!("{name} has no updates yet."),
                json!({ "project": null, "todos": [], "updates": [], "unread_replies": [] }),
            ));
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
        Ok(ToolOutput {
            text,
            data: json!({
                "project": project_json(&project),
                "needs_you": needs_you,
                "todos": todos_json,
                "updates": updates,
                "unread_replies": unread_json,
            }),
            images,
        })
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

    /// The project the call names, else the one at `project_path`, else the server's folder.
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

/// Every tool the server offers: the feed's, then the notebook's.
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
            "description": "Read the latest updates, pinned brief, optional to-dos, open attention status, and any unread human replies in this project's Feeder feed, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "project_path": project_path,
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

        // Simulate human reply through the store.
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

        let wrote = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "write_page",
                    "arguments": {
                        "kind": "fix",
                        "topic": "data",
                        "title": "SQLite WAL checkpoints need busy timeout",
                        "tags": ["sqlite", "locks"],
                        "what_happened": "Concurrent reads threw SQLITE_BUSY under write bursts.",
                        "why": "Default busy_timeout was zero on the reader connection.",
                        "do_this": "Set PRAGMA busy_timeout = 3000 on every connection."
                    }
                }
            }))
            .unwrap();
        assert_eq!(wrote.pointer("/result/isError"), Some(&json!(false)));
        let pid = wrote.pointer("/result/structuredContent/id").and_then(Value::as_i64).unwrap();

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
        assert_eq!(content.len(), 3); // 1 text + 2 inline images

        let wrote = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "write_page",
                    "arguments": {
                        "topic": "Rust",
                        "title": "SQLite multi-image cascade rules",
                        "body": "## Pattern\n\nAlways copy files before opening the transaction.\n\n- [ ] Verify rollback cleanup"
                    }
                }
            }))
            .unwrap();
        assert_eq!(wrote.pointer("/result/isError"), Some(&json!(false)));
        let pid = wrote.pointer("/result/structuredContent/id").and_then(Value::as_i64).unwrap();

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
}
