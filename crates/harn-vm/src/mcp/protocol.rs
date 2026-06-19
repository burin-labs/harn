use super::*;

pub(crate) fn parse_jsonrpc_result(msg: serde_json::Value) -> Result<serde_json::Value, VmError> {
    if let Some(error) = msg.get("error") {
        return Err(jsonrpc_error_to_vm_error(error));
    }
    Ok(msg
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

pub(crate) fn jsonrpc_error_to_vm_error(error: &serde_json::Value) -> VmError {
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown MCP error");
    let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "MCP error ({code}): {message}"
    ))))
}

pub(crate) fn client_request_rejection(msg: &serde_json::Value) -> Option<serde_json::Value> {
    let request_id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(|value| value.as_str())?;
    Some(crate::jsonrpc::error_response(
        request_id,
        -32601,
        &format!("Method not found: {method}"),
    ))
}

pub(crate) fn resolve_protocol_mode(
    protocol_mode: Option<&str>,
    protocol_version: Option<&str>,
) -> Result<McpProtocolMode, VmError> {
    let normalized = protocol_mode.map(|value| value.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        Some("legacy") | Some("2025") | Some("2025-11-25") => Ok(McpProtocolMode::Legacy),
        Some("rc") | Some("modern") | Some("draft") | Some("draft-2026-v1") => {
            Ok(McpProtocolMode::Modern)
        }
        Some(other) => Err(VmError::Runtime(format!(
            "mcp_connect: unsupported protocol_mode {other:?}; expected \"legacy\" or \"rc\""
        ))),
        None if protocol_version == Some(DRAFT_PROTOCOL_VERSION) => Ok(McpProtocolMode::Modern),
        None => Ok(McpProtocolMode::Legacy),
    }
}

pub(crate) fn mcp_connect_options(value: Option<&VmValue>) -> Result<McpConnectOptions, VmError> {
    let Some(value) = value else {
        return Ok(McpConnectOptions {
            protocol_mode: McpProtocolMode::Legacy,
            protocol_version: PROTOCOL_VERSION.to_string(),
        });
    };
    let VmValue::Dict(options) = value else {
        return Err(VmError::Runtime(format!(
            "mcp_connect: options must be a dict, got {}",
            value.type_name()
        )));
    };
    let protocol_mode_value = options.get("protocol_mode").map(|value| value.display());
    let protocol_version_value = options.get("protocol_version").map(|value| value.display());
    let protocol_mode = resolve_protocol_mode(
        protocol_mode_value.as_deref(),
        protocol_version_value.as_deref(),
    )?;
    let protocol_version = protocol_version_value
        .unwrap_or_else(|| default_protocol_version(protocol_mode).to_string());
    Ok(McpConnectOptions {
        protocol_mode,
        protocol_version,
    })
}

pub(crate) fn default_protocol_version(mode: McpProtocolMode) -> &'static str {
    match mode {
        McpProtocolMode::Legacy => PROTOCOL_VERSION,
        McpProtocolMode::Modern => DRAFT_PROTOCOL_VERSION,
    }
}

pub(crate) fn legacy_initialize_params(protocol_version: &str) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": legacy_client_capabilities(),
        "clientInfo": client_info(),
    })
}

pub(crate) fn client_info() -> serde_json::Value {
    serde_json::json!({
        "name": "harn",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub(crate) fn legacy_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "elicitation": {},
        "roots": {
            "listChanged": true,
        },
        "sampling": {},
    })
}

pub(crate) fn modern_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "elicitation": {},
        "roots": {},
        "sampling": {},
    })
}

pub(crate) fn request_params_for_protocol(
    protocol_mode: McpProtocolMode,
    protocol_version: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    if protocol_mode == McpProtocolMode::Legacy {
        return params;
    }

    let mut object = match params {
        serde_json::Value::Object(object) => object,
        serde_json::Value::Null => serde_json::Map::new(),
        other => serde_json::Map::from_iter([("value".to_string(), other)]),
    };
    let mut meta = object
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        RC_META_KEY_PROTOCOL_VERSION.to_string(),
        serde_json::Value::String(protocol_version.to_string()),
    );
    meta.insert(RC_META_KEY_CLIENT_INFO.to_string(), client_info());
    meta.insert(
        RC_META_KEY_CLIENT_CAPABILITIES.to_string(),
        modern_client_capabilities(),
    );
    object.insert("_meta".to_string(), serde_json::Value::Object(meta));
    serde_json::Value::Object(object)
}

pub(crate) fn maybe_retry_unsupported_protocol(
    protocol_mode: McpProtocolMode,
    protocol_version: &mut String,
    msg: &serde_json::Value,
) -> bool {
    if protocol_mode != McpProtocolMode::Modern {
        return false;
    }
    let Some(error) = msg.get("error") else {
        return false;
    };
    if error.get("code").and_then(|value| value.as_i64()) != Some(UNSUPPORTED_PROTOCOL_VERSION_CODE)
    {
        return false;
    }
    let supported = error
        .get("data")
        .and_then(|data| data.get("supported"))
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    let Some(selected) = select_supported_protocol_version(&supported) else {
        return false;
    };
    if selected == protocol_version {
        return false;
    }
    *protocol_version = selected.to_string();
    true
}

pub(crate) fn should_fallback_to_legacy_http_discovery(
    protocol_mode: McpProtocolMode,
    method: &str,
    status: u16,
) -> bool {
    protocol_mode == McpProtocolMode::Modern
        && method == "server/discover"
        && matches!(status, 400 | 404 | 405)
}

pub(crate) fn http_discovery_fallback_response(id: Option<u64>) -> serde_json::Value {
    crate::jsonrpc::error_response(
        id.map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
        -32601,
        "Modern MCP discovery was not recognized",
    )
}

pub(crate) fn select_supported_protocol_version(supported: &[&str]) -> Option<&'static str> {
    [DRAFT_PROTOCOL_VERSION, PROTOCOL_VERSION]
        .into_iter()
        .find(|candidate| supported.iter().any(|value| value == candidate))
}

pub(crate) fn is_method_not_found_response(msg: &serde_json::Value) -> bool {
    msg.get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_i64())
        == Some(-32601)
}

pub(crate) fn extract_tool_headers(tool: &serde_json::Value) -> Result<Vec<McpToolHeader>, String> {
    let Some(properties) = tool
        .get("inputSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(|value| value.as_object())
    else {
        return Ok(Vec::new());
    };

    let mut headers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (parameter, schema) in properties {
        let Some(header_name) = schema.get(X_MCP_HEADER).and_then(|value| value.as_str()) else {
            continue;
        };
        validate_mcp_header_annotation(parameter, header_name, schema, &mut seen)?;
        headers.push(McpToolHeader {
            parameter: parameter.clone(),
            header_name: format!("Mcp-Param-{header_name}"),
        });
    }
    Ok(headers)
}

pub(crate) fn filter_tools_for_client(tools: &[serde_json::Value]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("<unnamed>");
            match extract_tool_headers(tool) {
                Ok(_) => Some(tool.clone()),
                Err(reason) => {
                    tracing::warn!(tool = name, %reason, "excluding MCP tool from tools/list");
                    None
                }
            }
        })
        .collect()
}

pub(crate) fn validate_mcp_header_annotation(
    parameter: &str,
    header_name: &str,
    schema: &serde_json::Value,
    seen: &mut std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if header_name.is_empty() {
        return Err(format!("{parameter}: x-mcp-header must not be empty"));
    }
    if !header_name.is_ascii() || header_name.bytes().any(|byte| matches!(byte, b' ' | b':')) {
        return Err(format!(
            "{parameter}: x-mcp-header must be ASCII and exclude space or colon"
        ));
    }
    if reqwest::header::HeaderName::from_bytes(format!("Mcp-Param-{header_name}").as_bytes())
        .is_err()
    {
        return Err(format!(
            "{parameter}: x-mcp-header does not form a valid HTTP header name"
        ));
    }
    let lower = header_name.to_ascii_lowercase();
    if !seen.insert(lower) {
        return Err(format!(
            "{parameter}: duplicate x-mcp-header value {header_name:?}"
        ));
    }
    let is_primitive = match schema.get("type") {
        Some(serde_json::Value::String(value)) => {
            matches!(value.as_str(), "string" | "number" | "integer" | "boolean")
        }
        Some(serde_json::Value::Array(values)) => values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|ty| matches!(ty, "string" | "number" | "integer" | "boolean"))
        }),
        _ => false,
    };
    if !is_primitive {
        return Err(format!(
            "{parameter}: x-mcp-header is only valid on primitive schema types"
        ));
    }
    Ok(())
}

pub(crate) fn encode_mcp_header_value(value: &serde_json::Value) -> Option<String> {
    let raw = match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    if is_plain_mcp_header_value(&raw) {
        Some(raw)
    } else {
        Some(format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
        ))
    }
}

pub(crate) fn is_plain_mcp_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| matches!(byte, b'\t' | b' '..=b'~'))
}

pub(crate) fn extract_content_text(result: &serde_json::Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            json_to_vm_value(result).display()
        } else {
            texts.join("\n")
        }
    } else {
        json_to_vm_value(result).display()
    }
}
