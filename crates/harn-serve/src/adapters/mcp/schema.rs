//! MCP wire-shape normalization and JSON-RPC result helpers.
use super::*;

pub(super) fn build_call_request(
    adapter: &str,
    caller: &str,
    tool_name: &str,
    arguments: JsonValue,
    auth: AuthRequest,
    cancel_token: Arc<AtomicBool>,
    progress: Option<harn_vm::mcp_progress::ProgressContext>,
    request_id: Option<String>,
) -> Result<CallRequest, String> {
    let arguments = match arguments {
        JsonValue::Null => CallArguments::Named(BTreeMap::new()),
        JsonValue::Object(values) => CallArguments::Named(
            values
                .into_iter()
                .collect::<BTreeMap<String, serde_json::Value>>(),
        ),
        JsonValue::Array(values) => CallArguments::Positional(values),
        _ => {
            return Err("tool arguments must be an object, array, or null".to_string());
        }
    };
    Ok(CallRequest {
        adapter: adapter.to_string(),
        function: tool_name.to_string(),
        arguments,
        auth,
        caller: caller.to_string(),
        replay_key: None,
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: Some(cancel_token),
        agent_session_id: None,
        agent_event_sink: None,
        actor_chain: None,
        actor_chain_hop: None,
        progress,
        tenant_id: None,
        request_id,
        auth_context: None,
        auth_principal: None,
    })
}

pub(super) fn paged_result(key: &str, entries: Vec<JsonValue>, params: &JsonValue) -> JsonValue {
    let (offset, page_size) = parse_cursor(params);
    let page_end = (offset + page_size).min(entries.len());
    let page_entries = entries[offset..page_end].to_vec();
    let mut result = json!({ key: page_entries });
    if page_end < entries.len() {
        result["nextCursor"] = json!(encode_cursor(page_end));
    }
    result
}

pub(super) fn encode_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

pub(super) fn parse_cursor(params: &JsonValue) -> (usize, usize) {
    let offset = params
        .get("cursor")
        .and_then(JsonValue::as_str)
        .and_then(|cursor| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(cursor)
                .ok()?;
            let text = String::from_utf8(bytes).ok()?;
            text.parse::<usize>().ok()
        })
        .unwrap_or(0);
    (offset, 50)
}

pub(super) fn tool_entry(function: &crate::ExportedFunction) -> JsonValue {
    let mut entry = json!({
        "name": function.name,
        "title": function.name,
        "description": format!("Invoke exported Harn function '{}'.", function.name),
        "annotations": {
            "title": function.name,
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true,
        },
        "inputSchema": function.input_schema,
    });
    if let Some(output_schema) = function.output_schema.clone() {
        entry["outputSchema"] = output_schema;
    }
    entry
}

pub(super) fn tool_call_success(response: CallResponse) -> JsonValue {
    let mut result = json!({
        "content": content_blocks(&response.value),
        "isError": false,
    });
    if let JsonValue::Object(map) = response.value {
        result["structuredContent"] = JsonValue::Object(map);
    }
    result
}

pub(super) fn tool_call_error(message: String) -> JsonValue {
    json!({
        "content": [{
            "type": "text",
            "text": message,
        }],
        "isError": true,
    })
}

pub(super) fn content_blocks(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::String(text) => json!([{ "type": "text", "text": text }]),
        _ => json!([{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]),
    }
}

pub(super) fn request_key(id: &JsonValue) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_string())
}

pub(super) fn parse_error_response(message: &str) -> JsonValue {
    harn_vm::jsonrpc::error_response(JsonValue::Null, -32700, &format!("Parse error: {message}"))
}

pub(super) fn derived_server_name(catalog: &ExportCatalog) -> String {
    catalog
        .script_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("harn-serve")
        .to_string()
}
