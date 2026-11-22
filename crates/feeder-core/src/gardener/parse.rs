//! Turns each CLI's JSON stream into [`Event`]s: Claude Code's `stream-json` and Codex's
//! `exec --json`. Lines that aren't JSON, or events the gardener doesn't show, yield nothing.

use serde_json::Value;

use super::session::{Event, ToolCall};

/// The prefix Claude Code puts on the gardener server's tool names.
const CLAUDE_TOOL_PREFIX: &str = "mcp__feeder__";

/// What a stream has seen so far, for decisions that span lines.
#[derive(Debug, Default)]
pub(super) struct StreamState {
    /// The last thing shown was assistant text, so a new text block needs a paragraph break.
    after_text: bool,
    /// Codex's thread id, for resuming the conversation on the next turn.
    pub(super) thread_id: Option<String>,
}

impl StreamState {
    fn text(&mut self, text: &str, out: &mut Vec<Event>) {
        if text.is_empty() {
            return;
        }
        out.push(Event::Text(text.to_string()));
        self.after_text = true;
    }

    /// A new text block after earlier text: separate the two.
    fn text_block(&mut self, text: &str, out: &mut Vec<Event>) {
        if self.after_text {
            out.push(Event::Text("\n\n".into()));
        }
        self.text(text, out);
    }

    fn other(&mut self, event: Event, out: &mut Vec<Event>) {
        self.after_text = false;
        out.push(event);
    }
}

/// One line of `claude -p --output-format stream-json --verbose --include-partial-messages`.
pub(super) fn claude_line(line: &str, state: &mut StreamState) -> Vec<Event> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(line) else { return out };
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => state.other(Event::Started, &mut out),
        Some("stream_event") => {
            let event = &v["event"];
            match (event["type"].as_str(), event["delta"]["type"].as_str()) {
                (Some("content_block_start"), _) if event["content_block"]["type"] == "text" && state.after_text => {
                    out.push(Event::Text("\n\n".into()));
                }
                (Some("content_block_delta"), Some("text_delta")) => state.text(event["delta"]["text"].as_str().unwrap_or(""), &mut out),
                (Some("content_block_delta"), Some("thinking_delta")) => {
                    let delta = event["delta"]["thinking"].as_str().unwrap_or("");
                    if !delta.is_empty() {
                        state.other(Event::Thinking(delta.to_string()), &mut out);
                    }
                }
                _ => {}
            }
        }
        // The finished message carries each tool call with its whole input.
        Some("assistant") => {
            for block in v["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                let name = block["name"].as_str().unwrap_or("");
                let call = ToolCall {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: name.strip_prefix(CLAUDE_TOOL_PREFIX).unwrap_or(name).to_string(),
                    input: block["input"].clone(),
                };
                state.other(Event::ToolStarted(call), &mut out);
            }
        }
        Some("user") => {
            for block in v["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_result") {
                let output = match &block["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                };
                let id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                state.other(Event::ToolFinished { id, output, failed: block["is_error"] == true }, &mut out);
            }
        }
        Some("result") => {
            let event = if v["is_error"] == true {
                let why = v["result"].as_str().unwrap_or("Claude Code stopped").to_string();
                failure(why)
            } else {
                Event::TurnFinished
            };
            state.other(event, &mut out);
        }
        _ => {}
    }
    out
}

/// One line of `codex exec --json`.
pub(super) fn codex_line(line: &str, state: &mut StreamState) -> Vec<Event> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(line) else { return out };
    let item = &v["item"];
    match (v["type"].as_str(), item["type"].as_str()) {
        (Some("thread.started"), _) => {
            let first = state.thread_id.is_none();
            state.thread_id = v["thread_id"].as_str().map(str::to_string);
            if first {
                state.other(Event::Started, &mut out);
            }
        }
        (Some("item.started"), Some("mcp_tool_call")) => {
            let call = ToolCall {
                id: item["id"].as_str().unwrap_or("").to_string(),
                name: item["tool"].as_str().unwrap_or("").to_string(),
                input: item["arguments"].clone(),
            };
            state.other(Event::ToolStarted(call), &mut out);
        }
        (Some("item.completed"), Some("mcp_tool_call")) => {
            let error = item["error"]["message"].as_str().or_else(|| item["error"].as_str());
            let output = match error {
                Some(e) => e.to_string(),
                None => item["result"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            let failed = error.is_some() || item["status"] == "failed" || item["result"]["is_error"] == true;
            state.other(Event::ToolFinished { id: item["id"].as_str().unwrap_or("").to_string(), output, failed }, &mut out);
        }
        (Some("item.completed"), Some("agent_message")) => state.text_block(item["text"].as_str().unwrap_or(""), &mut out),
        (Some("item.completed"), Some("reasoning")) => {
            let text = item["text"].as_str().unwrap_or("");
            if !text.is_empty() {
                state.other(Event::Thinking(text.to_string()), &mut out);
            }
        }
        (Some("turn.completed"), _) => state.other(Event::TurnFinished, &mut out),
        (Some("turn.failed"), _) => {
            let why = v["error"]["message"].as_str().unwrap_or("Codex stopped").to_string();
            state.other(failure(why), &mut out);
        }
        (Some("error"), _) => {
            let why = v["message"].as_str().unwrap_or("Codex reported an error").to_string();
            state.other(failure(why), &mut out);
        }
        _ => {}
    }
    out
}

/// A failure, or a request to sign in when that's what it is.
pub(super) fn failure(why: String) -> Event {
    if looks_signed_out(&why) { Event::NeedsSignIn } else { Event::Failed(why) }
}

fn looks_signed_out(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "not logged in",
        "please run /login",
        "log in",
        "login",
        "sign in",
        "unauthorized",
        "401",
        "invalid api key",
        "authentication",
        "connect a provider",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn feed(lines: &[Value], parse: fn(&str, &mut StreamState) -> Vec<Event>) -> (Vec<Event>, StreamState) {
        let mut state = StreamState::default();
        let events = lines.iter().flat_map(|l| parse(&l.to_string(), &mut state)).collect();
        (events, state)
    }

    #[test]
    fn claude_streams_thinking_text_and_tool_calls() {
        let lines = [
            json!({ "type": "system", "subtype": "init", "tools": [] }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "Check flags first." } } }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "Looking at " } } }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "the flags." } } }),
            json!({ "type": "assistant", "message": { "content": [
                { "type": "text", "text": "Looking at the flags." },
                { "type": "tool_use", "id": "t1", "name": "mcp__feeder__list_flags", "input": {} }
            ] } }),
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "No open flags." }] }] } }),
            json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "text" } } }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "Nothing to do." } } }),
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": "Nothing to do." }),
            json!("not an event"),
        ];
        let (events, _) = feed(&lines, claude_line);
        assert_eq!(
            events,
            [
                Event::Started,
                Event::Thinking("Check flags first.".into()),
                Event::Text("Looking at ".into()),
                Event::Text("the flags.".into()),
                Event::ToolStarted(ToolCall { id: "t1".into(), name: "list_flags".into(), input: json!({}) }),
                Event::ToolFinished { id: "t1".into(), output: "No open flags.".into(), failed: false },
                Event::Text("Nothing to do.".into()),
                Event::TurnFinished,
            ]
        );
    }

    #[test]
    fn claude_errors_become_sign_in_or_failure() {
        let signed_out = json!({ "type": "result", "is_error": true, "result": "Not logged in · Please run /login" });
        let (events, _) = feed(&[signed_out], claude_line);
        assert_eq!(events, [Event::NeedsSignIn]);
        let other = json!({ "type": "result", "is_error": true, "result": "Overloaded" });
        let (events, _) = feed(&[other], claude_line);
        assert_eq!(events, [Event::Failed("Overloaded".into())]);
    }

    #[test]
    fn codex_items_become_events_and_keep_the_thread_id() {
        let lines = [
            json!({ "type": "thread.started", "thread_id": "th-1" }),
            json!({ "type": "turn.started" }),
            json!({ "type": "item.completed", "item": { "id": "i0", "type": "agent_message", "text": "Reading flags." } }),
            json!({ "type": "item.started", "item": { "id": "i1", "type": "mcp_tool_call", "server": "feeder", "tool": "list_flags", "arguments": {}, "status": "in_progress" } }),
            json!({ "type": "item.completed", "item": { "id": "i1", "type": "mcp_tool_call", "tool": "list_flags", "result": { "content": [{ "type": "text", "text": "No open flags." }] }, "error": null, "status": "completed" } }),
            json!({ "type": "item.completed", "item": { "id": "i2", "type": "reasoning", "text": "Nothing flagged." } }),
            json!({ "type": "item.completed", "item": { "id": "i3", "type": "agent_message", "text": "All good." } }),
            json!({ "type": "item.completed", "item": { "id": "i4", "type": "agent_message", "text": "Done." } }),
            json!({ "type": "turn.completed", "usage": {} }),
        ];
        let (events, state) = feed(&lines, codex_line);
        assert_eq!(state.thread_id.as_deref(), Some("th-1"));
        assert_eq!(
            events,
            [
                Event::Started,
                Event::Text("Reading flags.".into()),
                Event::ToolStarted(ToolCall { id: "i1".into(), name: "list_flags".into(), input: json!({}) }),
                Event::ToolFinished { id: "i1".into(), output: "No open flags.".into(), failed: false },
                Event::Thinking("Nothing flagged.".into()),
                Event::Text("All good.".into()),
                Event::Text("\n\n".into()),
                Event::Text("Done.".into()),
                Event::TurnFinished,
            ]
        );
    }

    #[test]
    fn codex_tool_errors_and_failed_turns_are_reported() {
        let lines = [
            json!({ "type": "item.completed", "item": { "id": "i1", "type": "mcp_tool_call", "tool": "write_page", "result": null, "error": { "message": "the gardener must name the flag" }, "status": "failed" } }),
            json!({ "type": "turn.failed", "error": { "message": "401 Unauthorized" } }),
        ];
        let (events, _) = feed(&lines, codex_line);
        assert_eq!(
            events,
            [Event::ToolFinished { id: "i1".into(), output: "the gardener must name the flag".into(), failed: true }, Event::NeedsSignIn]
        );
    }
}
