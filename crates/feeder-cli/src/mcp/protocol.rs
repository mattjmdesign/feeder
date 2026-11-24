use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub(super) const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub(super) fn supports_batching(version: &str) -> bool {
    matches!(version, "2025-03-26" | "2024-11-05")
}

pub(super) fn ann_read_only() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

pub(super) fn ann_local_write() -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": false,
        "idempotentHint": false,
        "openWorldHint": false
    })
}

pub(super) fn ann_local_idempotent() -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

pub(super) fn ann_destructive() -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": true,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

pub(super) fn decode_args<T: DeserializeOwned>(tool: &str, args: &Value) -> Result<T, String> {
    let value = if args.is_null() { json!({}) } else { args.clone() };
    if !value.is_object() {
        return Err(format!("invalid arguments for `{tool}`: expected a JSON object"));
    }
    serde_json::from_value::<T>(value).map_err(|e| format!("invalid arguments for `{tool}`: {e}"))
}

pub(super) fn non_empty(val: Option<String>) -> Option<String> {
    val.and_then(|s| {
        let trimmed = s.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub(super) fn project_schema() -> Value {
    json!({
        "type": "string",
        "description": "Project name/slug."
    })
}

pub(super) fn project_path_schema() -> Value {
    json!({
        "type": "string",
        "description": "Project folder path."
    })
}
