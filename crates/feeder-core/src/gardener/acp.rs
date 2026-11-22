//! Restricted ACP sessions for OpenCode and Antigravity.

use std::collections::HashSet;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::{Backend, Event, SessionMode, ToolCall};
use crate::{Error, Result};

/// Run with the CLI's own login in a Curator-specific profile, without inheriting shell secrets.
pub(super) fn profile_command(backend: Backend, binary: &Path, data: &Path) -> Result<Command> {
    let home = profile_dir(backend, data)?;
    let mut cmd = Command::new(binary);
    cmd.env_clear().env("HOME", &home);
    for var in ["PATH", "LANG", "LC_ALL", "TERM"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    for (name, dir) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")]
    {
        cmd.env(name, home.join(dir));
    }
    Ok(cmd)
}

fn profile_dir(backend: Backend, data: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(data)?.join("curator-profiles").join(backend.as_str());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&path)?;
    Ok(path)
}

pub(super) fn command(backend: Backend, binary: &Path, data: &Path, cwd: &Path, mode: SessionMode, server: &Value) -> Result<Command> {
    let mut cmd = profile_command(backend, binary, data)?;
    let output = profile_command(backend, binary, data)?.arg("--version").output()?;
    let version = String::from_utf8_lossy(&output.stdout);
    let minimum = match backend {
        Backend::OpenCode => (2, 3, 2),
        Backend::Antigravity => (1, 5, 5),
        _ => return Err(Error::Invalid("invalid ACP backend".into())),
    };
    if !output.status.success() || !supported_version(&version, minimum) {
        let (major, minor, patch) = minimum;
        return Err(Error::Invalid(format!("Update {} to {major}.{minor}.{patch} or a later {major}.x release.", backend.label())));
    }
    cmd.current_dir(cwd).arg("acp");
    match backend {
        Backend::OpenCode => {
            let config = opencode_config(mode);
            std::fs::write(cwd.join("opencode.json"), config.to_string())?;
            // Account data is kept in the private profile, but model runs use only this config.
            let config_dir = cwd.join("config");
            std::fs::create_dir_all(&config_dir)?;
            cmd.env("XDG_CONFIG_HOME", &config_dir)
                .env("OPENCODE_CONFIG_DIR", &config_dir)
                .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "1");
        }
        Backend::Antigravity => {
            configure_antigravity(data, mode, server)?;
            cmd.args(["--agent", "feeder-curator", "--sandbox", "--disable-telemetry"]);
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

fn opencode_config(mode: SessionMode) -> Value {
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
    json!({
        "default_agent": "feeder-curator",
        "agents": { "feeder-curator": { "description": "Feeder Curator", "mode": "primary", "system": mode.instructions() } },
        "update": "disable",
        "permissions": permissions,
        "policies": policies,
    })
}

fn policy(action: &str, resource: &str, effect: &str) -> Value {
    json!({ "action": action, "resource": resource, "effect": effect })
}

fn configure_antigravity(data: &Path, mode: SessionMode, server: &Value) -> Result<()> {
    let home = profile_dir(Backend::Antigravity, data)?.join(".gemini");
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
    // Antigravity's file config supports eager tools, so it never needs to read schema files.
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

pub(super) enum Action {
    Send(Value),
    Emit(Event),
}

/// The ACP session and its outstanding prompt. Provider IDs live only for this process.
#[derive(Default)]
pub(super) struct State {
    session: Option<String>,
    pending: Option<u64>,
    next: u64,
    tools: HashSet<String>,
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

    pub(super) fn prompt(&mut self, text: &str) -> Result<Value> {
        let session = self.session.as_deref().ok_or_else(|| Error::Invalid("Curator is still connecting".into()))?;
        if self.pending.is_some() {
            return Err(Error::Invalid("Curator is still answering".into()));
        }
        self.next = self.next.saturating_add(1).max(3);
        self.pending = Some(self.next);
        Ok(request(self.next, "session/prompt", json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] })))
    }

    pub(super) fn ready(&self) -> bool {
        self.session.is_some() && self.pending.is_none()
    }

    pub(super) fn receive(&mut self, message: Value, mode: SessionMode, cwd: &Path, server: &Value, opening: &str) -> Result<Vec<Action>> {
        if let Some(method) = message["method"].as_str() {
            if let Some(id) = message.get("id") {
                return Ok(vec![Action::Send(reject_request(id, method, &message["params"]))]);
            }
            let session = message["params"]["sessionId"].as_str();
            if method == "session/update" && session.is_some() && session == self.session.as_deref() {
                return Ok(self.update(&message["params"]["update"], mode));
            }
            return Ok(Vec::new());
        }
        let Some(id) = message["id"].as_u64() else { return Ok(Vec::new()) };
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
                Ok(vec![Action::Emit(Event::Started), Action::Send(self.prompt(opening)?)])
            }
            _ => {
                let reason = message["result"]["stopReason"].as_str().unwrap_or_default();
                if !matches!(reason, "end_turn" | "max_tokens" | "max_turn_requests" | "cancelled" | "refusal") {
                    return Err(Error::Invalid("The provider did not complete the Curator turn".into()));
                }
                self.pending = None;
                self.tools.clear();
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
        let mut actions = Vec::new();
        if !self.tools.contains(id)
            && let Some(name) = tool_name(update, mode)
        {
            self.tools.insert(id.to_string());
            actions.push(Action::Emit(Event::ToolStarted(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                input: update["rawInput"].clone(),
            })));
        }
        let status = update["status"].as_str().unwrap_or_default();
        if matches!(status, "completed" | "failed") && self.tools.remove(id) {
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

fn tool_name(call: &Value, mode: SessionMode) -> Option<&'static str> {
    ["name", "title"].into_iter().filter_map(|key| call[key].as_str()).find_map(|name| {
        let name = ["feeder_", "mcp__feeder__", "mcp_feeder_"].into_iter().find_map(|prefix| name.strip_prefix(prefix)).unwrap_or(name);
        mode.mcp_tools().iter().copied().find(|&tool| tool == name)
    })
}

fn reject_request(id: &Value, method: &str, params: &Value) -> Value {
    if method == "session/request_permission" {
        let reject = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| option["kind"] == "reject_once")
            .and_then(|option| option["optionId"].as_str());
        let outcome = match reject {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "This Curator client does not offer this capability" } })
    }
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
        let shell = reject_request(
            &json!(7),
            "session/request_permission",
            &json!({
                "options": [{ "kind": "allow_once", "optionId": "allow" }, { "kind": "reject_once", "optionId": "reject" }]
            }),
        );
        assert_eq!(shell.pointer("/result/outcome/optionId"), Some(&json!("reject")));
        assert_eq!(reject_request(&json!(7), "fs/read_text_file", &json!({})).pointer("/error/code"), Some(&json!(-32601)));
        let ask = opencode_config(SessionMode::Ask);
        let review = opencode_config(SessionMode::Review);
        let grants = |config: &Value, action: &str| {
            config["permissions"].as_array().unwrap().iter().any(|p| p["effect"] == "allow" && p["action"] == action)
        };
        assert!(grants(&ask, "feeder_read_page"));
        assert!(!grants(&ask, "feeder_write_page"));
        assert!(grants(&review, "feeder_write_page"));
        assert!(!grants(&review, "shell"));
    }
}
