//! Lightweight local HTTP companion (`feeder serve`) for reading feeds, briefs,
//! screenshots/clips, and submitting human replies from a phone or another device over Tailscale.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

use feeder_core::{FeedUpdate, Project, ProjectSummary, Reply, Store, ago, now_ms};
use serde_json::{Value, json};

use crate::todo_json;

pub fn serve(bind: &str, token: Option<&str>) -> Result<(), String> {
    let listener = TcpListener::bind(bind).map_err(|e| format!("failed to bind {bind}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    eprintln!("Feeder companion listening on http://{addr}");
    if token.is_some() {
        eprintln!("Auth: Bearer token required (or open /?token=...)");
    } else if !addr.ip().is_loopback() {
        eprintln!("Warning: bound to non-loopback address without --token");
    }
    let token = token.map(str::to_string);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let tok = token.clone();
        std::thread::spawn(move || {
            let _ = handle_connection(stream, tok.as_deref());
        });
    }
    Ok(())
}

struct HttpRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn handle_connection(mut stream: TcpStream, token: Option<&str>) -> std::io::Result<()> {
    let req = match read_request(&mut stream) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        Err(msg) => return write_response(&mut stream, "400 Bad Request", "text/plain; charset=utf-8", msg.as_bytes(), None),
    };

    if let Some(expected) = token
        && !authorized(&req, expected)
    {
        return write_response(
            &mut stream,
            "401 Unauthorized",
            "text/plain; charset=utf-8",
            b"Unauthorized: supply Authorization: Bearer <token> or ?token=<token>",
            None,
        );
    }

    let set_cookie = token.and_then(|expected| {
        req.query.get("token").filter(|t| t.as_str() == expected).map(|t| format!("feeder_token={t}; HttpOnly; SameSite=Lax; Path=/"))
    });

    let store = match Store::open_default() {
        Ok(s) => s,
        Err(e) => return write_json(&mut stream, "500 Internal Server Error", &json!({ "error": e.to_string() })),
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => write_response(&mut stream, "200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes(), set_cookie.as_deref()),
        ("GET", "/api/state") => match build_state_json(&store, req.query.get("project").map(String::as_str)) {
            Ok(v) => write_json(&mut stream, "200 OK", &v),
            Err(e) => write_json(&mut stream, "500 Internal Server Error", &json!({ "error": e })),
        },
        ("GET", "/api/events") => serve_events(&mut stream, &store, set_cookie.as_deref()),
        ("POST", "/api/reply") => {
            let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "invalid JSON" }));
            };
            let Some(update_id) = v.get("update_id").and_then(Value::as_i64) else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "update_id required" }));
            };
            let body = v.get("body").and_then(Value::as_str).unwrap_or("");
            let author = v.get("author").and_then(Value::as_str).unwrap_or("You");
            match store.add_reply(update_id, author, true, body) {
                Ok(reply) => write_json(&mut stream, "200 OK", &json!({ "reply": reply_json(&reply, now_ms()) })),
                Err(e) => write_json(&mut stream, "400 Bad Request", &json!({ "error": e.to_string() })),
            }
        }
        ("POST", "/api/brief") => {
            let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "invalid JSON" }));
            };
            let Some(project_id) = v.get("project_id").and_then(Value::as_i64) else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "project_id required" }));
            };
            let brief = v.get("brief").and_then(Value::as_str);
            match store.set_project_brief(project_id, brief) {
                Ok(()) => write_json(&mut stream, "200 OK", &json!({ "ok": true })),
                Err(e) => write_json(&mut stream, "400 Bad Request", &json!({ "error": e.to_string() })),
            }
        }
        ("POST", "/api/todo") => {
            let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "invalid JSON" }));
            };
            match handle_todo_post(&store, &v) {
                Ok(out) => write_json(&mut stream, "200 OK", &out),
                Err(e) => write_json(&mut stream, "400 Bad Request", &json!({ "error": e })),
            }
        }
        ("POST", "/api/seen") => {
            let v = serde_json::from_slice::<Value>(&req.body).unwrap_or(json!({}));
            let res = if v.get("all").and_then(Value::as_bool) == Some(true) {
                store.mark_all_seen()
            } else if let Some(pid) = v.get("project_id").and_then(Value::as_i64) {
                store.mark_seen(pid)
            } else {
                return write_json(&mut stream, "400 Bad Request", &json!({ "error": "project_id or all=true required" }));
            };
            match res {
                Ok(()) => write_json(&mut stream, "200 OK", &json!({ "ok": true })),
                Err(e) => write_json(&mut stream, "500 Internal Server Error", &json!({ "error": e.to_string() })),
            }
        }
        ("GET", p) if p.starts_with("/media/") => {
            let name = p.trim_start_matches("/media/");
            serve_media(&mut stream, &store, name)
        }
        _ => write_response(&mut stream, "404 Not Found", "text/plain; charset=utf-8", b"Not Found", None),
    }
}

fn serve_events(stream: &mut TcpStream, store: &Store, set_cookie: Option<&str>) -> std::io::Result<()> {
    let mut head = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n",
    );
    if let Some(cookie) = set_cookie {
        head.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(b": connected\n\n")?;
    stream.flush()?;

    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let mut last_version = store.data_version().ok();
    let mut ticks = 0u32;
    let mut probe = [0u8; 1];
    loop {
        match stream.read(&mut probe) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        let version = store.data_version().ok();
        if version != last_version {
            last_version = version;
            let payload = format!("event: update\ndata: {{\"version\":{}}}\n\n", version.unwrap_or(0));
            if stream.write_all(payload.as_bytes()).is_err() || stream.flush().is_err() {
                break;
            }
            ticks = 0;
        } else {
            ticks += 1;
            if ticks >= 30 {
                ticks = 0;
                if stream.write_all(b": keepalive\n\n").is_err() || stream.flush().is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn handle_todo_post(store: &Store, v: &Value) -> Result<Value, String> {
    let action = v.get("action").and_then(Value::as_str).ok_or("action required")?;
    match action {
        "add" => {
            let project_id = v.get("project_id").and_then(Value::as_i64).ok_or("project_id required")?;
            let title = v.get("title").and_then(Value::as_str).ok_or("title required")?;
            let item = store.add_todo(project_id, title).map_err(|e| e.to_string())?;
            Ok(json!({ "todo": todo_json(&item) }))
        }
        "toggle" => {
            let id = v.get("id").and_then(Value::as_i64).ok_or("id required")?;
            let done = match v.get("done").and_then(Value::as_bool) {
                Some(explicit) => explicit,
                None => {
                    let current = store.todo(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no to-do {id}"))?;
                    !current.done
                }
            };
            let item = store.set_todo_done(id, done).map_err(|e| e.to_string())?;
            Ok(json!({ "todo": todo_json(&item) }))
        }
        "rename" => {
            let id = v.get("id").and_then(Value::as_i64).ok_or("id required")?;
            let title = v.get("title").and_then(Value::as_str).ok_or("title required")?;
            let item = store.rename_todo(id, title).map_err(|e| e.to_string())?;
            Ok(json!({ "todo": todo_json(&item) }))
        }
        "delete" => {
            let id = v.get("id").and_then(Value::as_i64).ok_or("id required")?;
            store.delete_todo(id).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "clear_done" => {
            let project_id = v.get("project_id").and_then(Value::as_i64).ok_or("project_id required")?;
            let removed = store.clear_completed_todos(project_id).map_err(|e| e.to_string())?;
            Ok(json!({ "removed": removed }))
        }
        "clear_all" => {
            let project_id = v.get("project_id").and_then(Value::as_i64).ok_or("project_id required")?;
            let removed = store.clear_project_todos(project_id).map_err(|e| e.to_string())?;
            Ok(json!({ "removed": removed }))
        }
        other => Err(format!("unknown action: {other}")),
    }
}

fn build_state_json(store: &Store, project_slug: Option<&str>) -> Result<Value, String> {
    let now = now_ms();
    let projects = store.projects().map_err(|e| e.to_string())?;
    let active: Vec<&ProjectSummary> = projects.iter().filter(|p| !p.project.archived).collect();
    let selected = project_slug
        .and_then(|q| active.iter().find(|p| p.project.slug.eq_ignore_ascii_case(q) || p.project.name.eq_ignore_ascii_case(q)).copied());
    let updates: Vec<FeedUpdate> = match selected {
        Some(p) => store.feed(p.project.id, 60, None).map_err(|e| e.to_string())?,
        None => store.timeline(60).map_err(|e| e.to_string())?,
    };
    let todos_json: Vec<Value> = match selected {
        Some(p) => store.todos(p.project.id).map_err(|e| e.to_string())?.iter().map(todo_json).collect(),
        None => Vec::new(),
    };
    let ids: Vec<i64> = updates.iter().map(|u| u.id).collect();
    let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
    let mut reply_map: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &replies {
        reply_map.entry(r.update_id).or_default().push(reply_json(r, now));
    }
    let proj_lookup: HashMap<i64, &Project> = active.iter().map(|s| (s.project.id, &s.project)).collect();
    let updates_json: Vec<Value> = updates
        .iter()
        .map(|u| {
            let proj = proj_lookup.get(&u.project_id);
            let media = u.screenshot.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            let media_kind = u.media_kind().map(|k| match k {
                feeder_core::MediaKind::Image => "image",
                feeder_core::MediaKind::Video => "video",
            });
            json!({
                "id": u.id,
                "project_id": u.project_id,
                "project_name": proj.map(|p| p.name.as_str()).unwrap_or("Project"),
                "project_slug": proj.map(|p| p.slug.as_str()).unwrap_or(""),
                "agent": u.agent,
                "body": u.body,
                "branch": u.branch,
                "diff_stat": u.diff_stat,
                "commit_sha": u.commit_sha,
                "media_url": media.map(|m| format!("/media/{m}")),
                "media_kind": media_kind,
                "media_w": u.media_w,
                "media_h": u.media_h,
                "ago": ago(u.created_at, now),
                "created_at": u.created_at,
                "replies": reply_map.get(&u.id).cloned().unwrap_or_default(),
            })
        })
        .collect();

    let projects_json: Vec<Value> = active
        .iter()
        .map(|s| {
            json!({
                "id": s.project.id,
                "slug": s.project.slug,
                "name": s.project.name,
                "brief": s.project.brief,
                "pinned": s.project.pinned,
                "unseen": s.unseen(),
                "needs_you": s.needs_you,
                "unread_replies": s.unread_replies,
                "open_todos": s.open_todos,
                "updates": s.updates,
                "last_ago": s.last_at.map(|at| ago(at, now)),
                "last_agent": s.last_agent,
                "pulse": s.active_pulse.as_ref().map(|p| json!({
                    "agent": p.agent,
                    "branch": p.branch,
                    "status": p.status,
                })),
            })
        })
        .collect();

    Ok(json!({
        "projects": projects_json,
        "todos": todos_json,
        "updates": updates_json,
        "selected": selected.map(|s| s.project.slug.as_str()),
    }))
}

fn reply_json(r: &Reply, now: i64) -> Value {
    json!({
        "id": r.id,
        "update_id": r.update_id,
        "author": r.author,
        "from_human": r.from_human,
        "body": r.body,
        "ago": ago(r.created_at, now),
        "read": r.read_at.is_some(),
    })
}

fn serve_media(stream: &mut TcpStream, store: &Store, name: &str) -> std::io::Result<()> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', '\0']) {
        return write_response(stream, "400 Bad Request", "text/plain; charset=utf-8", b"Invalid media name", None);
    }
    let path = store.dir().join("media").join(name);
    let Ok(bytes) = std::fs::read(&path) else {
        return write_response(stream, "404 Not Found", "text/plain; charset=utf-8", b"Media not found", None);
    };
    let mime = mime_for(&path);
    write_response(stream, "200 OK", mime, &bytes, None)
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

fn authorized(req: &HttpRequest, expected: &str) -> bool {
    if let Some(auth) = req.headers.get("authorization")
        && let Some(bearer) = auth.strip_prefix("Bearer ")
        && bearer.trim() == expected
    {
        return true;
    }
    if req.query.get("token").is_some_and(|t| t == expected) {
        return true;
    }
    if let Some(cookie) = req.headers.get("cookie") {
        for part in cookie.split(';') {
            if let Some((k, v)) = part.trim().split_once('=')
                && k.trim() == "feeder_token"
                && v.trim() == expected
            {
                return true;
            }
        }
    }
    false
}

fn read_request(stream: &mut TcpStream) -> Result<Option<HttpRequest>, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("malformed request line".into());
    }
    let method = parts[0].to_string();
    let target = parts[1];
    let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.to_string();
    let query = parse_query(raw_query);

    let mut headers = HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = headers.get("content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(1024 * 1024);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    }
    Ok(Some(HttpRequest { method, path, query, headers, body }))
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn write_json(stream: &mut TcpStream, status: &str, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    write_response(stream, status, "application/json; charset=utf-8", &body, None)
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8], set_cookie: Option<&str>) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(cookie) = set_cookie {
        head.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Feeder Companion</title>
<style>
:root{color-scheme:light dark;--bg:#f6f6f7;--card:#fff;--fg:#18191c;--muted:#676b75;--line:rgba(0,0,0,.09);--hover:rgba(0,0,0,.045);--accent:#3b82f6;--warn:#d97706;--live:#10b981;}
@media(prefers-color-scheme:dark){:root{--bg:#121316;--card:#1b1c20;--fg:#f2f3f5;--muted:#9096a3;--line:rgba(255,255,255,.09);--hover:rgba(255,255,255,.06);}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;}
header{position:sticky;top:0;background:var(--bg);border-bottom:1px solid var(--line);padding:12px 16px;display:flex;gap:10px;align-items:center;flex-wrap:wrap;z-index:10;}
.brand{font-weight:650;font-size:15px;margin-right:auto;}
.tabs,.projects{display:flex;gap:6px;flex-wrap:wrap;}
button,input,textarea{font:inherit;color:inherit;}
.btn{border:1px solid var(--line);background:var(--card);border-radius:8px;padding:5px 10px;cursor:pointer;}
.btn:hover{background:var(--hover);}
.btn.active{border-color:var(--accent);color:var(--accent);}
main{max-width:740px;margin:0 auto;padding:16px;}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px;margin-bottom:14px;}
.meta{color:var(--muted);font-size:12px;display:flex;gap:8px;align-items:center;flex-wrap:wrap;}
.pill{padding:2px 7px;border-radius:6px;background:var(--hover);font-family:ui-monospace,monospace;font-size:11px;}
.dot{width:7px;height:7px;border-radius:99px;display:inline-block;}
.dot.unseen{background:var(--accent);}.dot.needs{background:var(--warn);}.dot.live{background:var(--live);}
.body{white-space:pre-wrap;margin-top:10px;}
.media{margin-top:10px;max-width:100%;border-radius:8px;border:1px solid var(--line);}
.replies{margin-top:12px;border-top:1px solid var(--line);padding-top:10px;display:flex;flex-direction:column;gap:6px;}
.reply{font-size:13px;background:var(--hover);padding:7px 10px;border-radius:8px;}
.reply-form{display:flex;gap:8px;margin-top:8px;}
.reply-form input{flex:1;padding:6px 10px;border-radius:8px;border:1px solid var(--line);background:transparent;}
.brief{white-space:pre-wrap;font-size:13px;color:var(--muted);}
.todo-head{display:flex;align-items:center;gap:8px;margin-bottom:8px;}
.todo-head strong{margin-right:auto;font-size:13px;}
.todo-list{display:flex;flex-direction:column;gap:6px;margin-bottom:8px;}
.todo-row{display:flex;align-items:center;gap:8px;padding:4px 0;}
.todo-row input[type=checkbox]{width:15px;height:15px;cursor:pointer;accent-color:var(--accent);}
.todo-row span{flex:1;min-width:0;word-break:break-word;}
.todo-row.done span{text-decoration:line-through;color:var(--muted);}
.btn-del{border:0;background:transparent;color:var(--muted);cursor:pointer;padding:2px 6px;border-radius:6px;}
.btn-del:hover{background:var(--hover);color:var(--fg);}
</style>
</head>
<body>
<header>
  <span class="brand">Feeder</span>
  <div class="tabs">
    <button class="btn" id="tab-feed" onclick="setMode('feed')">Feed</button>
    <button class="btn" onclick="markAllRead()">Mark all read</button>
  </div>
</header>
<main>
  <div class="projects" id="projects" style="margin-bottom:14px"></div>
  <div id="content">Loading…</div>
</main>
<script>
let state = null, selected = null, mode = 'feed', pendingRender = false, todoOpen = false, todoClearConfirm = false;
const urlTok = new URLSearchParams(location.search).get('token');
function apiUrl(path){
  if(!urlTok) return path;
  return path + (path.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(urlTok);
}
function isTyping(){
  const a = document.activeElement;
  return a && (a.tagName === 'INPUT' || a.tagName === 'TEXTAREA');
}
async function load(background) {
  const q = selected ? '?project=' + encodeURIComponent(selected) : '';
  const r = await fetch(apiUrl('/api/state' + q));
  state = await r.json();
  if(background && isTyping()){ pendingRender = true; return; }
  pendingRender = false;
  render();
}
document.addEventListener('focusout', () => {
  setTimeout(() => { if(pendingRender && !isTyping()){ pendingRender = false; render(); } }, 80);
});
function setMode(m){ mode = m; render(); }
function pickProject(slug){ selected = selected === slug ? null : slug; mode = 'feed'; todoOpen = false; todoClearConfirm = false; load(false); }
function openTodos(){ todoOpen = true; todoClearConfirm = false; render(); const el = document.getElementById('todo-input'); if(el) el.focus(); }
function closeEmptyTodos(){ todoOpen = false; todoClearConfirm = false; render(); }
async function markAllRead(){ await fetch(apiUrl('/api/seen'), {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({all:true})}); load(false); }
async function sendReply(uid, el){
  const body = el.value.trim(); if(!body) return;
  await fetch(apiUrl('/api/reply'),{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({update_id:uid,body})});
  el.value=''; load(false);
}
async function saveBrief(pid){
  const el = document.getElementById('brief-input');
  await fetch(apiUrl('/api/brief'),{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({project_id:pid,brief:el.value})});
  load(false);
}
async function postTodo(payload){
  await fetch(apiUrl('/api/todo'),{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});
  load(false);
}
async function addTodo(pid, el){
  const title = el.value.trim(); if(!title) return;
  el.value = ''; todoOpen = true; todoClearConfirm = false;
  await postTodo({action:'add', project_id:pid, title});
}
async function toggleTodo(id, done){ todoClearConfirm = false; await postTodo({action:'toggle', id, done}); }
async function deleteTodo(id){ todoClearConfirm = false; await postTodo({action:'delete', id}); }
async function clearDoneTodos(pid){ todoClearConfirm = false; await postTodo({action:'clear_done', project_id:pid}); }
async function clearAllTodos(pid){
  if(!todoClearConfirm){ todoClearConfirm = true; render(); return; }
  todoClearConfirm = false; todoOpen = false;
  await postTodo({action:'clear_all', project_id:pid});
}
function cancelClearTodos(){ todoClearConfirm = false; render(); }
function esc(s){return (s||'').replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));}
function render(){
  if(!state) return;
  document.getElementById('tab-feed').classList.toggle('active', mode==='feed');
  const pEl = document.getElementById('projects');
  pEl.innerHTML = state.projects.map(p => {
    const active = selected === p.slug ? 'active' : '';
    const dot = p.pulse ? '<span class="dot live" title="Active"></span>' : (p.needs_you ? '<span class="dot needs" title="Needs you"></span>' : (p.unseen ? '<span class="dot unseen"></span>' : ''));
    return `<button class="btn ${active}" onclick="pickProject('${esc(p.slug)}')">${dot} ${esc(p.name)}</button>`;
  }).join('');
  const cEl = document.getElementById('content');
  let html = '';
  if(selected){
    const proj = state.projects.find(p => p.slug === selected);
    if(proj){
      const todos = state.todos || [];
      const showTodoCard = todoOpen || todos.length > 0;
      html += `<div class="card"><div class="meta"><strong>${esc(proj.name)}</strong>${proj.pulse?`<span class="pill">${esc(proj.pulse.agent)}: ${esc(proj.pulse.status)}</span>`:''}</div>`;
      html += `<div style="margin-top:8px"><textarea id="brief-input" rows="2" style="width:100%;padding:8px;border-radius:8px;border:1px solid var(--line);background:transparent" placeholder="Pinned project brief (Goal / Focus / Commands)…">${esc(proj.brief||'')}</textarea><div style="margin-top:6px;display:flex;gap:8px"><button class="btn" onclick="saveBrief(${proj.id})">Save brief</button>${showTodoCard?'':`<button class="btn" onclick="openTodos()">+ To-do</button>`}</div></div></div>`;
      if(showTodoCard){
        const openCount = todos.filter(t => !t.done).length;
        const doneCount = todos.length - openCount;
        html += `<div class="card"><div class="todo-head"><strong>${openCount ? `To-do · ${openCount}` : 'To-do'}</strong>`;
        if(doneCount > 0){ html += `<button class="btn" onclick="clearDoneTodos(${proj.id})">Clear done</button>`; }
        if(todos.length > 0){
          html += todoClearConfirm
            ? `<button class="btn" onclick="clearAllTodos(${proj.id})">Confirm clear all</button><button class="btn" onclick="cancelClearTodos()">Cancel</button>`
            : `<button class="btn" onclick="clearAllTodos(${proj.id})">Clear all</button>`;
        } else {
          html += `<button class="btn" onclick="closeEmptyTodos()">Close</button>`;
        }
        html += `</div>`;
        if(todos.length > 0){
          html += `<div class="todo-list">`;
          for(const t of todos){
            html += `<div class="todo-row ${t.done?'done':''}"><input type="checkbox" ${t.done?'checked':''} onchange="toggleTodo(${t.id},this.checked)"><span>${esc(t.title)}</span><button class="btn-del" title="Delete to-do" onclick="deleteTodo(${t.id})">×</button></div>`;
          }
          html += `</div>`;
        }
        html += `<form class="reply-form" onsubmit="event.preventDefault();addTodo(${proj.id},this.querySelector('input'))"><input id="todo-input" placeholder="Add a to-do…"><button class="btn" type="submit">Add</button></form></div>`;
      }
    }
  }
  if(!state.updates.length){ html += '<div class="card meta">No updates yet.</div>'; }
  for(const u of state.updates){
    html += `<div class="card"><div class="meta"><strong>${esc(u.project_name)}</strong><span>·</span><span>${esc(u.agent)}</span>${u.branch?`<span class="pill">${esc(u.branch)}</span>`:''}${u.diff_stat?`<span class="pill">${esc(u.diff_stat)}</span>`:''}<span>·</span><span>${esc(u.ago)}</span></div>`;
    html += `<div class="body">${esc(u.body)}</div>`;
    if(u.media_url){
      const dims = (u.media_w && u.media_h) ? ` width="${u.media_w}" height="${u.media_h}" style="height:auto"` : '';
      html += u.media_kind === 'video'
        ? `<video class="media" controls playsinline src="${esc(apiUrl(u.media_url))}"></video>`
        : `<img class="media"${dims} src="${esc(apiUrl(u.media_url))}" alt="">`;
    }
    html += `<div class="replies">`;
    for(const r of u.replies){
      html += `<div class="reply"><strong>${esc(r.author)}</strong> <span class="meta" style="display:inline">${esc(r.ago)}</span>: ${esc(r.body)}</div>`;
    }
    html += `<form class="reply-form" onsubmit="event.preventDefault();sendReply(${u.id},this.querySelector('input'))"><input placeholder="Reply or answer decision…"><button class="btn" type="submit">Send</button></form></div></div>`;
  }
  cEl.innerHTML = html;
}
load(false);
if(window.EventSource){
  const es = new EventSource(apiUrl('/api/events'));
  es.addEventListener('update', () => load(true));
}
setInterval(() => load(true), 30000);
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_and_auth_check_work() {
        let q = parse_query("project=my+app&token=sec%20123");
        assert_eq!(q.get("project").map(String::as_str), Some("my app"));
        assert_eq!(q.get("token").map(String::as_str), Some("sec 123"));

        let req = HttpRequest {
            method: "GET".into(),
            path: "/".into(),
            query: HashMap::new(),
            headers: HashMap::from([("authorization".into(), "Bearer secret-xyz".into())]),
            body: Vec::new(),
        };
        assert!(authorized(&req, "secret-xyz"));
        assert!(!authorized(&req, "wrong"));
    }

    #[test]
    fn media_mime_handles_images_and_video() {
        assert_eq!(mime_for(Path::new("shot.png")), "image/png");
        assert_eq!(mime_for(Path::new("clip.mp4")), "video/mp4");
        assert_eq!(mime_for(Path::new("clip.mov")), "video/quicktime");
    }

    #[test]
    fn todo_api_and_state_json_roundtrip() {
        let dir = std::env::temp_dir().join(format!("feeder-web-todo-{}-{}", std::process::id(), now_ms()));
        let store = Store::open(&dir).unwrap();
        let p = store.project_named("Delta", None).unwrap();

        let added = handle_todo_post(&store, &json!({ "action": "add", "project_id": p.id, "title": "First task" })).unwrap();
        let tid = added.pointer("/todo/id").and_then(Value::as_i64).unwrap();
        assert_eq!(added.pointer("/todo/done"), Some(&json!(false)));

        let renamed = handle_todo_post(&store, &json!({ "action": "rename", "id": tid, "title": "Updated task" })).unwrap();
        assert_eq!(renamed.pointer("/todo/title"), Some(&json!("Updated task")));

        let state_open = build_state_json(&store, Some("delta")).unwrap();
        assert_eq!(state_open.pointer("/projects/0/open_todos"), Some(&json!(1)));
        assert_eq!(state_open.pointer("/todos/0/title"), Some(&json!("Updated task")));

        let toggled = handle_todo_post(&store, &json!({ "action": "toggle", "id": tid })).unwrap();
        assert_eq!(toggled.pointer("/todo/done"), Some(&json!(true)));

        let cleared = handle_todo_post(&store, &json!({ "action": "clear_done", "project_id": p.id })).unwrap();
        assert_eq!(cleared.pointer("/removed"), Some(&json!(1)));

        let _ = handle_todo_post(&store, &json!({ "action": "add", "project_id": p.id, "title": "Temp task" })).unwrap();
        let cleared_all = handle_todo_post(&store, &json!({ "action": "clear_all", "project_id": p.id })).unwrap();
        assert_eq!(cleared_all.pointer("/removed"), Some(&json!(1)));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sse_events_stream_emits_update_on_store_commit() {
        let dir = std::env::temp_dir().join(format!("feeder-web-sse-{}-{}", std::process::id(), now_ms()));
        let reader_store = Store::open(&dir).unwrap();
        let writer_store = Store::open(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let _ = serve_events(&mut sock, &reader_store, None);
        });

        let mut client = TcpStream::connect(addr).unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        let mut buf = [0u8; 512];
        let mut received = String::new();
        while !received.contains(": connected") {
            let n = client.read(&mut buf).unwrap();
            assert!(n > 0, "EOF before SSE connected comment; got: {received}");
            received.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(received.contains("text/event-stream"), "got: {received}");

        let p = writer_store.project_named("Live", None).unwrap();
        writer_store.post(p.id, feeder_core::NewUpdate { agent: "Codex".into(), body: "SSE ping".into(), ..Default::default() }).unwrap();

        while !received.contains("event: update") {
            let n2 = client.read(&mut buf).unwrap();
            assert!(n2 > 0, "EOF before SSE update event; got: {received}");
            received.push_str(&String::from_utf8_lossy(&buf[..n2]));
        }
        assert!(received.contains("event: update"), "got: {received}");

        drop(client);
        let _ = server.join();
        std::fs::remove_dir_all(dir).ok();
    }
}
