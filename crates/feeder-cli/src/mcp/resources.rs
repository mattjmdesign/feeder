use feeder_core::{GUIDE, MAX_CHARS, NOTEBOOK_GUIDE, PageFilter, ago, now_ms};
use serde_json::{Value, json};

use super::Server;
use super::protocol::rpc_error;
use crate::project_json;

impl Server {
    pub(super) fn resources_list(&self) -> Result<Value, String> {
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
                "uri": "feeder://pending",
                "name": "Pending decisions and unread replies",
                "description": "Unread human replies and open Decision needed / Blocked updates across active projects (Markdown).",
                "mimeType": "text/markdown"
            }),
            json!({
                "uri": "feeder://library",
                "name": "Library",
                "description": "Shared index of current Feeder Library lessons across all projects (Markdown).",
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

    pub(super) fn resource_templates_list(&self) -> Value {
        json!({
            "resourceTemplates": [{
                "uriTemplate": "feeder://project/{slug}/feed",
                "name": "Project feed",
                "description": "Latest updates for a Feeder project in Markdown.",
                "mimeType": "text/markdown"
            }, {
                "uriTemplate": "feeder://library/{id}",
                "name": "Library note",
                "description": "A single Feeder Library lesson in Markdown.",
                "mimeType": "text/markdown"
            }]
        })
    }

    pub(super) fn resources_read(&self, params: &Value) -> Result<Value, (i64, String)> {
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
        if uri == "feeder://pending" {
            let now = now_ms();
            let unread = store.unread_human_replies(None).map_err(|e| (-32603, e.to_string()))?;
            let summaries = store.projects().map_err(|e| (-32603, e.to_string()))?;
            let names: std::collections::HashMap<i64, String> = summaries.iter().map(|s| (s.project.id, s.project.name.clone())).collect();
            let needs_you: Vec<_> = summaries.into_iter().filter(|s| !s.project.archived && s.needs_you).collect();
            let mut md = String::from("# Pending decisions and unread replies\n");
            if unread.is_empty() && needs_you.is_empty() {
                md.push_str("\nNo pending decisions or unread human replies.\n");
            } else {
                if !unread.is_empty() {
                    md.push_str(&format!("\n## Unread human replies ({})\n", unread.len()));
                    for r in &unread {
                        let pname = names.get(&r.project_id).map(String::as_str).unwrap_or("Unknown");
                        md.push_str(&format!(
                            "\n- reply #{} ({pname}, update #{}, {}): {}\n",
                            r.id,
                            r.update_id,
                            ago(r.created_at, now),
                            r.body
                        ));
                    }
                }
                if !needs_you.is_empty() {
                    md.push_str(&format!("\n## Open Decision / Blocked cards ({})\n", needs_you.len()));
                    for s in &needs_you {
                        let uid = s.last_update_id.unwrap_or(0);
                        let when = s.last_at.map(|t| ago(t, now)).unwrap_or_else(|| "recently".into());
                        let body = s.last_body.as_deref().unwrap_or("");
                        md.push_str(&format!("\n- {} #{uid} ({when}): {body}\n", s.project.name));
                    }
                }
            }
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": md }] }));
        }
        if uri == "feeder://library" || uri == "feeder://notebook" {
            let pages = store.pages(&PageFilter::default()).map_err(|e| (-32603, e.to_string()))?;
            let mut md = String::from("# Feeder Library\n");
            if pages.is_empty() {
                md.push_str("\nNo Library notes yet.\n");
            } else {
                for p in &pages {
                    md.push_str(&format!("\n- {}\n", p.summary_line()));
                }
            }
            return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": md }] }));
        }
        if let Some(id_str) = uri.strip_prefix("feeder://library/").or_else(|| uri.strip_prefix("feeder://notebook/")) {
            let id = id_str.parse::<i64>().map_err(|_| (-32002, format!("resource not found: {uri}")))?;
            let page = store.page(id).map_err(|e| (-32603, e.to_string()))?.ok_or((-32002, format!("Library note not found: {id}")))?;
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

    pub(super) fn prompts_list(&self) -> Value {
        json!({
            "prompts": [{
                "name": "update",
                "title": "Post a Feeder update",
                "description": "Write a 3-block Feed handoff (Done / Why / Next, under 900 chars) and post it with post_update."
            }, {
                "name": "lesson",
                "title": "Flag a lesson draft for Curator",
                "description": "Search the Library with search_library, then submit a full Markdown lesson draft via flag_knowledge."
            }]
        })
    }

    pub(super) fn prompts_get(&self, id: Value, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str);
        match name {
            Some("update") => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "description": "Write a short 3-block Feed update and call post_update.",
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Write a Feeder update in up to 3 short Markdown blocks separated by blank lines (under {MAX_CHARS} chars total):\n\
                                 1. `Done:`/`Shipped:`/`Fixed:` concrete outcome and proof.\n\
                                 2. `Why:`/`Benefit:` problem solved or what it unlocks.\n\
                                 3. `Next:` (or `Decision needed:` / `Blocked:`) next step and why.\n\n\
                                 Check `git status` or `git log -n 5` if useful, attach real media paths in `media` when visible, and call `post_update` (you can include `mark_replies_read`, `complete_todos`, `add_todos`, `helped_notes`, and `flag_lesson` in the same call).\n\n{GUIDE}"
                            )
                        }
                    }]
                }
            }),
            Some("lesson") => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "description": "Search the Library first, then file a lesson draft for Curator via flag_knowledge.",
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Review this session for a reusable bug fix, pitfall, pattern, decision, or user correction. \
                                 Call `search_library` (and `read_library` on hits) first. If anything is missing or outdated, \
                                 call `flag_knowledge` with a full Markdown lesson draft in `suggestion` (symptom/mistake, root cause, verified fix or rule). Never include secrets.\n\n{NOTEBOOK_GUIDE}"
                            )
                        }
                    }]
                }
            }),
            _ => rpc_error(id, -32602, "unknown prompt; use `update` or `lesson`"),
        }
    }
}
