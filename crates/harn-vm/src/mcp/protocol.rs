use super::*;

pub(crate) const REQUESTED_SCHEMA_PROPERTY_ORDER: &str = "requestedSchemaPropertyOrder";

fn elicitation_property_order(params: &rmcp::model::ElicitRequestParams) -> Option<&[String]> {
    match params {
        rmcp::model::ElicitRequestParams::FormElicitationParams {
            requested_schema, ..
        } => requested_schema.property_order.as_deref(),
        _ => None,
    }
}

fn attach_requested_schema_property_order(
    params: &mut serde_json::Value,
    property_order: &[String],
) {
    if let Some(params) = params.as_object_mut() {
        params.insert(
            REQUESTED_SCHEMA_PROPERTY_ORDER.to_string(),
            serde_json::json!(property_order),
        );
    }
}

pub(crate) fn project_elicitation_params(
    params: &rmcp::model::ElicitRequestParams,
) -> Result<serde_json::Value, serde_json::Error> {
    let property_order = elicitation_property_order(params);
    let mut value = serde_json::to_value(params)?;
    if let Some(property_order) = property_order {
        attach_requested_schema_property_order(&mut value, property_order);
    }
    Ok(value)
}

#[derive(Deserialize)]
struct JsonRpcInputEnvelope {
    result: Option<InputRequiredProjection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputRequiredProjection {
    #[serde(default)]
    input_requests: rmcp::model::InputRequests,
}

/// Parse one JSON-RPC message and project RMCP's typed elicitation field order
/// into an explicit internal sidecar before a generic JSON object can discard
/// it. JSON object order is not a protocol contract; the sidecar is.
pub(crate) fn parse_jsonrpc_message(raw: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_slice(raw)?;
    let Some(result) = value.get("result") else {
        return Ok(value);
    };
    let is_input_required = ["resultType", "status"].into_iter().any(|field| {
        result.get(field).and_then(serde_json::Value::as_str) == Some(RESULT_TYPE_INPUT_REQUIRED)
    });
    if !is_input_required || result.get("inputRequests").is_none() {
        return Ok(value);
    }
    let Ok(envelope) = serde_json::from_slice::<JsonRpcInputEnvelope>(raw) else {
        return Ok(value);
    };
    let Some(result) = envelope.result else {
        return Ok(value);
    };
    for (key, request) in result.input_requests {
        let rmcp::model::InputRequest::Elicitation(request) = request else {
            continue;
        };
        let Some(property_order) = elicitation_property_order(&request.params) else {
            continue;
        };
        let Some(params) = value
            .get_mut("result")
            .and_then(|result| result.get_mut("inputRequests"))
            .and_then(|requests| requests.get_mut(&key))
            .and_then(|request| request.get_mut("params"))
        else {
            continue;
        };
        attach_requested_schema_property_order(params, property_order);
    }
    Ok(value)
}

#[cfg(test)]
mod input_order_tests {
    use super::*;

    #[test]
    fn raw_elicitation_order_becomes_explicit_before_generic_json_sorting() {
        let raw = br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"input_required","inputRequests":{"form":{"method":"elicitation/create","params":{"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{"zeta":{"type":"string"},"alpha":{"type":"integer"}}}}}}}}"#;
        let parsed = parse_jsonrpc_message(raw).expect("valid JSON-RPC response");
        assert_eq!(
            parsed["result"]["inputRequests"]["form"]["params"][REQUESTED_SCHEMA_PROPERTY_ORDER],
            serde_json::json!(["zeta", "alpha"])
        );

        let reverse = br#"{"jsonrpc":"2.0","id":1,"result":{"status":"input_required","inputRequests":{"form":{"method":"elicitation/create","params":{"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{"alpha":{"type":"integer"},"zeta":{"type":"string"}}}}}}}}"#;
        let parsed = parse_jsonrpc_message(reverse).expect("valid reverse-order response");
        assert_eq!(
            parsed["result"]["inputRequests"]["form"]["params"][REQUESTED_SCHEMA_PROPERTY_ORDER],
            serde_json::json!(["alpha", "zeta"])
        );
    }

    #[test]
    fn ordinary_result_named_input_requests_is_not_augmented() {
        let raw = br#"{"jsonrpc":"2.0","id":1,"result":{"inputRequests":{"form":{"method":"elicitation/create","params":{"mode":"form","message":"Data","requestedSchema":{"type":"object","properties":{"zeta":{"type":"string"}}}}}}}}"#;
        let parsed = parse_jsonrpc_message(raw).expect("valid custom result");
        assert!(parsed["result"]["inputRequests"]["form"]["params"]
            .get(REQUESTED_SCHEMA_PROPERTY_ORDER)
            .is_none());
    }

    #[test]
    fn direct_sdk_elicitation_projection_carries_typed_order() {
        let raw = r#"{"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{"zeta":{"type":"string"},"alpha":{"type":"integer"}}}}"#;
        let params: rmcp::model::ElicitRequestParams =
            serde_json::from_str(raw).expect("valid typed elicitation params");
        let projected = project_elicitation_params(&params).expect("params project to JSON");
        assert_eq!(
            projected[REQUESTED_SCHEMA_PROPERTY_ORDER],
            serde_json::json!(["zeta", "alpha"])
        );
    }
}

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

pub(crate) fn unsupported_embedded_input_response(
    msg: &serde_json::Value,
) -> Option<serde_json::Value> {
    let request_id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(|value| value.as_str())?;
    Some(crate::jsonrpc::error_response(
        request_id,
        -32601,
        &format!("Method not found: {method}"),
    ))
}

pub(crate) fn mcp_connect_options(value: Option<&VmValue>) -> Result<McpConnectOptions, VmError> {
    let Some(value) = value else {
        return resolve_connect_protocol_options(None);
    };
    let VmValue::Dict(options) = value else {
        return Err(VmError::Runtime(format!(
            "mcp_connect: options must be a dict, got {}",
            value.type_name()
        )));
    };
    let protocol_version_value = options.get("protocol_version").map(|value| value.display());
    resolve_connect_protocol_options(protocol_version_value.as_deref())
}

pub(crate) fn resolve_connect_protocol_options(
    protocol_version_value: Option<&str>,
) -> Result<McpConnectOptions, VmError> {
    let protocol_version_value = protocol_version_value
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let protocol_version = protocol_version_value
        .unwrap_or(PROTOCOL_VERSION)
        .to_string();
    if !crate::mcp_protocol::is_sdk_protocol_version(&protocol_version) {
        return Err(VmError::Runtime(format!(
            "mcp_connect: unsupported protocol_version {protocol_version:?}; expected one of {}",
            crate::mcp_protocol::sdk_protocol_versions().join(", ")
        )));
    }
    Ok(McpConnectOptions { protocol_version })
}

pub(crate) fn client_info() -> serde_json::Value {
    serde_json::json!({
        "name": "harn",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub(crate) fn stable_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "elicitation": {"form": {}, "url": {}},
        "roots": {},
        "sampling": {},
        "extensions": {
            TASKS_EXTENSION_ID: {},
        },
    })
}

pub(crate) fn request_params_for_protocol(
    protocol_version: &str,
    params: serde_json::Value,
) -> serde_json::Value {
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
        MCP_META_KEY_PROTOCOL_VERSION.to_string(),
        serde_json::Value::String(protocol_version.to_string()),
    );
    meta.insert(MCP_META_KEY_CLIENT_INFO.to_string(), client_info());
    meta.insert(
        MCP_META_KEY_CLIENT_CAPABILITIES.to_string(),
        stable_client_capabilities(),
    );
    object.insert("_meta".to_string(), serde_json::Value::Object(meta));
    serde_json::Value::Object(object)
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
