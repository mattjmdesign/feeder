use std::cell::{Ref, RefCell};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use feeder_core::{MAX_CHARS, MAX_PARAGRAPHS, Project, Store, agent_display_name, agent_from_env, detect_project};
use serde_json::{Value, json};

use self::protocol::{PROTOCOL_VERSIONS, rpc_error, supports_batching};

mod feed;
mod gardener;
mod library;
mod protocol;
mod resources;

const MAX_RPC_LINE_BYTES: usize = 16 * 1024 * 1024;
const BLOCKED_PROJECT_ROOTS: [&str; 8] = ["/Users", "/Applications", "/System", "/Library", "/tmp", "/private/tmp", "/var", "/private/var"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Agent,
    Gardener(i64),
}

struct OpenedStore {
    store: Store,
    dev_ino: Option<(u64, u64)>,
}

pub struct Server {
    client: Option<String>,
    protocol_version: String,
    cwd: PathBuf,
    store_dir: Option<PathBuf>,
    store: RefCell<Option<OpenedStore>>,
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
            Some(rpc_error(Value::Null, -32600, "request exceeds maximum size"))
        } else {
            let line = String::from_utf8_lossy(&buf);
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    if !supports_batching(&server.protocol_version) {
                        Some(rpc_error(Value::Null, -32600, "JSON-RPC batching is not supported in this MCP protocol version"))
                    } else {
                        let replies: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                        (!replies.is_empty()).then_some(Value::Array(replies))
                    }
                }
                Ok(msg) => server.handle(&msg),
                Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
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

fn db_dev_ino(db_path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(db_path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
        None
    }
}

fn has_git_root(dir: &Path) -> bool {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    canon.ancestors().any(|p| p.join(".git").exists())
}

fn is_blocked_project_root(root: &Path) -> bool {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if canon.parent().is_none() {
        return true;
    }
    if dirs::home_dir().and_then(|h| h.canonicalize().ok().or(Some(h))).is_some_and(|h| h == canon) {
        return true;
    }
    BLOCKED_PROJECT_ROOTS.iter().any(|p| Path::new(p) == canon)
}

fn agent_instructions() -> String {
    format!(
        "Feeder is the user's local project update centre and shared engineering Library.\n\n\
         Feed workflow:\n\
         - Start or resume with `list_projects` (`filter: \"pending\"`) or `get_feed` (pass `task` keywords for up to 3 relevant Library notes).\n\
         - Show live activity on longer tasks via `update_project` (`pulse`).\n\
         - Post meaningful milestones with `post_update` in 1-{MAX_PARAGRAPHS} short Markdown blocks under {MAX_CHARS} chars separated by blank lines (`Done:`/`Shipped:`/`Fixed:` outcome + proof, `Why:`/`Benefit:`, `Next:` or `Decision needed:`/`Blocked:` for Needs you). Pass `media` paths when visual, plus optional `mark_replies_read`, `complete_todos`, `add_todos`, `helped_notes`, and `flag_lesson` in the same call.\n\n\
         Library workflow:\n\
         - Call `search_library` before non-trivial debugging or unfamiliar choices, then `read_library` on matching IDs.\n\
         - On user corrections or non-obvious fixes/pitfalls/patterns, call `flag_knowledge` with a full Markdown draft in `suggestion` for Curator review; never write shared notes directly.\n\
         - Never include secrets, tokens, or keys anywhere in Feeder."
    )
}

impl Server {
    fn new(client: Option<String>, cwd: PathBuf, store_dir: Option<PathBuf>) -> Self {
        Self { client, protocol_version: PROTOCOL_VERSIONS[0].to_string(), cwd, store_dir, store: RefCell::new(None), role: Role::Agent }
    }

    pub(super) fn open_store(&self) -> Result<Ref<'_, Store>, String> {
        let dir = self.store_dir.as_deref().map(Path::to_path_buf).unwrap_or_else(feeder_core::data_dir);
        let db_path = dir.join("feeder.db");
        let current_id = db_dev_ino(&db_path);
        let needs_reopen = self.store.borrow().as_ref().is_none_or(|opened| opened.dev_ino != current_id);
        if needs_reopen {
            *self.store.borrow_mut() = None;
            let store = match &self.store_dir {
                Some(path) => Store::open(path),
                None => Store::open_default(),
            }
            .map_err(|e| e.to_string())?;
            let dev_ino = db_dev_ino(&db_path);
            *self.store.borrow_mut() = Some(OpenedStore { store, dev_ino });
        }
        Ok(Ref::map(self.store.borrow(), |opt| &opt.as_ref().expect("store initialized").store))
    }

    pub(super) fn resolve_agent(&self, explicit: Option<&str>) -> String {
        explicit
            .map(agent_display_name)
            .or_else(agent_from_env)
            .or_else(|| self.client.as_deref().map(agent_display_name))
            .unwrap_or_else(|| "Unknown agent".into())
    }

    pub(super) fn base(&self, explicit_path: Option<&str>) -> PathBuf {
        match explicit_path {
            Some(raw) => {
                let p = PathBuf::from(raw);
                if p.is_absolute() { p } else { self.cwd.join(p) }
            }
            None => self.cwd.clone(),
        }
    }

    pub(super) fn known_projects_hint(&self, store: &Store) -> String {
        let Ok(list) = store.projects() else {
            return String::new();
        };
        let names: Vec<_> = list.into_iter().filter(|p| !p.project.archived).map(|p| p.project.name).take(10).collect();
        if names.is_empty() { String::new() } else { format!(" (active projects: {})", names.join(", ")) }
    }

    pub(super) fn resolve_project(
        &self,
        store: &Store,
        explicit_name: Option<&str>,
        explicit_path: Option<&str>,
        create: bool,
    ) -> Result<Option<Project>, String> {
        if let Some(name) = explicit_name {
            return if create { store.project_named(name, None).map(Some) } else { store.find_project(name) }.map_err(|e| e.to_string());
        }
        if let Some(raw_path) = explicit_path {
            let folder = self.base(Some(raw_path));
            if !folder.is_dir() {
                return Err(format!("`project_path` does not exist or is not a folder: {}", folder.display()));
            }
            let detected = detect_project(&folder);
            if create && is_blocked_project_root(&detected.path) {
                return Err(format!(
                    "refusing to create a project at `{}`; pass an explicit `project` name or project folder{}",
                    detected.path.display(),
                    self.known_projects_hint(store)
                ));
            }
            return if create { store.project_at(&detected).map(Some) } else { store.find_detected(&detected) }.map_err(|e| e.to_string());
        }
        let detected = detect_project(&self.cwd);
        if let Some(existing) = store.find_detected(&detected).map_err(|e| e.to_string())? {
            return Ok(Some(existing));
        }
        if !create {
            return Ok(None);
        }
        if !has_git_root(&self.cwd) || is_blocked_project_root(&detected.path) {
            return Err(format!(
                "couldn't infer a project from `{}` (not a project git repository); pass `project` or `project_path`{}",
                self.cwd.display(),
                self.known_projects_hint(store)
            ));
        }
        store.project_at(&detected).map(Some).map_err(|e| e.to_string())
    }

    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?;
        match method {
            "initialize" => match self.initialize(&params) {
                Ok(result) => Some(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
                Err(msg) => Some(rpc_error(id, -32603, &msg)),
            },
            "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            "tools/list" => {
                let res = match self.role {
                    Role::Agent => Ok(json!({ "tools": all_tools() })),
                    Role::Gardener(run) => self.curator_mode(run).map(|(mode, is_chat)| json!({ "tools": gardener::tools(mode, is_chat) })),
                };
                Some(match res {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err(msg) => rpc_error(id, -32603, &msg),
                })
            }
            "tools/call" => Some(match self.handle_tool_call(&params) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, msg)) => rpc_error(id, code, &msg),
            }),
            "resources/list" if self.role == Role::Agent => Some(match self.resources_list() {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(msg) => rpc_error(id, -32603, &msg),
            }),
            "resources/templates/list" if self.role == Role::Agent => {
                Some(json!({ "jsonrpc": "2.0", "id": id, "result": self.resource_templates_list() }))
            }
            "resources/read" if self.role == Role::Agent => Some(match self.resources_read(&params) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, msg)) => rpc_error(id, code, &msg),
            }),
            "prompts/list" if self.role == Role::Agent => Some(json!({ "jsonrpc": "2.0", "id": id, "result": self.prompts_list() })),
            "prompts/get" if self.role == Role::Agent => Some(self.prompts_get(id, &params)),
            _ => Some(rpc_error(id, -32601, &format!("method not found: {method}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, String> {
        self.client = params.pointer("/clientInfo/name").and_then(Value::as_str).map(str::to_string);
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSIONS[0]);
        let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
        self.protocol_version = version.to_string();
        let (capabilities, instructions) = match self.role {
            Role::Agent => (json!({ "tools": {}, "prompts": {}, "resources": {} }), agent_instructions()),
            Role::Gardener(run) => (json!({ "tools": {} }), self.curator_mode(run)?.0.instructions().to_string()),
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": capabilities,
            "serverInfo": { "name": "feeder", "version": env!("CARGO_PKG_VERSION") },
            "instructions": instructions
        }))
    }

    fn handle_tool_call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(name) = params.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            return Err((-32602, "`name` string is required".into()));
        };
        if params.get("arguments").is_some_and(|v| !v.is_null() && !v.is_object()) {
            return Err((-32602, "`arguments` must be a JSON object".into()));
        }
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match (self.role, name) {
            (Role::Gardener(run), _) => Ok(tool_result(self.gardener_call(run, name, &args))),
            (Role::Agent, _) if library::TOOLS.contains(&name) => Ok(tool_result(self.library_call(name, &args))),
            (Role::Agent, "write_page" | "archive_page" | "dismiss_flag") => Ok(tool_result(Err(
                "everyday agents cannot edit shared Library notes directly; use `flag_knowledge` with a draft in `suggestion`".into(),
            ))),
            (Role::Agent, _) if is_agent_feed_tool(name) => Ok(tool_result(self.feed_call(name, &args))),
            (Role::Agent, _) => Err((-32602, format!("unknown tool: {name}; call tools/list to see available tools"))),
        }
    }

    #[cfg(test)]
    fn call(&self, params: &Value) -> Value {
        match self.handle_tool_call(params) {
            Ok(result) => result,
            Err((_, message)) => tool_result(Err(message)),
        }
    }
}

fn is_agent_feed_tool(name: &str) -> bool {
    matches!(
        name,
        "list_projects"
            | "get_feed"
            | "search_updates"
            | "post_update"
            | "reply_to_update"
            | "edit_update"
            | "update_project"
            | "delete_item"
            | "get_recent_activity"
            | "get_pending_decisions"
            | "dismiss_attention"
            | "mark_replies_read"
            | "set_project_brief"
            | "pulse"
            | "list_todos"
            | "add_todo"
            | "toggle_todo"
            | "delete_todo"
            | "delete_update"
    )
}

fn tool_result(outcome: Result<ToolOutput, String>) -> Value {
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

fn all_tools() -> Value {
    let mut list = feed::tools();
    if let Value::Array(items) = &mut list {
        items.extend(library::tools());
    }
    list
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

    fn init_git(dir: &Path) {
        let ok = std::process::Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(dir).status().unwrap().success();
        assert!(ok);
    }

    fn call_rpc(server: &mut Server, id: i64, name: &str, arguments: Value) -> Value {
        server
            .handle(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }))
            .unwrap()
    }

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(feed::base64_encode(b""), "");
        assert_eq!(feed::base64_encode(b"f"), "Zg==");
        assert_eq!(feed::base64_encode(b"fo"), "Zm8=");
        assert_eq!(feed::base64_encode(b"foo"), "Zm9v");
        assert_eq!(feed::base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn schema_annotations_and_context_budgets_hold() {
        let store_dir = temp_dir("schema-budget");
        let work_dir = temp_dir("work-budget");
        let mut server = Server::new(None, work_dir.clone(), Some(store_dir.clone()));
        let init = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2025-11-25", "clientInfo": { "name": "claude-code" } }
            }))
            .unwrap();
        let instructions = init.pointer("/result/instructions").and_then(Value::as_str).unwrap();
        assert!(instructions.len() <= 1200, "instructions too long: {}", instructions.len());
        assert!(!instructions.contains("feeder "), "MCP instructions must stay free of CLI syntax");

        let listed = server.handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} })).unwrap();
        let tools = listed.pointer("/result/tools").and_then(Value::as_array).unwrap();
        assert_eq!(tools.len(), 11);
        let serialized_len = serde_json::to_string(tools).unwrap().len();
        assert!(serialized_len <= 9000, "tools/list too large: {serialized_len} bytes");

        let mut destructive = Vec::new();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let ann = tool["annotations"].as_object().expect("annotations required on every tool");
            assert_eq!(ann.get("openWorldHint").and_then(Value::as_bool), Some(false), "{name} must set openWorldHint=false");
            assert!(ann.get("readOnlyHint").and_then(Value::as_bool).is_some(), "{name} missing readOnlyHint");
            assert!(ann.get("idempotentHint").and_then(Value::as_bool).is_some(), "{name} missing idempotentHint");
            if ann.get("destructiveHint").and_then(Value::as_bool) == Some(true) {
                destructive.push(name);
            } else {
                assert_eq!(ann.get("destructiveHint").and_then(Value::as_bool), Some(false), "{name} missing destructiveHint=false");
            }
            if let Some(req) = tool.pointer("/inputSchema/required").and_then(Value::as_array) {
                let props = tool.pointer("/inputSchema/properties").and_then(Value::as_object).unwrap();
                for key in req {
                    let k = key.as_str().unwrap();
                    assert!(props.contains_key(k), "{name} required `{k}` missing from properties");
                }
            }
        }
        assert_eq!(destructive, ["delete_item"]);

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
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

        let post = call_rpc(
            &mut server,
            2,
            "post_update",
            json!({
                "project": "Alpha",
                "summary": "Changed: first pass",
                "branch": "feat/mcp",
                "screenshot_path": shot
            }),
        );
        assert_eq!(post.pointer("/result/isError"), Some(&json!(false)));
        let uid = post.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();
        assert_eq!(post.pointer("/result/structuredContent/update/branch"), Some(&json!("feat/mcp")));

        let feed = call_rpc(&mut server, 3, "get_feed", json!({ "project": "Alpha", "include_screenshots": true }));
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
        assert!(uris.contains(&"feeder://pending"));
        assert!(uris.contains(&"feeder://library"));
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

        let edited = call_rpc(&mut server, 6, "edit_update", json!({ "id": uid, "summary": "Changed: edited pass" }));
        assert_eq!(edited.pointer("/result/structuredContent/update/body"), Some(&json!("Changed: edited pass")));

        let deleted = call_rpc(&mut server, 7, "delete_item", json!({ "kind": "update", "id": uid }));
        assert_eq!(deleted.pointer("/result/structuredContent/deleted"), Some(&json!(uid)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_replies_brief_and_pulse_roundtrip() {
        let store_dir = temp_dir("store2");
        let work_dir = temp_dir("work2");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let setup_proj = call_rpc(
            &mut server,
            1,
            "update_project",
            json!({
                "project": "Beta",
                "brief": "Goal: fast local UI\nFocus: replies",
                "pulse": { "status": "Running clippy", "ttl_seconds": 90 }
            }),
        );
        assert_eq!(setup_proj.pointer("/result/structuredContent/project/brief"), Some(&json!("Goal: fast local UI\nFocus: replies")));
        assert_eq!(setup_proj.pointer("/result/structuredContent/pulse/status"), Some(&json!("Running clippy")));

        let post =
            call_rpc(&mut server, 3, "post_update", json!({ "project": "Beta", "summary": "Done: tests\n\nDecision needed: ship today?" }));
        let uid = post.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();

        let pending_before = call_rpc(&mut server, 31, "list_projects", json!({ "filter": "pending" }));
        assert_eq!(pending_before.pointer("/result/structuredContent/needs_you_updates/0/id"), Some(&json!(uid)));

        let store = Store::open(&store_dir).unwrap();
        let human_reply = store.add_reply(uid, "You", true, "Yes, ship today!").unwrap();

        let pending_res = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 32, "method": "resources/read", "params": { "uri": "feeder://pending" } }))
            .unwrap();
        let pending_md = pending_res.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
        assert!(pending_md.contains("Yes, ship today!"));

        let pending = call_rpc(&mut server, 4, "list_projects", json!({ "filter": "pending" }));
        assert_eq!(pending.pointer("/result/structuredContent/unread_replies/0/body"), Some(&json!("Yes, ship today!")));
        assert_eq!(pending.pointer("/result/structuredContent/needs_you_updates").and_then(Value::as_array).map(Vec::len), Some(0));

        let reply = call_rpc(&mut server, 6, "reply_to_update", json!({ "id": uid, "summary": "Shipping now.", "agent": "Codex" }));
        assert_eq!(reply.pointer("/result/structuredContent/reply/author"), Some(&json!("Codex")));
        assert_eq!(reply.pointer("/result/structuredContent/reply/from_human"), Some(&json!(false)));
        assert_eq!(reply.pointer("/result/structuredContent/marked_replies_read"), Some(&json!(1)));
        assert!(store.unread_human_replies(None).unwrap().is_empty());

        let legacy_mark = call_rpc(&mut server, 7, "mark_replies_read", json!({ "ids": [human_reply.id] }));
        assert_eq!(legacy_mark.pointer("/result/isError"), Some(&json!(false)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn single_roundtrip_post_update_and_todos_work_atomically() {
        let store_dir = temp_dir("store-single-rt");
        let work_dir = temp_dir("work-single-rt");
        let store = Store::open(&store_dir).unwrap();
        let gamma = store.project_named("Gamma", None).unwrap();
        let beta = store.project_named("Other", None).unwrap();
        let foreign_todo = store.add_todo(beta.id, "Foreign task").unwrap();
        let existing_todo = store.add_todo(gamma.id, "Ship release").unwrap();
        let page = store
            .create_page(
                feeder_core::PageDraft::new("SQLite busy timeout", "Set PRAGMA busy_timeout = 5000."),
                feeder_core::HUMAN_AUTHOR,
                None,
                feeder_core::OnSimilar::Refuse,
            )
            .unwrap();
        drop(store);

        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));
        let bad_post = call_rpc(
            &mut server,
            1,
            "post_update",
            json!({
                "project": "Gamma",
                "summary": "Done: tried partial mutation.",
                "complete_todos": [foreign_todo.id],
                "helped_notes": [page.id],
                "add_todos": ["Should not be created"]
            }),
        );
        assert_eq!(bad_post.pointer("/result/isError"), Some(&json!(true)));
        let store = Store::open(&store_dir).unwrap();
        assert!(store.feed(gamma.id, 10, None).unwrap().is_empty(), "pre-validation aborts before posting");
        assert_eq!(store.page(page.id).unwrap().unwrap().helped, 0);
        drop(store);

        let good_post = call_rpc(
            &mut server,
            2,
            "post_update",
            json!({
                "project": "Gamma",
                "summary": "Done: shipped release.",
                "complete_todos": [existing_todo.id],
                "add_todos": ["Write docs"],
                "helped_notes": [page.id],
                "flag_lesson": {
                    "kind": "missing",
                    "note": "Capture release verification order.",
                    "suggestion": "## Order\n\nRun checks after formatting."
                }
            }),
        );
        assert_eq!(good_post.pointer("/result/isError"), Some(&json!(false)));
        let uid = good_post.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();
        let fid = good_post.pointer("/result/structuredContent/flagged_lesson_id").and_then(Value::as_i64).unwrap();

        let store = Store::open(&store_dir).unwrap();
        assert_eq!(store.page(page.id).unwrap().unwrap().helped, 1);
        assert!(store.todo(existing_todo.id).unwrap().unwrap().done);
        let flag = store.flag(fid).unwrap().unwrap();
        assert_eq!(flag.source_update_id, Some(uid));

        let feed = call_rpc(&mut server, 3, "get_feed", json!({ "project": "Gamma" }));
        let feed_text = feed.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
        assert!(feed_text.contains("[x] Ship release"));
        assert!(feed_text.contains("[ ] Write docs"));

        let added_tid = good_post.pointer("/result/structuredContent/added_todos/0/id").and_then(Value::as_i64).unwrap();
        let deleted = call_rpc(&mut server, 4, "delete_item", json!({ "kind": "todo", "id": added_tid }));
        assert_eq!(deleted.pointer("/result/structuredContent/deleted"), Some(&json!(added_tid)));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_dismiss_attention_and_library_resources() {
        let store_dir = temp_dir("store-dismiss-lib");
        let work_dir = temp_dir("work-dismiss-lib");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let posted = call_rpc(
            &mut server,
            1,
            "post_update",
            json!({ "project": "Delta", "summary": "Done: built API.\n\nDecision needed: keep v1?" }),
        );
        let uid = posted.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();

        let feed_before = call_rpc(&mut server, 2, "get_feed", json!({ "project": "Delta" }));
        assert_eq!(feed_before.pointer("/result/structuredContent/needs_you"), Some(&json!(true)));

        let dismissed = call_rpc(&mut server, 3, "update_project", json!({ "project": "Delta", "dismiss_update_id": uid }));
        assert_eq!(dismissed.pointer("/result/isError"), Some(&json!(false)));

        let feed_after = call_rpc(&mut server, 4, "get_feed", json!({ "project": "Delta" }));
        assert_eq!(feed_after.pointer("/result/structuredContent/needs_you"), Some(&json!(false)));

        let store = Store::open(&store_dir).unwrap();
        let pid1 = store
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
        let pid2 = store
            .create_page(
                feeder_core::PageDraft::new("Rune layout padding rules", "Keep inset math inside style::pane_kit."),
                feeder_core::HUMAN_AUTHOR,
                None,
                feeder_core::OnSimilar::Refuse,
            )
            .unwrap()
            .id;

        for uri in ["feeder://library", "feeder://notebook"] {
            let index = server.handle(&json!({ "jsonrpc": "2.0", "id": 6, "method": "resources/read", "params": { "uri": uri } })).unwrap();
            let index_md = index.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
            assert!(index_md.contains("SQLite WAL checkpoints need busy timeout"));
        }

        for uri in [format!("feeder://library/{pid1}"), format!("feeder://notebook/{pid1}")] {
            let page_res =
                server.handle(&json!({ "jsonrpc": "2.0", "id": 7, "method": "resources/read", "params": { "uri": uri } })).unwrap();
            let page_md = page_res.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
            assert!(page_md.contains("PRAGMA busy_timeout = 3000"));
        }

        let batch = call_rpc(&mut server, 8, "read_library", json!({ "ids": [pid1, pid2] }));
        assert_eq!(batch.pointer("/result/structuredContent/pages").and_then(Value::as_array).map(Vec::len), Some(2));
        let batch_md = batch.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
        assert!(batch_md.contains("PRAGMA busy_timeout = 3000"));
        assert!(batch_md.contains("Keep inset math inside style::pane_kit."));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn progressive_disclosure_strips_bodies_and_scopes_library_search() {
        let store_dir = temp_dir("store-prog");
        let work_dir = temp_dir("work-prog");
        let store = Store::open(&store_dir).unwrap();
        let alpha = store.project_named("Alpha", None).unwrap();
        let beta = store.project_named("Beta", None).unwrap();
        let page = |title: &str, body: &str, project_id: Option<i64>| {
            store
                .create_page(
                    feeder_core::PageDraft { topic: Some("rust".into()), ..feeder_core::PageDraft::new(title, body) },
                    feeder_core::HUMAN_AUTHOR,
                    project_id,
                    feeder_core::OnSimilar::Allow,
                )
                .unwrap()
        };
        let local = page("Alpha SQLite lock", "SECRET_MARKER_BODY_ALPHA", Some(alpha.id));
        let shared = page("Shared SQLite lock", "SECRET_MARKER_BODY_SHARED", None);
        page("Beta SQLite lock", "SECRET_MARKER_BODY_BETA", Some(beta.id));

        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));
        let search = call_rpc(&mut server, 1, "search_library", json!({ "project": "Alpha", "topic": "rust" }));
        let pages = search.pointer("/result/structuredContent/pages").and_then(Value::as_array).unwrap();
        assert_eq!(pages.len(), 2);
        for hit in pages {
            assert!(hit.get("excerpt").is_some());
            assert!(hit.get("body").is_none(), "search_library must never leak full body into structuredContent");
        }
        assert!(pages.iter().any(|p| p["id"] == local.id));
        assert!(pages.iter().any(|p| p["id"] == shared.id));

        let handoff = call_rpc(&mut server, 2, "get_feed", json!({ "project": "Alpha", "task": "sqlite" }));
        let notes = handoff.pointer("/result/structuredContent/related_notes").and_then(Value::as_array).unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n["url"] == format!("feeder://library/{}", local.id)));
        assert!(!handoff.to_string().contains("Beta SQLite lock"));

        let run = store.start_gardener_run(feeder_core::gardener::Backend::Codex).unwrap();
        server.role = Role::Gardener(run);
        let curator_search = call_rpc(&mut server, 3, "search_notebook", json!({ "topic": "rust" }));
        let c_pages = curator_search.pointer("/result/structuredContent/pages").and_then(Value::as_array).unwrap();
        assert_eq!(c_pages.len(), 3);
        for hit in c_pages {
            assert!(hit.get("excerpt").is_some());
            assert!(hit.get("body").is_none(), "curator search_notebook must strip body too");
        }

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn mcp_carousel_and_read_library_single_page_roundtrip() {
        let store_dir = temp_dir("store-carousel-md");
        let work_dir = temp_dir("work-carousel-md");
        let shot1 = work_dir.join("a.png");
        let shot2 = work_dir.join("b.png");
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        std::fs::write(&shot1, png).unwrap();
        std::fs::write(&shot2, png).unwrap();

        let mut server = Server::new(Some("claude-code".into()), work_dir.clone(), Some(store_dir.clone()));
        let posted = call_rpc(
            &mut server,
            1,
            "post_update",
            json!({
                "project": "Orbit",
                "summary": "Done: redesigned onboarding flow and settings drawer.",
                "media": [shot1, shot2]
            }),
        );
        assert_eq!(posted.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(posted.pointer("/result/structuredContent/update/media").and_then(Value::as_array).map(Vec::len), Some(2));

        let feed = call_rpc(&mut server, 2, "get_feed", json!({ "project": "Orbit", "include_screenshots": true }));
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

        let page = call_rpc(&mut server, 4, "read_library", json!({ "id": pid }));
        assert_eq!(page.pointer("/result/structuredContent/topic"), Some(&json!("rust")));
        assert!(
            page.pointer("/result/structuredContent/body").and_then(Value::as_str).is_some_and(|b| b.contains("Verify rollback cleanup"))
        );

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
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
        let names: Vec<_> = listed["result"]["tools"].as_array().unwrap().iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "list_projects",
                "get_feed",
                "search_updates",
                "post_update",
                "reply_to_update",
                "edit_update",
                "update_project",
                "delete_item",
                "search_library",
                "read_library",
                "flag_knowledge",
            ]
        );

        let read = server.call(&json!({ "name": "read_page", "arguments": { "id": page.id } }));
        let helped = server.call(&json!({ "name": "mark_page_helped", "arguments": { "id": page.id } }));
        let write = server
            .call(&json!({ "name": "write_page", "arguments": { "id": page.id, "title": page.title, "body": "Overwrite this note." } }));
        let archive = server.call(&json!({ "name": "archive_page", "arguments": { "id": page.id } }));
        let current = store.page(page.id).unwrap().unwrap();
        drop(server);
        drop(store);
        std::fs::remove_dir_all(store_dir).unwrap();

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
    fn security_hardening_guards_cwd_typos_media_cap_batching_and_db_reload() {
        let store_dir = temp_dir("store-sec");
        let non_git_dir = temp_dir("non-git-sec");
        let store = Store::open(&store_dir).unwrap();
        let existing = store.project_named("Orbit", None).unwrap();
        drop(store);

        let mut server = Server::new(Some("codex".into()), non_git_dir.clone(), Some(store_dir.clone()));

        let bogus = call_rpc(&mut server, 1, "post_update", json!({ "summary": "Done: should not create project from non-git cwd." }));
        assert_eq!(bogus.pointer("/result/isError"), Some(&json!(true)));
        let err_msg = bogus.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
        assert!(err_msg.contains("Orbit"), "error suggests active projects: {err_msg}");

        let typo = call_rpc(&mut server, 2, "post_update", json!({ "project": "Orbit", "summary": "Done: x", "unknown_key": 1 }));
        assert_eq!(typo.pointer("/result/isError"), Some(&json!(true)));
        assert!(typo.to_string().contains("unknown field"));

        let bad_type = call_rpc(&mut server, 3, "read_library", json!({ "id": "42" }));
        assert_eq!(bad_type.pointer("/result/isError"), Some(&json!(true)));

        let ro_guard = call_rpc(&mut server, 4, "get_feed", json!({ "project": "Orbit", "mark_replies_read": true }));
        assert_eq!(ro_guard.pointer("/result/isError"), Some(&json!(true)));
        assert!(ro_guard.to_string().contains("strictly read-only"));

        let bad_path =
            call_rpc(&mut server, 5, "post_update", json!({ "project_path": non_git_dir.join("missing-folder"), "summary": "Done: nope" }));
        assert_eq!(bad_path.pointer("/result/isError"), Some(&json!(true)));

        let unknown = call_rpc(&mut server, 6, "no_such_tool", json!({}));
        assert_eq!(unknown.pointer("/error/code"), Some(&json!(-32602)));

        let big1 = non_git_dir.join("big1.png");
        let big2 = non_git_dir.join("big2.png");
        let mut big_png = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
        big_png.resize(3 * 1024 * 1024, 0u8);
        std::fs::write(&big1, &big_png).unwrap();
        std::fs::write(&big2, &big_png).unwrap();
        let git_proj = temp_dir("git-media-cap");
        init_git(&git_proj);
        server.cwd = git_proj.clone();
        let posted_media = call_rpc(&mut server, 7, "post_update", json!({ "summary": "Done: two large PNGs.", "media": [big1, big2] }));
        assert_eq!(posted_media.pointer("/result/isError"), Some(&json!(false)));
        let cap_feed = call_rpc(&mut server, 8, "get_feed", json!({ "include_screenshots": true }));
        let content_blocks = cap_feed.pointer("/result/content").and_then(Value::as_array).unwrap();
        assert_eq!(content_blocks.len(), 2, "4 MiB cumulative cap allows first 3 MiB PNG and skips second 3 MiB PNG");

        let init_new = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 9, "method": "initialize", "params": { "protocolVersion": "2025-11-25" } }))
            .unwrap();
        assert_eq!(init_new.pointer("/result/protocolVersion"), Some(&json!("2025-11-25")));
        assert!(!supports_batching(&server.protocol_version));

        let init_old = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 10, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }))
            .unwrap();
        assert_eq!(init_old.pointer("/result/protocolVersion"), Some(&json!("2025-03-26")));
        assert!(supports_batching(&server.protocol_version));

        assert!(call_rpc(&mut server, 11, "list_projects", json!({})).pointer("/result/structuredContent/projects/0").is_some());
        drop(server.store.borrow_mut().take());
        let fresh_store_dir = temp_dir("store-sec-replace");
        let fresh = Store::open(&fresh_store_dir).unwrap();
        fresh.project_named("ReplacedProject", None).unwrap();
        drop(fresh);
        let _ = Store::open(&store_dir).unwrap();
        let old_handle = server.open_store().is_ok();
        assert!(old_handle);
        drop(server.store.borrow_mut().take());
        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::create_dir_all(&store_dir);
        let fresh_in_place = Store::open(&store_dir).unwrap();
        fresh_in_place.project_named("ReopenedProject", None).unwrap();
        drop(fresh_in_place);
        let after_replace = call_rpc(&mut server, 12, "list_projects", json!({}));
        let names_after: Vec<_> = after_replace
            .pointer("/result/structuredContent/projects")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert!(names_after.contains(&"ReopenedProject"));
        assert!(!names_after.contains(&existing.name.as_str()));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(fresh_store_dir);
        let _ = std::fs::remove_dir_all(non_git_dir);
        let _ = std::fs::remove_dir_all(git_proj);
    }

    #[test]
    fn empty_searches_and_flag_knowledge_file_flags_for_the_gardener() {
        let store_dir = temp_dir("store-flags");
        let work_dir = temp_dir("work-flags");
        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        for id in 1..=2 {
            let miss = call_rpc(&mut server, id, "search_library", json!({ "query": "notarize the dmg" }));
            let text = miss.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
            assert!(text.contains("flag_knowledge"), "the empty result asks for a flag: {text}");
        }
        let flagged = call_rpc(
            &mut server,
            3,
            "flag_knowledge",
            json!({ "kind": "wrong", "note": "The release page says release.sh is in the root", "suggestion": "It moved to scripts/" }),
        );
        assert_eq!(flagged.pointer("/result/structuredContent/kind"), Some(&json!("wrong")));
        let no_suggestion = call_rpc(&mut server, 4, "flag_knowledge", json!({ "note": "No instructions for rotating the appcast key" }));
        assert_eq!(no_suggestion.pointer("/result/isError"), Some(&json!(false)));

        let store = Store::open(&store_dir).unwrap();
        let open = store.flags(Some(feeder_core::FlagStatus::Open), 10).unwrap();
        assert_eq!(open.len(), 2, "only the two explicit flags; empty searches must not file flags");
        assert!(!open.iter().any(|f| f.note.contains("notarize the dmg")));
        assert!(open.iter().any(|f| f.suggestion.as_deref() == Some("It moved to scripts/")));

        let refused = call_rpc(&mut server, 5, "flag_knowledge", json!({ "note": "no" }));
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

        let listed = server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).unwrap();
        let tools = listed.pointer("/result/tools").and_then(Value::as_array).unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, gardener::tool_names(feeder_core::gardener::SessionMode::Review), "only the gardener's tools");
        for t in tools {
            assert_eq!(t.pointer("/annotations/openWorldHint").and_then(Value::as_bool), Some(false));
        }

        let post = call_rpc(&mut server, 2, "post_update", json!({ "project": "Alpha", "summary": "Done: x" }));
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
        let unsourced = call_rpc(
            &mut server,
            3,
            "write_page",
            json!({ "title": "Release checks", "body": "Run the changelog check.", "reason": "Useful" }),
        );
        assert_eq!(unsourced.pointer("/result/isError"), Some(&json!(true)));
        let sourced = call_rpc(
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
            let ok = call_rpc(
                &mut server,
                n,
                "write_page",
                json!({ "title": title, "body": "Do the step.", "reason": "Flagged", "flags": [f] }),
            );
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
        let over = call_rpc(
            &mut server,
            20,
            "write_page",
            json!({ "title": "One more page", "body": "Body.", "reason": "Flagged", "flags": [f] }),
        );
        assert_eq!(over.pointer("/result/isError"), Some(&json!(true)), "the sixth change is refused");

        let missing = call_rpc(&mut server, 21, "search_notebook", json!({ "query": "zzz nothing" }));
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
        assert_eq!(names, ["search_notebook", "read_page", "list_flags", "read_traces"]);
        let call = |name: &str, arguments: Value| server.call(&json!({ "name": name, "arguments": arguments }));
        for query in [json!("deployment"), Value::Null] {
            let result = call("search_notebook", json!({ "query": query }));
            let pages = result.pointer("/structuredContent/pages").unwrap().as_array().unwrap();
            assert_eq!(pages.len(), 2);
            assert!(pages.iter().any(|page| page["id"] == own_id));
            assert!(pages.iter().any(|page| page["id"] == shared_id));
        }
        assert_eq!(call("read_page", json!({ "id": own_id }))["isError"], false);
        assert_eq!(call("read_page", json!({ "id": shared_id }))["isError"], false);
        assert_eq!(call("read_page", json!({ "id": other_id }))["isError"], true);
        assert_eq!(call("list_flags", json!({}))["isError"], false);
        assert_eq!(call("read_traces", json!({}))["isError"], false);
        assert_eq!(call("read_traces", json!({ "project": "Beta" }))["isError"], true);
        for tool in ["write_page", "archive_page", "dismiss_flag"] {
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
        use feeder_core::{NewUpdate, Project};

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
        let call = |name: &str, arguments: Value| gardener.call(&json!({ "name": name, "arguments": arguments }));

        let flags = call("list_flags", json!({}));
        assert_eq!(flags.pointer("/structuredContent/flags/0/source_update_id"), Some(&json!(update.id)));
        assert!(flags.pointer("/structuredContent/flags/1").is_none());
        let traces = call("read_traces", json!({}));
        assert_eq!(traces.pointer("/structuredContent/traces/0/replies/0/body"), Some(&json!(correction)));
        assert!(traces.pointer("/structuredContent/traces/1").is_none());
        let other_trace = call("read_traces", json!({ "update_id": unrelated.id }));
        assert_eq!(other_trace["isError"], true);
        let refused = call("dismiss_flag", json!({ "id": other_flag.id, "reason": "Wrong scope" }));
        assert_eq!(refused["isError"], true);

        let title = "When to run the appcast check";
        let write = |flags: Vec<i64>, updates: Vec<i64>| {
            call(
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
        let lookup = worker.call(&json!({ "name": "search_library", "arguments": { "query": "appcast check" } }));
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
        let lookup = worker.call(&json!({ "name": "search_library", "arguments": { "query": "appcast check" } }));
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
                let out = call_rpc(
                    &mut server,
                    id,
                    "write_page",
                    json!({ "title": title, "body": "Do it.", "reason": "Flagged", "flags": [flag] }),
                );
                if out.pointer("/result/isError") == Some(&json!(true)) {
                    refused += 1;
                }
            }
        }
        assert_eq!(refused, 1, "five changes per session, however many servers it starts");
        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn wave_b_shared_notes_brief_guard_replies_and_git_meta_scoping() {
        let store_dir = temp_dir("store-wave-b");
        let work_dir = temp_dir("work-wave-b");
        std::fs::create_dir_all(&work_dir).unwrap();
        init_git(&work_dir);
        let git_ok = std::process::Command::new("git")
            .args(["checkout", "-q", "-b", "feature/cwd-branch"])
            .current_dir(&work_dir)
            .status()
            .unwrap()
            .success();
        assert!(git_ok);

        let shot = work_dir.join("screen.png");
        std::fs::write(&shot, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();

        let mut server = Server::new(Some("codex".into()), work_dir.clone(), Some(store_dir.clone()));

        let shared_flag =
            call_rpc(&mut server, 1, "flag_knowledge", json!({ "shared": true, "note": "Keep release scripts reproducible" }));
        assert_eq!(shared_flag.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(shared_flag.pointer("/result/structuredContent/project_id"), Some(&Value::Null));

        let conflict = call_rpc(
            &mut server,
            2,
            "flag_knowledge",
            json!({ "shared": true, "project": "Other", "note": "Should conflict with project" }),
        );
        assert_eq!(conflict.pointer("/result/isError"), Some(&json!(true)));

        let posted = call_rpc(
            &mut server,
            3,
            "post_update",
            json!({
                "project": "UnlinkedRepo",
                "summary": "Done: Shipped clean APIs\nWhy: Keep git meta scoped\nNext: Add tests",
                "screenshot_path": "screen.png",
                "screenshots": ["screen.png"]
            }),
        );
        assert_eq!(posted.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(posted.pointer("/result/structuredContent/update/branch"), Some(&Value::Null));
        assert_eq!(posted.pointer("/result/structuredContent/update/media").and_then(Value::as_array).map(Vec::len), Some(1));
        let update_id = posted.pointer("/result/structuredContent/update/id").and_then(Value::as_i64).unwrap();

        let reply_res = call_rpc(&mut server, 4, "reply_to_update", json!({ "id": update_id, "summary": "Verified!" }));
        assert_eq!(reply_res.pointer("/result/isError"), Some(&json!(false)));

        let found = call_rpc(&mut server, 5, "search_updates", json!({ "query": "clean APIs" }));
        assert_eq!(found.pointer("/result/structuredContent/updates/0/replies/0/body"), Some(&json!("Verified!")));

        let empty_brief = call_rpc(&mut server, 6, "update_project", json!({ "project": "UnlinkedRepo", "brief": "  " }));
        assert_eq!(empty_brief.pointer("/result/isError"), Some(&json!(true)));

        let store = Store::open(&store_dir).unwrap();
        let proj = store.project_named("UnlinkedRepo", None).unwrap();
        let run = store
            .start_curator_run(
                feeder_core::gardener::Backend::Codex,
                feeder_core::gardener::Scope::Notebook,
                feeder_core::gardener::SessionMode::Review,
            )
            .unwrap();
        let f = store
            .flag_knowledge(
                feeder_core::NewFlag {
                    kind: feeder_core::FlagKind::Missing,
                    page_id: None,
                    project_id: Some(proj.id),
                    note: "Cross-project release note".into(),
                    suggestion: None,
                },
                "Codex",
            )
            .unwrap();
        drop(store);
        server.role = Role::Gardener(run);
        let wrote = call_rpc(
            &mut server,
            7,
            "write_page",
            json!({
                "title": "Shared CI checklist",
                "body": "Always verify release builds cleanly.",
                "shared": true,
                "reason": "Cross-project rule",
                "flags": [f.id]
            }),
        );
        assert_eq!(wrote.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(wrote.pointer("/result/structuredContent/page/project"), Some(&Value::Null));

        let _ = std::fs::remove_dir_all(store_dir);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
