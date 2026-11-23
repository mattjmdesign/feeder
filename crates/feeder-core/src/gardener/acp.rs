use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::process::ExitStatus;

use serde_json::{Value, json};

use super::{Backend, Event, SessionMode, ToolCall};
use crate::{Error, Result};

pub(super) fn check_version(backend: Backend, status: Option<ExitStatus>, version: &str) -> Result<()> {
    let minimum = match backend {
        Backend::OpenCode => (2, 3, 2),
        Backend::Antigravity => (1, 5, 5),
        _ => return Err(Error::Invalid("invalid ACP backend".into())),
    };
    if !status.is_some_and(|status| status.success()) || !supported_version(version, minimum) {
        let (major, minor, patch) = minimum;
        return Err(Error::Invalid(format!("Update {} to {major}.{minor}.{patch} or a later {major}.x release.", backend.label())));
    }
    Ok(())
}

pub(super) fn command(
    backend: Backend,
    binary: &Path,
    data: &Path,
    cwd: &Path,
    mode: SessionMode,
    server: &Value,
    run: &super::RunConfig,
) -> Result<Command> {
    let mut cmd = super::profile::command(backend, binary, data)?;
    cmd.current_dir(cwd).arg("acp");
    match backend {
        Backend::OpenCode => {
            let config = opencode_config(mode, run.model.as_deref(), server);
            std::fs::write(cwd.join("opencode.json"), config.to_string())?;
            let config_dir = cwd.join("config");
            std::fs::create_dir_all(&config_dir)?;
            cmd.env("XDG_CONFIG_HOME", &config_dir)
                .env("OPENCODE_CONFIG_DIR", &config_dir)
                .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "1");
        }
        Backend::Antigravity => {
            configure_antigravity(data, mode, server)?;
            cmd.arg("--gemini-dir").arg(super::profile::gemini_dir(data)?);
            cmd.args(["--include-default-customizations=false", "--agent", "feeder-curator", "--sandbox", "--disable-telemetry"]);
        }
        _ => unreachable!("validated ACP backend"),
    }
    Ok(cmd)
}

fn supported_version(stdout: &str, minimum: (u32, u32, u32)) -> bool {
    let parse = |word: &str| {
        let mut parts = word.trim_start_matches('v').split('.');
        let version = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        if parts.next().is_none() { Some(version) } else { None }
    };
    stdout.split_whitespace().find_map(parse).is_some_and(|version| version.0 == minimum.0 && version >= minimum)
}

fn opencode_config(mode: SessionMode, model: Option<&str>, server: &Value) -> Value {
    let mut permissions = vec![json!({ "action": "*", "resource": "*", "effect": "deny" })];
    let mut policies = vec![
        policy("tool.use", "*", "deny"),
        policy("mcp.use", "*", "deny"),
        policy("mcp.use", "feeder", "allow"),
        policy("plugin.use", "*", "deny"),
        policy("skill.use", "*", "deny"),
        policy("websearch.use", "*", "deny"),
    ];
    for name in mode.mcp_tools() {
        let action = format!("feeder_{name}");
        policies.push(policy("tool.use", &format!("{action}:*"), "allow"));
        permissions.push(policy(&action, "*", "allow"));
    }
    let mut mcp = serde_json::Map::new();
    for inherited in inherited_opencode_mcp_names() {
        if inherited != "feeder" {
            mcp.insert(inherited, json!({ "type": "local", "command": ["true"], "enabled": false }));
        }
    }
    if let Some(bin) = server["command"].as_str() {
        let mut command = vec![Value::String(bin.to_string())];
        if let Some(args) = server["args"].as_array() {
            command.extend(args.iter().cloned());
        }
        let mut environment = serde_json::Map::new();
        if let Some(envs) = server["env"].as_array() {
            for entry in envs {
                if let (Some(k), Some(v)) = (entry["name"].as_str(), entry["value"].as_str()) {
                    environment.insert(k.to_string(), Value::String(v.to_string()));
                }
            }
        }
        mcp.insert(
            "feeder".into(),
            json!({
                "type": "local",
                "command": command,
                "environment": environment,
                "enabled": true,
            }),
        );
    }
    let mut config = json!({
        "default_agent": "feeder-curator",
        "agents": { "feeder-curator": { "description": "Feeder Curator", "mode": "primary", "system": mode.instructions() } },
        "mcp": mcp,
        "update": "disable",
        "permissions": permissions,
        "policies": policies,
    });
    if let Some(model) = model {
        config["model"] = json!(model);
        config["agents"]["feeder-curator"]["model"] = json!(model);
    }
    config
}

fn inherited_opencode_mcp_names() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let path = std::path::PathBuf::from(home).join(".config/opencode/opencode.json");
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else { return Vec::new() };
    let Some(mcp) = parsed["mcp"].as_object() else { return Vec::new() };
    let mut names = Vec::new();
    for (key, val) in mcp {
        if key == "servers" {
            if let Some(servers) = val.as_object() {
                names.extend(servers.keys().cloned());
            }
        } else {
            names.push(key.clone());
        }
    }
    names
}

fn policy(action: &str, resource: &str, effect: &str) -> Value {
    json!({ "action": action, "resource": resource, "effect": effect })
}

fn configure_antigravity(data: &Path, mode: SessionMode, server: &Value) -> Result<()> {
    let home = super::profile::gemini_dir(data)?;
    let profile = home.join("antigravity-cli");
    std::fs::create_dir_all(&profile)?;
    let file = profile.join("settings.json");
    let mut settings: Value = match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("Curator's Antigravity settings are unreadable".into()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.into()),
    };
    let fields = settings.as_object_mut().ok_or_else(|| Error::Invalid("Curator's Antigravity settings are unreadable".into()))?;
    fields.insert("agentMode".into(), json!(""));
    fields.insert("allowNonWorkspaceAccess".into(), json!(false));
    fields.insert("toolPermission".into(), json!("request-review"));
    fields.insert("useG1Credits".into(), json!(false));
    fields.insert(
        "permissions".into(),
        json!({
            "allow": ["mcp(feeder/*)"],
            "deny": ["command(*)", "read_file(*)", "write_file(*)", "read_url(*)", "execute_url(*)", "unsandboxed(*)"]
        }),
    );
    std::fs::write(file, settings.to_string())?;
    let agents = profile.join("agents");
    std::fs::create_dir_all(&agents)?;
    let agent = format!(
        "---\nname: feeder-curator\ndescription: Feeder Notebook Curator\ntools: []\nmainAgent: true\nsubagent: false\ncommandExecutionPolicy: off\n---\n{}",
        mode.instructions()
    );
    std::fs::write(agents.join("feeder-curator.md"), agent)?;
    let config = home.join("config");
    std::fs::create_dir_all(&config)?;
    let tools: serde_json::Map<String, Value> = mode.mcp_tools().iter().map(|&name| (name.into(), json!({ "eager": true }))).collect();
    let feeder_home = &server["env"][0]["value"];
    let mcp = json!({
        "mcpServers": { "feeder": {
            "command": server["command"], "args": server["args"], "env": { "FEEDER_HOME": feeder_home },
            "enabledTools": mode.mcp_tools(), "tools": tools, "timeoutSeconds": 30,
        }}
    });
    std::fs::write(config.join("mcp_config.json"), mcp.to_string())?;
    Ok(())
}

#[derive(Debug)]
pub(super) enum Action {
    Send(Value),
    Emit(Event),
}

#[derive(Default)]
pub(super) struct State {
    session: Option<String>,
    pending: Option<u64>,
    pending_model: Option<u64>,
    pending_effort: Option<u64>,
    next: u64,
    tools: HashSet<String>,
    pending_tools: HashMap<String, &'static str>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
}

impl State {
    pub(super) fn initialize() -> Value {
        request(
            1,
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {
            "name": "feeder", "version": env!("CARGO_PKG_VERSION"),
        } }),
        )
    }

    fn next_id(&mut self) -> u64 {
        self.next = self.next.saturating_add(1).max(3);
        self.next
    }

    pub(super) fn prompt(&mut self, text: &str) -> Result<Value> {
        let id = self.next.saturating_add(1).max(3);
        let session = self.session.as_deref().ok_or_else(|| Error::Invalid("Curator is still connecting".into()))?;
        if self.pending.is_some() {
            return Err(Error::Invalid("Curator is still answering".into()));
        }
        let req = request(id, "session/prompt", json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }));
        self.next = id;
        self.pending = Some(id);
        Ok(req)
    }

    pub(super) fn ready(&self) -> bool {
        self.session.is_some() && self.pending.is_none()
    }

    pub(super) fn receive(&mut self, message: Value, mode: SessionMode, cwd: &Path, server: &Value, opening: &str) -> Result<Vec<Action>> {
        if let Some(method) = message["method"].as_str() {
            if let Some(id) = message.get("id") {
                return Ok(vec![Action::Send(handle_request(id, method, &message["params"], self.session.as_deref(), mode))]);
            }
            let session = message["params"]["sessionId"].as_str();
            if method == "session/update" && session.is_some() && session == self.session.as_deref() {
                return Ok(self.update(&message["params"]["update"], mode));
            }
            return Ok(Vec::new());
        }
        let Some(id) = message["id"].as_u64() else { return Ok(Vec::new()) };
        if self.pending_model == Some(id) {
            self.pending_model = None;
            return check_config_response(&message);
        }
        if self.pending_effort == Some(id) {
            self.pending_effort = None;
            return check_config_response(&message);
        }
        if !matches!(id, 1 | 2) && self.pending != Some(id) {
            return Ok(Vec::new());
        }
        if let Some(error) = message.get("error") {
            let detail = error["message"].as_str().unwrap_or("The provider returned an ACP error");
            return Err(Error::Invalid(detail.to_string()));
        }
        match id {
            1 => {
                if message["result"]["protocolVersion"] != 1 {
                    return Err(Error::Invalid("The provider doesn't support ACP protocol version 1".into()));
                }
                Ok(vec![Action::Send(request(2, "session/new", json!({ "cwd": cwd, "mcpServers": server })))])
            }
            2 => {
                let session = message["result"]["sessionId"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
                    .ok_or_else(|| Error::Invalid("The provider did not return a valid session ID".into()))?;
                if self.session.is_some() {
                    return Err(Error::Invalid("The provider returned a duplicate session".into()));
                }
                self.session = Some(session.to_string());
                let config_options = message["result"]["configOptions"].as_array();
                let mut actions = vec![Action::Emit(Event::Started)];
                if let Some(model) = self.model.take() {
                    let id = self.next_id();
                    self.pending_model = Some(id);
                    if let Some(config_id) = config_option_id(config_options, "model", &["model"]) {
                        actions.push(Action::Send(request(
                            id,
                            "session/set_config_option",
                            json!({ "sessionId": session, "configId": config_id, "value": model }),
                        )));
                    } else {
                        actions.push(Action::Send(request(id, "session/set_model", json!({ "sessionId": session, "modelId": model }))));
                    }
                }
                if let Some(effort) = self.effort.take() {
                    let config_id = match config_options {
                        Some(_) => config_option_id(config_options, "thought_level", &["effort", "thinking_level"]),
                        None => Some("effort"),
                    };
                    if let Some(config_id) = config_id {
                        let id = self.next_id();
                        self.pending_effort = Some(id);
                        actions.push(Action::Send(request(
                            id,
                            "session/set_config_option",
                            json!({ "sessionId": session, "configId": config_id, "value": effort }),
                        )));
                    }
                }
                actions.push(Action::Send(self.prompt(opening)?));
                Ok(actions)
            }
            _ => {
                let reason = message["result"]["stopReason"].as_str().unwrap_or_default();
                if !matches!(reason, "end_turn" | "max_tokens" | "max_turn_requests" | "cancelled" | "refusal") {
                    return Err(Error::Invalid("The provider did not complete the Curator turn".into()));
                }
                self.pending = None;
                self.tools.clear();
                self.pending_tools.clear();
                Ok(vec![Action::Emit(Event::TurnFinished)])
            }
        }
    }

    fn update(&mut self, update: &Value, mode: SessionMode) -> Vec<Action> {
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let Some(text) = update.pointer("/content/text").and_then(Value::as_str) else { return Vec::new() };
                let event = if kind == "agent_message_chunk" { Event::Text(text.into()) } else { Event::Thinking(text.into()) };
                vec![Action::Emit(event)]
            }
            "tool_call" | "tool_call_update" => self.tool_update(update, mode),
            _ => Vec::new(),
        }
    }

    fn tool_update(&mut self, update: &Value, mode: SessionMode) -> Vec<Action> {
        let Some(id) = update["toolCallId"].as_str() else { return Vec::new() };
        let status = update["status"].as_str().unwrap_or_default();
        let raw_input = &update["rawInput"];
        let empty_input = raw_input.is_null() || raw_input.as_object().is_some_and(serde_json::Map::is_empty);
        let mut actions = Vec::new();
        if !self.tools.contains(id) {
            let name = tool_name(update, mode).or_else(|| self.pending_tools.get(id).copied());
            if let Some(name) = name {
                if status == "pending" && empty_input {
                    self.pending_tools.insert(id.to_string(), name);
                    return actions;
                }
                self.pending_tools.remove(id);
                self.tools.insert(id.to_string());
                actions.push(Action::Emit(Event::ToolStarted(ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: raw_input.clone(),
                })));
            }
        }
        if matches!(status, "completed" | "failed") && self.tools.remove(id) {
            self.pending_tools.remove(id);
            let output = update["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item.pointer("/content/text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            let output = if output.is_empty() && !update["rawOutput"].is_null() {
                update["rawOutput"].as_str().map_or_else(|| update["rawOutput"].to_string(), str::to_string)
            } else {
                output
            };
            actions.push(Action::Emit(Event::ToolFinished { id: id.into(), output, failed: status == "failed" }));
        }
        actions
    }
}

fn check_config_response(message: &Value) -> Result<Vec<Action>> {
    if let Some(error) = message.get("error") {
        if error["code"].as_i64() == Some(-32601) {
            return Ok(Vec::new());
        }
        let detail = error["message"].as_str().unwrap_or("The provider rejected the Curator model or thinking setting");
        return Err(Error::Invalid(detail.to_string()));
    }
    Ok(Vec::new())
}

fn tool_name(call: &Value, mode: SessionMode) -> Option<&'static str> {
    ["name", "title"].into_iter().filter_map(|key| call[key].as_str()).find_map(|name| {
        let name = ["feeder_", "mcp__feeder__", "mcp_feeder_"].into_iter().find_map(|prefix| name.strip_prefix(prefix)).unwrap_or(name);
        mode.mcp_tools().iter().copied().find(|&tool| tool == name)
    })
}

fn allowed_feeder_permission(params: &Value, active_session: Option<&str>, mode: SessionMode) -> bool {
    if active_session.is_some() && params["sessionId"].as_str() != active_session {
        return false;
    }
    let call = &params["toolCall"];
    if !matches!(call["kind"].as_str(), None | Some("other")) {
        return false;
    }
    let Some(raw_name) = call["name"].as_str() else { return false };
    let Some(stripped) = ["feeder_", "mcp__feeder__", "mcp_feeder_"].into_iter().find_map(|prefix| raw_name.strip_prefix(prefix)) else {
        return false;
    };
    mode.mcp_tools().contains(&stripped)
}

fn handle_request(id: &Value, method: &str, params: &Value, active_session: Option<&str>, mode: SessionMode) -> Value {
    if method == "session/request_permission" {
        let options = params["options"].as_array();
        let find_option =
            |kind: &str| options.into_iter().flatten().find(|option| option["kind"] == kind).and_then(|option| option["optionId"].as_str());
        let selected =
            if allowed_feeder_permission(params, active_session, mode) { find_option("allow_once") } else { find_option("reject_once") };
        let outcome = match selected {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "This Curator client does not offer this capability" } })
    }
}

fn config_option_id<'a>(options: Option<&'a Vec<Value>>, category: &str, ids: &[&str]) -> Option<&'a str> {
    options?.iter().find_map(|option| {
        let id = option["id"].as_str()?;
        let matches_category = option["category"].as_str() == Some(category);
        let matches_id = ids.contains(&id);
        (matches_category || matches_id).then_some(id)
    })
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_preserves_turn_boundaries_and_the_notebook_tool_trace() {
        let mut state = State::default();
        let cwd = Path::new("/tmp/feeder");
        let tools = json!([{ "name": "feeder" }]);
        let mode = SessionMode::Ask;
        let open = "Show the runbook";
        let init = state.receive(json!({ "id": 1, "result": { "protocolVersion": 1 } }), mode, cwd, &tools, open).unwrap();
        assert!(matches!(init.as_slice(), [Action::Send(m)] if m["params"]["mcpServers"] == tools));
        let started = state.receive(json!({ "id": 2, "result": { "sessionId": "sample" } }), mode, cwd, &tools, open).unwrap();
        assert!(matches!(started.as_slice(), [Action::Emit(Event::Started), Action::Send(m)] if m["id"] == 3));
        assert!(state.prompt("Too early").is_err());
        let update = json!({ "method": "session/update", "params": { "sessionId": "sample", "update": {
            "sessionUpdate": "tool_call", "name": "feeder_read_page", "toolCallId": "p", "rawInput": { "id": 1 },
            "status": "completed", "content": [{ "type": "content", "content": { "type": "text", "text": "Runbook" } }]
        } } });
        let actions = state.receive(update, mode, cwd, &tools, open).unwrap();
        assert!(matches!(actions.as_slice(), [
                Action::Emit(Event::ToolStarted(call)), Action::Emit(Event::ToolFinished { output, failed: false, .. })
            ] if call.name == "read_page" && output == "Runbook"));
        let done = state.receive(json!({ "id": 3, "result": { "stopReason": "end_turn" } }), mode, cwd, &tools, open).unwrap();
        assert!(matches!(done.as_slice(), [Action::Emit(Event::TurnFinished)]));
        assert_eq!(state.prompt("Now what?").unwrap()["id"], 4);
    }

    #[test]
    fn acp_refuses_local_client_capabilities_and_uses_exact_notebook_permissions() {
        let options = json!([{ "kind": "allow_once", "optionId": "allow" }, { "kind": "reject_once", "optionId": "reject" }]);
        let shell = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "s", "toolCall": { "name": "bash", "kind": "execute" }, "options": options }),
            Some("s"),
            SessionMode::Review,
        );
        assert_eq!(shell.pointer("/result/outcome/optionId"), Some(&json!("reject")));
        let spoofed_title = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "s", "toolCall": { "name": "bash", "title": "feeder_read_page", "kind": "other" }, "options": options }),
            Some("s"),
            SessionMode::Review,
        );
        assert_eq!(spoofed_title.pointer("/result/outcome/optionId"), Some(&json!("reject")));
        let write_in_ask = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "s", "toolCall": { "name": "mcp_feeder_write_page", "kind": "other" }, "options": options }),
            Some("s"),
            SessionMode::Ask,
        );
        assert_eq!(write_in_ask.pointer("/result/outcome/optionId"), Some(&json!("reject")));
        let wrong_session = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "other", "toolCall": { "name": "mcp_feeder_search_notebook", "kind": "other" }, "options": options }),
            Some("s"),
            SessionMode::Ask,
        );
        assert_eq!(wrong_session.pointer("/result/outcome/optionId"), Some(&json!("reject")));
        let allowed_ask = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "s", "toolCall": { "name": "mcp_feeder_search_notebook", "kind": "other" }, "options": options }),
            Some("s"),
            SessionMode::Ask,
        );
        assert_eq!(allowed_ask.pointer("/result/outcome/optionId"), Some(&json!("allow")));
        let allowed_review = handle_request(
            &json!(7),
            "session/request_permission",
            &json!({ "sessionId": "s", "toolCall": { "name": "mcp_feeder_write_page", "kind": "other" }, "options": options }),
            Some("s"),
            SessionMode::Review,
        );
        assert_eq!(allowed_review.pointer("/result/outcome/optionId"), Some(&json!("allow")));
        assert_eq!(
            handle_request(&json!(7), "fs/read_text_file", &json!({}), Some("s"), SessionMode::Ask).pointer("/error/code"),
            Some(&json!(-32601))
        );
        let server = json!({
            "name": "feeder",
            "command": "/usr/local/bin/feeder",
            "args": ["mcp", "--gardener", "--gardener-run", "7"],
            "env": [{ "name": "FEEDER_HOME", "value": "/tmp/feeder-home" }],
        });
        let ask = opencode_config(SessionMode::Ask, None, &server);
        let review = opencode_config(SessionMode::Review, Some("openai/gpt-6-sol"), &server);
        let grants = |config: &Value, action: &str| {
            config["permissions"].as_array().unwrap().iter().any(|p| p["effect"] == "allow" && p["action"] == action)
        };
        assert!(grants(&ask, "feeder_read_page"));
        assert!(grants(&ask, "feeder_read_traces"));
        assert!(grants(&ask, "feeder_list_flags"));
        assert!(!grants(&ask, "feeder_write_page"));
        assert!(grants(&review, "feeder_write_page"));
        assert!(!grants(&review, "shell"));
        assert!(ask["model"].is_null());
        assert_eq!(review["agents"]["feeder-curator"]["model"], "openai/gpt-6-sol");
        assert_eq!(review["mcp"]["feeder"]["command"], json!(["/usr/local/bin/feeder", "mcp", "--gardener", "--gardener-run", "7"]));
        assert_eq!(review["mcp"]["feeder"]["environment"]["FEEDER_HOME"], "/tmp/feeder-home");
        assert_eq!(review["mcp"]["feeder"]["enabled"], true);
    }

    #[test]
    fn opencode_acp_command_omits_unsupported_standalone_flag() {
        let dir = std::env::temp_dir().join(format!("feeder-acp-cmd-{}-{}", std::process::id(), fastrand::u32(..)));
        let cwd = dir.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let cmd = command(
            Backend::OpenCode,
            Path::new("/usr/bin/opencode"),
            &dir,
            &cwd,
            SessionMode::Ask,
            &json!({}),
            &super::super::RunConfig::default(),
        )
        .unwrap();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["acp"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opencode_pending_tool_waits_for_real_input_on_in_progress_update() {
        let mut state = State::default();
        let (mode, cwd, tools) = (SessionMode::Ask, Path::new("/tmp"), json!([]));
        state.receive(json!({ "id": 1, "result": { "protocolVersion": 1 } }), mode, cwd, &tools, "hi").unwrap();
        state.receive(json!({ "id": 2, "result": { "sessionId": "s" } }), mode, cwd, &tools, "hi").unwrap();
        let pending = json!({ "method": "session/update", "params": { "sessionId": "s", "update": {
            "sessionUpdate": "tool_call", "toolCallId": "c1", "name": "feeder_search_notebook", "status": "pending", "rawInput": {}
        } } });
        assert!(state.receive(pending, mode, cwd, &tools, "hi").unwrap().is_empty());
        let running = json!({ "method": "session/update", "params": { "sessionId": "s", "update": {
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "title": "feeder_search_notebook", "status": "in_progress",
            "rawInput": { "query": "SQLite" }
        } } });
        let started = state.receive(running, mode, cwd, &tools, "hi").unwrap();
        assert!(
            matches!(started.as_slice(), [Action::Emit(Event::ToolStarted(call))] if call.name == "search_notebook" && call.text_arg("query") == Some("SQLite"))
        );
        let finished = json!({ "method": "session/update", "params": { "sessionId": "s", "update": {
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "#1 SQLite" } }]
        } } });
        let done = state.receive(finished, mode, cwd, &tools, "hi").unwrap();
        assert!(
            matches!(done.as_slice(), [Action::Emit(Event::ToolFinished { id, output, failed: false })] if id == "c1" && output == "#1 SQLite")
        );
    }

    #[test]
    fn a_chosen_model_is_set_before_the_first_prompt() {
        let mut state = State { model: Some("gemini-4-neon-high".into()), ..State::default() };
        let (mode, cwd, tools) = (SessionMode::Ask, Path::new("/tmp"), json!([]));
        state.receive(json!({ "id": 1, "result": { "protocolVersion": 1 } }), mode, cwd, &tools, "hi").unwrap();
        let actions = state.receive(json!({ "id": 2, "result": { "sessionId": "s" } }), mode, cwd, &tools, "hi").unwrap();
        let sent: Vec<&Value> = actions.iter().filter_map(|a| if let Action::Send(m) = a { Some(m) } else { None }).collect();
        assert_eq!(sent[0]["id"], 3);
        assert_eq!(sent[0]["method"], "session/set_model");
        assert_eq!(sent[0]["params"]["modelId"], "gemini-4-neon-high");
        assert_eq!(sent[1]["id"], 4);
        assert_eq!(sent[1]["method"], "session/prompt");
        assert!(
            state
                .receive(json!({ "id": 3, "error": { "code": -32601, "message": "not implemented" } }), mode, cwd, &tools, "hi")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn acp_config_options_set_model_and_effort_before_prompt() {
        let mut state = State { model: Some("openai/gpt-6-sol".into()), effort: Some("high".into()), ..State::default() };
        let (mode, cwd, tools) = (SessionMode::Ask, Path::new("/tmp"), json!([]));
        state.receive(json!({ "id": 1, "result": { "protocolVersion": 1 } }), mode, cwd, &tools, "hi").unwrap();
        let actions = state
            .receive(
                json!({
                    "id": 2,
                    "result": {
                        "sessionId": "s",
                        "configOptions": [
                            { "id": "model", "category": "model" },
                            { "id": "effort", "category": "thought_level" }
                        ]
                    }
                }),
                mode,
                cwd,
                &tools,
                "hi",
            )
            .unwrap();
        let sent: Vec<&Value> = actions.iter().filter_map(|a| if let Action::Send(m) = a { Some(m) } else { None }).collect();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0]["id"], 3);
        assert_eq!(sent[0]["method"], "session/set_config_option");
        assert_eq!(sent[0]["params"], json!({ "sessionId": "s", "configId": "model", "value": "openai/gpt-6-sol" }));
        assert_eq!(sent[1]["id"], 4);
        assert_eq!(sent[1]["method"], "session/set_config_option");
        assert_eq!(sent[1]["params"], json!({ "sessionId": "s", "configId": "effort", "value": "high" }));
        assert_eq!(sent[2]["id"], 5);
        assert_eq!(sent[2]["method"], "session/prompt");
        assert!(state.receive(json!({ "id": 3, "result": {} }), mode, cwd, &tools, "hi").unwrap().is_empty());
        let err = state
            .receive(
                json!({ "id": 4, "error": { "code": -32602, "message": "Invalid params: effort not found: high" } }),
                mode,
                cwd,
                &tools,
                "hi",
            )
            .unwrap_err();
        assert!(err.to_string().contains("effort not found"));
    }
}
