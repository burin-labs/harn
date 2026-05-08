//! A2A wire-schema normalization and response construction helpers.
use super::*;

impl A2aServer {
    pub(super) fn agent_card(&self, public_url: &str) -> JsonValue {
        let skills = self
            .catalog
            .functions
            .values()
            .map(public_skill_card)
            .collect::<Vec<_>>();
        let extended_supported = self.extended_card_available();
        let (security_schemes, security) = if extended_supported {
            policy_security_schemes(self.core.auth_policy())
        } else {
            (json!({}), json!([]))
        };
        let mut card = json!({
            "protocolVersion": A2A_PROTOCOL_VERSION,
            "name": self.agent_name,
            "description": "Harn peer agent",
            "url": public_url,
            "preferredTransport": "JSONRPC",
            "additionalInterfaces": [
                {
                    "url": public_url,
                    "transport": "JSONRPC",
                },
                {
                    "url": format!("{}{}", public_url.trim_end_matches('/'), A2A_REST_BASE),
                    "transport": "HTTP+JSON",
                }
            ],
            "version": env!("CARGO_PKG_VERSION"),
            "provider": {
                "organization": "Harn",
                "url": "https://harn.dev"
            },
            "securitySchemes": security_schemes,
            "security": security,
            "supportsAuthenticatedExtendedCard": extended_supported,
            "defaultInputModes": ["application/json", "text/plain", "application/octet-stream"],
            "defaultOutputModes": ["application/json", "text/plain", "application/octet-stream"],
            "capabilities": {
                "streaming": true,
                "pushNotifications": true
            },
            "skills": skills
        });
        if let Some(secret) = self.card_signing_secret.as_deref() {
            sign_card(&mut card, secret);
        }
        card
    }

    /// Authenticated extended card. The public card advertises the
    /// available skills with a generic description and the set of
    /// declared security schemes; the extended card adds per-skill
    /// `outputModes` detail (currently identical to the public card),
    /// includes the authenticated principal's subject so callers can
    /// verify the auth round-trip, and tags itself with
    /// `metadata.extendedAgentCard: true` so it cannot be confused with
    /// the public card.
    pub(super) fn extended_agent_card(
        &self,
        public_url: &str,
        principal_subject: &str,
    ) -> JsonValue {
        let mut card = self.agent_card(public_url);
        if let Some(object) = card.as_object_mut() {
            object.insert(
                "metadata".to_string(),
                json!({
                    "extendedAgentCard": true,
                    "principal": principal_subject,
                }),
            );
            // Mirror declared schemes/requirements onto the extended
            // card. They are also on the public card when the feature
            // is enabled, but a future change might choose to omit
            // them publicly while keeping the extended card intact.
            let (security_schemes, security) = policy_security_schemes(self.core.auth_policy());
            object.insert("securitySchemes".to_string(), security_schemes);
            object.insert("security".to_string(), security);
            object.insert(
                "skills".to_string(),
                JsonValue::Array(
                    self.catalog
                        .functions
                        .values()
                        .map(extended_skill_card)
                        .collect(),
                ),
            );
        }
        card
    }

    pub(super) fn extended_card_available(&self) -> bool {
        !self.core.auth_policy().methods.is_empty()
    }
}

pub(super) fn publish_locked(task: &mut TaskState, event: JsonValue) {
    task.events.push(event.clone());
    task.subscribers.retain(|tx| {
        tx.unbounded_send(wrap_event(JsonValue::Null, event.clone()))
            .is_ok()
    });
    if task.status.is_terminal() {
        task.subscribers.clear();
    }
}

pub(super) fn wrap_event(rpc_id: JsonValue, event: JsonValue) -> JsonValue {
    harn_vm::jsonrpc::response(rpc_id, event)
}

pub(super) fn task_to_json(task: &TaskState) -> JsonValue {
    let history = task
        .history
        .iter()
        .map(|message| {
            json!({
                "id": message.id,
                "role": message.role,
                "parts": message.parts,
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "id": task.id,
        "status": {"state": task.status.as_str()},
        "history": history,
        "artifacts": task.artifacts.clone(),
    });
    if let Some(context_id) = task.context_id.as_ref() {
        value["contextId"] = JsonValue::String(context_id.clone());
    }
    if !task.metadata.is_empty() {
        value["metadata"] = serde_json::to_value(&task.metadata)
            .unwrap_or_else(|_| JsonValue::Object(Default::default()));
    }
    value
}

pub(super) fn handoff_task_metadata(
    response: &CallResponse,
) -> Option<BTreeMap<String, JsonValue>> {
    let handoffs = harn_vm::orchestration::extract_handoffs_from_json_value(&response.value);
    if handoffs.is_empty() {
        return None;
    }
    Some(BTreeMap::from([
        (
            "handoff_ids".to_string(),
            JsonValue::Array(
                handoffs
                    .iter()
                    .map(|handoff| JsonValue::String(handoff.id.clone()))
                    .collect(),
            ),
        ),
        (
            "handoffs".to_string(),
            serde_json::to_value(&handoffs).unwrap_or_else(|_| JsonValue::Array(Vec::new())),
        ),
    ]))
}

pub(super) fn status_event(task_id: &str, status: TaskStatus) -> JsonValue {
    json!({
        "type": "status",
        "taskId": task_id,
        "status": {"state": status.as_str()},
    })
}

pub(super) fn task_rpc_response(rpc_id: &JsonValue, task_json: JsonValue) -> JsonValue {
    harn_vm::jsonrpc::response(rpc_id.clone(), task_json)
}

pub(super) fn error_response(rpc_id: JsonValue, code: i64, message: &str) -> JsonValue {
    harn_vm::jsonrpc::error_response(rpc_id, code, message)
}

pub(super) fn push_config_error_response(rpc_id: JsonValue, message: &str) -> JsonValue {
    if message.starts_with("EventLogError:") {
        return error_response(rpc_id, -32603, message);
    }
    error_response(rpc_id, A2A_TASK_NOT_FOUND, message)
}

pub(super) fn push_config_topic() -> Topic {
    Topic::new(A2A_PUSH_CONFIG_TOPIC).expect("valid A2A push config topic")
}

pub(super) fn load_push_configs(
    log: &Arc<AnyEventLog>,
) -> HashMap<String, BTreeMap<String, JsonValue>> {
    futures::executor::block_on(async {
        let topic = push_config_topic();
        let mut store = HashMap::<String, BTreeMap<String, JsonValue>>::new();
        let mut cursor = None;
        loop {
            let events = match log.read_range(&topic, cursor, 512).await {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(
                        target: "harn_serve::a2a",
                        %error,
                        "failed to replay A2A push notification configs"
                    );
                    break;
                }
            };
            if events.is_empty() {
                break;
            }
            for (event_id, event) in events {
                apply_persisted_push_config_event(&mut store, event);
                cursor = Some(event_id);
            }
        }
        store
    })
}

pub(super) fn apply_persisted_push_config_event(
    store: &mut HashMap<String, BTreeMap<String, JsonValue>>,
    event: LogEvent,
) {
    let Some(task_id) = event.payload.get("taskId").and_then(JsonValue::as_str) else {
        return;
    };
    let Some(config_id) = event.payload.get("configId").and_then(JsonValue::as_str) else {
        return;
    };
    match event.kind.as_str() {
        A2A_PUSH_CONFIG_SET_KIND => {
            let Some(config) = event.payload.get("config").cloned() else {
                return;
            };
            store
                .entry(task_id.to_string())
                .or_default()
                .insert(config_id.to_string(), config);
        }
        A2A_PUSH_CONFIG_DELETE_KIND => {
            if let Some(configs) = store.get_mut(task_id) {
                configs.remove(config_id);
            }
        }
        _ => {}
    }
}

/// Soft-deprecation observer for the legacy `a2a-version` request header.
///
/// A2A 0.3.0 negotiates protocol version through AgentCard discovery, not via
/// request headers. We no longer reject requests carrying `a2a-version`; we
/// just log a warning so we can spot residual client usage during the
/// deprecation window. Slated for full removal one minor cycle after v0.7.x.
pub(super) fn log_legacy_version_header(headers: &HeaderMap) {
    if let Some(version) = headers
        .get(A2A_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        tracing::warn!(
            target: "harn_serve::a2a",
            requested_version = %version,
            supported_version = %A2A_PROTOCOL_VERSION,
            "a2a-version request header is deprecated; clients should negotiate via AgentCard discovery"
        );
    }
}

pub(super) fn return_immediately(params: &JsonValue) -> bool {
    params
        .pointer("/configuration/returnImmediately")
        .and_then(JsonValue::as_bool)
        .or_else(|| {
            params
                .pointer("/configuration/blocking")
                .and_then(JsonValue::as_bool)
                .map(|blocking| !blocking)
        })
        .unwrap_or(false)
}

pub(super) fn task_id_param(params: &JsonValue) -> Option<&str> {
    params
        .get("taskId")
        .or_else(|| params.get("task_id"))
        .or_else(|| params.get("id"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
}

pub(super) fn push_config_id_param(params: &JsonValue) -> Option<&str> {
    params
        .get("pushNotificationConfigId")
        .or_else(|| params.get("push_notification_config_id"))
        .or_else(|| params.get("configId"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
}

pub(super) fn message_parts(params: &JsonValue) -> Result<Vec<JsonValue>, A2aPrepareError> {
    let parts = params
        .pointer("/message/parts")
        .and_then(JsonValue::as_array)
        .map(|parts| {
            parts
                .iter()
                .enumerate()
                .map(|(index, part)| normalize_part(part, index))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    if let Some(parts) = parts {
        if !parts.is_empty() {
            return Ok(parts);
        }
    }
    Ok(vec![json!({
        "type": "text",
        "text": params
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
    })])
}

pub(super) fn message_text(params: &JsonValue, parts: &[JsonValue]) -> String {
    let text = parts
        .iter()
        .filter(|part| part_kind(part) == Some("text"))
        .filter_map(|part| part.get("text").and_then(JsonValue::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        params
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        text
    }
}

pub(super) fn normalize_part(part: &JsonValue, index: usize) -> Result<JsonValue, A2aPrepareError> {
    let Some(object) = part.as_object() else {
        return Err(A2aPrepareError::new(
            -32602,
            format!("A2A message part {index} must be an object"),
        ));
    };
    let kind = part_kind(part);
    match kind {
        Some("text") | None if object.contains_key("text") => {
            let text = part
                .get("text")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    A2aPrepareError::new(-32602, format!("A2A text part {index} requires text"))
                })?;
            let mut normalized = json!({"type": "text", "text": text});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some("file") | None if object.contains_key("file") || has_flat_file_fields(part) => {
            let file = normalize_file_part(part, index)?;
            let mut normalized = json!({"type": "file", "file": file});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some("data") | None if object.contains_key("data") => {
            let data = part.get("data").cloned().ok_or_else(|| {
                A2aPrepareError::new(-32602, format!("A2A data part {index} requires data"))
            })?;
            let mut normalized = json!({"type": "data", "data": data});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some(kind) => Err(A2aPrepareError::new(
            -32602,
            format!("unsupported A2A message part type '{kind}' at index {index}"),
        )),
        None => Err(A2aPrepareError::new(
            -32602,
            format!("A2A message part {index} requires text, file, or data content"),
        )),
    }
}

pub(super) fn part_kind(part: &JsonValue) -> Option<&str> {
    part.get("type")
        .or_else(|| part.get("kind"))
        .and_then(JsonValue::as_str)
}

pub(super) fn has_flat_file_fields(part: &JsonValue) -> bool {
    part.get("bytes").is_some() || part.get("uri").is_some()
}

pub(super) fn copy_optional_part_fields(source: &JsonValue, target: &mut JsonValue) {
    for field in ["metadata", "mediaType"] {
        if let Some(value) = source.get(field) {
            target[field] = value.clone();
        }
    }
}

pub(super) fn normalize_file_part(
    part: &JsonValue,
    index: usize,
) -> Result<JsonValue, A2aPrepareError> {
    let source = part.get("file").unwrap_or(part);
    let bytes = source
        .get("bytes")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty());
    let uri = source
        .get("uri")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty());
    match (bytes, uri) {
        (Some(_), Some(_)) => {
            return Err(A2aPrepareError::new(
                -32602,
                format!("A2A file part {index} must contain exactly one of bytes or uri"),
            ));
        }
        (None, None) => {
            return Err(A2aPrepareError::new(
                -32602,
                format!("A2A file part {index} requires bytes or uri"),
            ));
        }
        (Some(bytes), None) => {
            base64::engine::general_purpose::STANDARD
                .decode(bytes.as_bytes())
                .map_err(|error| {
                    A2aPrepareError::new(
                        -32602,
                        format!("A2A file part {index} bytes must be base64: {error}"),
                    )
                })?;
        }
        (None, Some(_)) => {}
    }

    let mut file = json!({});
    if let Some(bytes) = bytes {
        file["bytes"] = JsonValue::String(bytes.to_string());
    }
    if let Some(uri) = uri {
        file["uri"] = JsonValue::String(uri.to_string());
    }
    if let Some(mime_type) = source
        .get("mimeType")
        .or_else(|| source.get("mime_type"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["mimeType"] = JsonValue::String(mime_type.to_string());
    } else {
        file["mimeType"] = JsonValue::String("application/octet-stream".to_string());
    }
    if let Some(name) = source
        .get("name")
        .or_else(|| source.get("filename"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["name"] = JsonValue::String(name.to_string());
    }
    Ok(file)
}

pub(super) fn artifacts_from_parts(parts: &[JsonValue]) -> Vec<JsonValue> {
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| artifact_from_part(index, part))
        .collect()
}

pub(super) fn a2a_artifacts_from_parts(parts: &[JsonValue]) -> Vec<JsonValue> {
    artifacts_from_parts(parts)
        .iter()
        .map(a2a_artifact_from_harn_artifact)
        .collect()
}

pub(super) fn artifact_from_part(index: usize, part: &JsonValue) -> Option<JsonValue> {
    match part_kind(part)? {
        "file" => {
            let file = part.get("file")?;
            let id = part
                .pointer("/metadata/artifact_id")
                .or_else(|| part.pointer("/metadata/id"))
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("a2a-file-{index}"));
            Some(json!({
                "_type": "artifact",
                "id": id,
                "kind": "file",
                "title": file.get("name").and_then(JsonValue::as_str).unwrap_or("file"),
                "data": file,
                "metadata": {
                    "a2a_part_index": index,
                    "mimeType": file.get("mimeType").cloned().unwrap_or(JsonValue::Null)
                }
            }))
        }
        "data" => {
            let id = part
                .pointer("/metadata/artifact_id")
                .or_else(|| part.pointer("/metadata/id"))
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("a2a-data-{index}"));
            Some(json!({
                "_type": "artifact",
                "id": id,
                "kind": "data",
                "title": "structured data",
                "data": part.get("data").cloned().unwrap_or(JsonValue::Null),
                "metadata": {
                    "a2a_part_index": index
                }
            }))
        }
        _ => None,
    }
}

pub(super) fn message_argument_payload(
    params: &JsonValue,
    parts: &[JsonValue],
    text: &str,
) -> JsonValue {
    let mut message = params
        .get("message")
        .cloned()
        .unwrap_or_else(|| json!({"role": "user"}));
    message["parts"] = JsonValue::Array(parts.to_vec());
    if message.get("role").and_then(JsonValue::as_str).is_none() {
        message["role"] = JsonValue::String("user".to_string());
    }

    let mut payload = json!({
        "message": message,
        "parts": parts,
        "text": text,
        "artifacts": artifacts_from_parts(parts),
    });
    if let Some(context_id) = params.get("contextId").cloned() {
        payload["contextId"] = context_id;
    }
    payload
}

pub(super) fn param_accepts_structured_message(
    param: &crate::ExportedParam,
    parts: &[JsonValue],
) -> bool {
    let has_non_text = parts.iter().any(|part| part_kind(part) != Some("text"));
    if !has_non_text {
        return false;
    }
    param
        .type_expr
        .as_ref()
        .is_some_and(type_expr_accepts_json_object)
        || param.input_schema.get("type").and_then(JsonValue::as_str) == Some("object")
}

pub(super) fn type_expr_accepts_json_object(type_expr: &harn_parser::TypeExpr) -> bool {
    match type_expr {
        harn_parser::TypeExpr::Named(name) => name == "dict",
        harn_parser::TypeExpr::Shape(_) | harn_parser::TypeExpr::DictType(_, _) => true,
        harn_parser::TypeExpr::Union(types) | harn_parser::TypeExpr::Intersection(types) => {
            types.iter().any(type_expr_accepts_json_object)
        }
        _ => false,
    }
}

pub(super) fn caller_label(params: &JsonValue) -> String {
    params
        .pointer("/message/metadata/caller")
        .or_else(|| params.pointer("/metadata/caller"))
        .and_then(JsonValue::as_str)
        .unwrap_or("a2a-peer")
        .to_string()
}

pub(super) fn select_function(
    catalog: &ExportCatalog,
    params: &JsonValue,
) -> Result<String, A2aPrepareError> {
    for pointer in [
        "/function",
        "/skillId",
        "/message/metadata/function",
        "/message/metadata/skillId",
        "/message/metadata/target_agent",
        "/metadata/target_agent",
    ] {
        let Some(name) = params
            .pointer(pointer)
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let name = name.rsplit('/').next().unwrap_or(name);
        if catalog.function(name).is_some() {
            return Ok(name.to_string());
        }
    }

    for candidate in ["execute", "default", "main", "handle", "run"] {
        if catalog.function(candidate).is_some() {
            return Ok(candidate.to_string());
        }
    }
    if catalog.functions.len() == 1 {
        return Ok(catalog
            .functions
            .keys()
            .next()
            .expect("one function")
            .clone());
    }
    Err(A2aPrepareError::new(
        -32602,
        "A2A task must identify an exported function when multiple functions are exported",
    ))
}

pub(super) fn message_arguments(
    function: &crate::ExportedFunction,
    params: &JsonValue,
    parts: &[JsonValue],
    text: &str,
) -> Result<CallArguments, A2aPrepareError> {
    if let Some(arguments) = params
        .get("arguments")
        .or_else(|| params.pointer("/message/metadata/arguments"))
    {
        return json_arguments(arguments.clone());
    }

    if function.params.is_empty() {
        return Ok(CallArguments::Positional(Vec::new()));
    }

    let target_param = ["task", "message", "input"]
        .iter()
        .find_map(|name| function.params.iter().find(|param| param.name == *name))
        .or_else(|| (function.params.len() == 1).then(|| &function.params[0]));
    let Some(param) = target_param else {
        return Err(A2aPrepareError::new(
            -32602,
            "A2A task text can only be inferred for a single-argument export or a task/message/input parameter",
        ));
    };
    let value = if param_accepts_structured_message(param, parts) {
        message_argument_payload(params, parts, text)
    } else {
        JsonValue::String(text.to_string())
    };
    Ok(CallArguments::Named(BTreeMap::from([(
        param.name.clone(),
        value,
    )])))
}

pub(super) fn json_arguments(value: JsonValue) -> Result<CallArguments, A2aPrepareError> {
    match value {
        JsonValue::Null => Ok(CallArguments::Named(BTreeMap::new())),
        JsonValue::Object(values) => Ok(CallArguments::Named(values.into_iter().collect())),
        JsonValue::Array(values) => Ok(CallArguments::Positional(values)),
        _ => Err(A2aPrepareError::new(
            -32602,
            "A2A arguments must be an object, array, or null",
        )),
    }
}

pub(super) fn response_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

pub(super) fn response_parts(value: &JsonValue) -> Vec<JsonValue> {
    for pointer in ["/parts", "/message/parts", "/result/parts"] {
        if let Some(parts) = value.pointer(pointer).and_then(JsonValue::as_array) {
            let normalized = parts
                .iter()
                .enumerate()
                .filter_map(|(index, part)| normalize_part(part, index).ok())
                .collect::<Vec<_>>();
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    let mut parts = Vec::new();
    if let Some(text) = value
        .get("visible_text")
        .or_else(|| value.get("text"))
        .and_then(JsonValue::as_str)
        .filter(|text| !text.is_empty())
    {
        parts.push(json!({"type": "text", "text": text}));
    }

    for artifact in artifacts_in_value(value) {
        if let Some(part) = part_from_artifact(artifact) {
            parts.push(part);
        }
    }

    if parts.is_empty() {
        parts.push(json!({"type": "text", "text": response_text(value)}));
    }
    parts
}

pub(super) fn response_artifacts(value: &JsonValue, parts: &[JsonValue]) -> Vec<JsonValue> {
    let artifacts = artifacts_in_value(value)
        .into_iter()
        .map(a2a_artifact_from_harn_artifact)
        .collect::<Vec<_>>();
    if artifacts.is_empty() {
        a2a_artifacts_from_parts(parts)
    } else {
        artifacts
    }
}

pub(super) fn artifacts_in_value(value: &JsonValue) -> Vec<&JsonValue> {
    let mut artifacts = Vec::new();
    if is_harn_artifact(value) {
        artifacts.push(value);
    }
    for pointer in ["/artifacts", "/run/artifacts", "/result/artifacts"] {
        if let Some(items) = value.pointer(pointer).and_then(JsonValue::as_array) {
            artifacts.extend(items.iter().filter(|item| is_harn_artifact(item)));
        }
    }
    artifacts
}

pub(super) fn is_harn_artifact(value: &JsonValue) -> bool {
    value.get("_type").and_then(JsonValue::as_str) == Some("artifact")
        || value.get("kind").and_then(JsonValue::as_str).is_some()
            && (value.get("data").is_some()
                || value.get("text").is_some()
                || value.get("metadata").is_some())
}

pub(super) fn part_from_artifact(artifact: &JsonValue) -> Option<JsonValue> {
    let kind = artifact
        .get("kind")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if let Some(file) = file_part_from_artifact(artifact, kind) {
        return Some(file);
    }
    if kind == "data" || kind == "handoff" {
        return Some(json!({
            "type": "data",
            "data": artifact.get("data").cloned().unwrap_or_else(|| artifact.clone()),
            "metadata": {
                "artifact_id": artifact.get("id").cloned().unwrap_or(JsonValue::Null),
                "artifact_kind": kind,
            }
        }));
    }
    None
}

pub(super) fn file_part_from_artifact(artifact: &JsonValue, kind: &str) -> Option<JsonValue> {
    let data = artifact.get("data");
    let metadata = artifact.get("metadata");
    let bytes = data
        .and_then(|data| data.get("bytes").or_else(|| data.get("bytes_base64")))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if matches!(kind, "workspace_file" | "file") {
                data.and_then(|data| data.get("content"))
                    .or_else(|| artifact.get("text"))
                    .and_then(JsonValue::as_str)
                    .map(|content| {
                        base64::engine::general_purpose::STANDARD.encode(content.as_bytes())
                    })
            } else {
                None
            }
        });
    let uri = data
        .and_then(|data| data.get("uri").or_else(|| data.get("url")))
        .or_else(|| {
            metadata.and_then(|metadata| metadata.get("uri").or_else(|| metadata.get("url")))
        })
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if bytes.is_none() && uri.is_none() && !matches!(kind, "file" | "workspace_file") {
        return None;
    }
    let mut file = json!({});
    if let Some(bytes) = bytes {
        file["bytes"] = JsonValue::String(bytes);
    } else if let Some(uri) = uri {
        file["uri"] = JsonValue::String(uri);
    } else {
        return None;
    }
    file["mimeType"] = JsonValue::String(
        data.and_then(|data| data.get("mimeType").or_else(|| data.get("mime_type")))
            .or_else(|| {
                metadata.and_then(|metadata| {
                    metadata
                        .get("mimeType")
                        .or_else(|| metadata.get("mime_type"))
                })
            })
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(if kind == "workspace_file" {
                "text/plain"
            } else {
                "application/octet-stream"
            })
            .to_string(),
    );
    if let Some(name) = data
        .and_then(|data| data.get("name").or_else(|| data.get("filename")))
        .or_else(|| metadata.and_then(|metadata| metadata.get("path")))
        .or_else(|| artifact.get("title"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["name"] = JsonValue::String(name.to_string());
    }
    Some(json!({
        "type": "file",
        "file": file,
        "metadata": {
            "artifact_id": artifact.get("id").cloned().unwrap_or(JsonValue::Null),
            "artifact_kind": kind,
        }
    }))
}

pub(super) fn a2a_artifact_from_harn_artifact(artifact: &JsonValue) -> JsonValue {
    let part = part_from_artifact(artifact).unwrap_or_else(|| {
        json!({
            "type": "data",
            "data": artifact,
        })
    });
    let mut value = json!({
        "artifactId": artifact
            .get("id")
            .and_then(JsonValue::as_str)
            .unwrap_or("artifact"),
        "name": artifact
            .get("title")
            .or_else(|| artifact.get("kind"))
            .and_then(JsonValue::as_str)
            .unwrap_or("artifact"),
        "parts": [part],
    });
    let mut metadata = artifact
        .get("metadata")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    metadata
        .entry("timestamp")
        .or_insert_with(|| JsonValue::String(current_timestamp_rfc3339()));
    if !metadata.contains_key("artifact_kind") {
        if let Some(kind) = artifact.get("kind").and_then(JsonValue::as_str) {
            metadata.insert(
                "artifact_kind".to_string(),
                JsonValue::String(kind.to_string()),
            );
        }
    }
    value["metadata"] = JsonValue::Object(metadata);
    value
}

/// Current wall-clock time as an RFC3339 / ISO-8601 UTC string. Used to
/// stamp each A2A `Artifact.metadata.timestamp` so downstream consumers
/// can order outputs even when several artifacts share a task.
pub(super) fn current_timestamp_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::new())
}

/// Wrap a tool call's `raw_output` in an A2A `Artifact`. The stable id
/// is derived from the model-issued `tool_call_id` so a streaming
/// `artifact-update` event and the artifact stored on the task share
/// the same identity. String outputs become a single text part; every
/// other JSON shape becomes a single data part — matching what
/// `response_parts` would produce for the same value at task
/// completion.
pub(super) fn tool_output_artifact(
    tool_call_id: &str,
    tool_name: &str,
    output: &JsonValue,
) -> JsonValue {
    let part = match output {
        JsonValue::String(text) => json!({"type": "text", "text": text}),
        _ => json!({"type": "data", "data": output.clone()}),
    };
    json!({
        "artifactId": format!("tool-{tool_call_id}"),
        "name": tool_name,
        "parts": [part],
        "metadata": {
            "timestamp": current_timestamp_rfc3339(),
            "tool_call_id": tool_call_id,
            "artifact_kind": "tool_output",
        }
    })
}

pub(super) fn public_skill_card(function: &crate::ExportedFunction) -> JsonValue {
    json!({
        "id": function.name,
        "name": function.name,
        "description": format!("Invoke exported Harn function '{}'.", function.name),
        "tags": ["harn", "function"],
        "examples": [],
        "inputModes": ["application/json", "text/plain", "application/octet-stream"],
        "outputModes": ["application/json", "text/plain", "application/octet-stream"],
        "inputSchema": function.input_schema,
    })
}

pub(super) fn extended_skill_card(function: &crate::ExportedFunction) -> JsonValue {
    let mut card = public_skill_card(function);
    if let Some(object) = card.as_object_mut() {
        object.insert(
            "description".to_string(),
            JsonValue::String(format!(
                "Invoke exported Harn function '{}'. Includes detailed schemas for authenticated callers.",
                function.name
            )),
        );
        // The output schema is not currently introspected from the
        // typed return value of an exported Harn function. Emit an
        // empty object so authenticated tooling can rely on the field
        // being present even when the schema is unknown.
        object.insert("outputSchema".to_string(), json!({}));
    }
    card
}

pub(super) fn policy_security_schemes(policy: &AuthPolicy) -> (JsonValue, JsonValue) {
    let mut schemes = serde_json::Map::new();
    let mut requirements: Vec<JsonValue> = Vec::new();
    for method in &policy.methods {
        match method {
            AuthMethodConfig::ApiKey(_) => {
                schemes.insert(
                    "apiKey".to_string(),
                    json!({
                        "type": "apiKey",
                        "in": "header",
                        "name": "Authorization",
                        "description": "API key supplied as `Authorization: Bearer <key>` or `X-API-Key`.",
                    }),
                );
                requirements.push(json!({"apiKey": []}));
            }
            AuthMethodConfig::Hmac(config) => {
                schemes.insert(
                    "hmac".to_string(),
                    json!({
                        "type": "http",
                        "scheme": "HMAC-SHA256",
                        "description": format!(
                            "HMAC-SHA256 canonical request signature (provider '{}').",
                            config.provider
                        ),
                    }),
                );
                requirements.push(json!({"hmac": []}));
            }
            AuthMethodConfig::OAuth21(config) => {
                let mut scheme = json!({
                    "type": "oauth2",
                    "description": "OAuth 2.1 access token validated by the transport.",
                });
                if let Some(object) = scheme.as_object_mut() {
                    object.insert(
                        "issuer".to_string(),
                        JsonValue::String(config.issuer.clone()),
                    );
                    if let Some(audience) = config.audience.as_ref() {
                        object.insert("audience".to_string(), JsonValue::String(audience.clone()));
                    }
                }
                schemes.insert("oauth2".to_string(), scheme);
                let scopes = config
                    .required_scopes
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect::<Vec<_>>();
                requirements.push(json!({"oauth2": scopes}));
            }
        }
    }
    (JsonValue::Object(schemes), JsonValue::Array(requirements))
}

pub(super) fn www_authenticate_header(policy: &AuthPolicy) -> HeaderValue {
    let mut schemes = Vec::new();
    for method in &policy.methods {
        match method {
            AuthMethodConfig::ApiKey(_) | AuthMethodConfig::OAuth21(_) => {
                if !schemes.contains(&"Bearer") {
                    schemes.push("Bearer");
                }
            }
            AuthMethodConfig::Hmac(_) => {
                if !schemes.contains(&"HMAC-SHA256") {
                    schemes.push("HMAC-SHA256");
                }
            }
        }
    }
    if schemes.is_empty() {
        schemes.push("Bearer");
    }
    let value = schemes
        .into_iter()
        .map(|scheme| format!("{scheme} realm=\"{A2A_AUTH_REALM}\""))
        .collect::<Vec<_>>()
        .join(", ");
    HeaderValue::from_str(&value)
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer realm=\"harn-a2a\""))
}

pub(super) fn http_auth_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    headers: &HeaderMap,
) -> AuthRequest {
    AuthRequest {
        method: method.as_str().to_string(),
        path: path.to_string(),
        body,
        headers: normalized_headers(headers),
        validated_oauth: None,
    }
}

pub(super) fn normalized_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

pub(super) fn sign_card(card: &mut JsonValue, secret: &str) {
    let Ok(bytes) = serde_json::to_vec(card) else {
        return;
    };
    let protected = json!({
        "alg": "HS256",
        "typ": "JOSE",
        "kid": "harn-serve",
    });
    let Ok(protected_bytes) = serde_json::to_vec(&protected) else {
        return;
    };
    let protected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(protected_bytes);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return;
    };
    mac.update(format!("{protected}.{payload}").as_bytes());
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    card["signatures"] = json!([{
        "protected": protected,
        "signature": signature,
    }]);
}

pub(super) fn derived_agent_name(catalog: &ExportCatalog) -> String {
    catalog
        .script_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("harn-serve")
        .to_string()
}

/// Agent-session id used by the A2A adapter when scoping worker events
/// to a task. Prefixed so it can't collide with a user-supplied
/// session id and so the sink registry can be inspected for A2A entries
/// in tests.
pub(super) fn a2a_worker_session_id(task_id: &str) -> String {
    format!("a2a:{task_id}")
}

/// `AgentEventSink` implementation that publishes worker lifecycle
/// updates and structured plan emissions onto an A2A task's event
/// stream. Chat/tool chunks are deliberately ignored here; they belong
/// to the task history or ACP stream rather than this extension feed.
pub(super) struct A2aPrepareError {
    code: i64,
    message: String,
}

impl A2aPrepareError {
    pub(super) fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(super) fn with_id(self, rpc_id: JsonValue) -> JsonValue {
        error_response(rpc_id, self.code, &self.message)
    }
}
