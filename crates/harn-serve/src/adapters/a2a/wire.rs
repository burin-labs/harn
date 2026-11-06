//! Versioned JSON-RPC boundary over the shared task lifecycle.
use super::*;
use harn_vm::a2a::A2aWireVersion;
use serde::{Deserialize, Serialize};
mod listing;
pub(super) use listing::ListQuery;

pub(super) const UNSUPPORTED_OPERATION: i64 = -32004;
const PUSH_NOT_SUPPORTED: i64 = -32003;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendParameters {
    message: IncomingMessage,
    #[serde(default)]
    configuration: Option<SendConfiguration>,
    #[serde(default)]
    metadata: Option<serde_json::Map<String, JsonValue>>,
    #[serde(default)]
    tenant: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IncomingMessage {
    message_id: String,
    role: String,
    parts: Vec<JsonValue>,
    #[serde(default)]
    context_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Map<String, JsonValue>>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    reference_task_ids: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendConfiguration {
    #[serde(default)]
    accepted_output_modes: Vec<String>,
    #[serde(default)]
    history_length: Option<i32>,
    #[serde(default)]
    return_immediately: bool,
    #[serde(default)]
    task_push_notification_config: Option<JsonValue>,
}

pub(super) fn requested_version(headers: &HeaderMap) -> Option<A2aWireVersion> {
    let mut values = headers.get_all(A2A_VERSION_HEADER).iter();
    let Some(value) = values.next() else {
        return Some(A2aWireVersion::V0_3);
    };
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?.trim();
    if value.is_empty() {
        return Some(A2aWireVersion::V0_3);
    }
    A2aWireVersion::from_advertised(value)
}

#[derive(Clone, Copy)]
pub(super) enum Operation {
    Send,
    Stream,
    Get,
    Cancel,
    Subscribe,
    ExtendedCard,
    List,
}

impl Operation {
    const ALL: &[Self] = &[
        Self::Send,
        Self::Stream,
        Self::Get,
        Self::Cancel,
        Self::Subscribe,
        Self::ExtendedCard,
        Self::List,
    ];

    fn from_v1(method: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.v1_method() == method)
    }

    fn v1_method(self) -> &'static str {
        match self {
            Self::Send => "SendMessage",
            Self::Stream => "SendStreamingMessage",
            Self::Get => "GetTask",
            Self::Cancel => "CancelTask",
            Self::Subscribe => "SubscribeToTask",
            Self::ExtendedCard => "GetExtendedAgentCard",
            Self::List => "ListTasks",
        }
    }

    fn legacy_method(self) -> &'static str {
        match self {
            Self::Send => "message/send",
            Self::Stream => "message/stream",
            Self::Get => "tasks/get",
            Self::Cancel => "tasks/cancel",
            Self::Subscribe => "tasks/resubscribe",
            Self::ExtendedCard => "agent/getAuthenticatedExtendedCard",
            Self::List => "tasks/list",
        }
    }
}

pub(super) fn v1_methods() -> Vec<&'static str> {
    Operation::ALL
        .iter()
        .map(|operation| operation.v1_method())
        .collect()
}

pub(super) struct V1Request {
    operation: Operation,
    id: JsonValue,
    history_length: Option<usize>,
    pub(super) listing: Option<ListQuery>,
}

impl V1Request {
    pub(super) fn decode(request: &mut JsonValue) -> Result<Self, JsonValue> {
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        if request["jsonrpc"] != "2.0"
            || !(id.is_null() || id.is_number() || id.is_string())
            || !request.get("method").is_some_and(JsonValue::is_string)
        {
            return Err(schema::error_response(
                JsonValue::Null,
                -32600,
                "Invalid JSON-RPC request",
            ));
        }
        let invalid = |message: &str| schema::error_response(id.clone(), -32602, message);
        let method = request["method"]
            .as_str()
            .expect("validated JSON-RPC method");
        let operation = Operation::from_v1(method).ok_or_else(|| {
            let code = match method {
                "CreateTaskPushNotificationConfig"
                | "GetTaskPushNotificationConfig"
                | "ListTaskPushNotificationConfigs"
                | "DeleteTaskPushNotificationConfig" => PUSH_NOT_SUPPORTED,
                _ => -32601,
            };
            schema::error_response(id.clone(), code, "A2A 1.0 method not supported")
        })?;
        let params = request
            .as_object_mut()
            .ok_or_else(|| invalid("Expected JSON-RPC object"))?
            .entry("params")
            .or_insert_with(|| json!({}));
        if !params.is_object() {
            return Err(invalid("Expected object parameters"));
        }
        if let Some(tenant) = params.get("tenant").filter(|value| !value.is_null()) {
            let tenant = tenant
                .as_str()
                .ok_or_else(|| invalid("tenant must be a string"))?;
            if !tenant.is_empty() {
                return Err(invalid("This interface does not support tenant routing"));
            }
        }
        let limit = match operation {
            Operation::Send | Operation::Stream => params.pointer("/configuration/historyLength"),
            _ => params.get("historyLength"),
        };
        let history_length = limit
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= i32::MAX as usize)
                    .ok_or_else(|| invalid("historyLength must be a nonnegative int32"))
            })
            .transpose()?;
        match operation {
            Operation::Send | Operation::Stream => {
                let mut send: SendParameters =
                    serde_json::from_value(params.clone()).map_err(|error| {
                        invalid(&format!("Invalid A2A 1.0 send parameters: {error}"))
                    })?;
                let message = &mut send.message;
                if message.role != "ROLE_USER" || message.message_id.is_empty() {
                    return Err(invalid("Expected ROLE_USER and a nonempty messageId"));
                }
                if message.parts.is_empty() {
                    return Err(invalid("message.parts must not be empty"));
                }
                for part in &mut message.parts {
                    decode_part(part).map_err(&invalid)?;
                }
                message.role = "user".to_owned();
                let context = message
                    .context_id
                    .clone()
                    .unwrap_or_else(|| Uuid::now_v7().to_string());
                if send
                    .configuration
                    .as_ref()
                    .is_some_and(|config| config.task_push_notification_config.is_some())
                {
                    return Err(schema::error_response(
                        id.clone(),
                        PUSH_NOT_SUPPORTED,
                        "A2A 1.0 push notifications are not supported",
                    ));
                }
                if context.is_empty() {
                    return Err(invalid("contextId must be a nonempty string when supplied"));
                }
                *params = serde_json::to_value(send).expect("JSON send parameters");
                params["contextId"] = json!(context);
            }
            Operation::Get | Operation::Cancel | Operation::Subscribe => {
                if params["id"].as_str().is_none_or(str::is_empty) {
                    return Err(invalid("id is required"));
                }
            }
            Operation::ExtendedCard | Operation::List => {}
        }
        let listing = matches!(operation, Operation::List)
            .then(|| ListQuery::decode(params))
            .transpose()
            .map_err(invalid)?;
        request["method"] = json!(operation.legacy_method());
        Ok(Self {
            operation,
            id,
            history_length,
            listing,
        })
    }

    pub(super) fn encode(
        self,
        mut processed: ProcessedRpc,
        server: Arc<A2aServer>,
    ) -> ProcessedRpc {
        match &mut processed.outcome {
            RpcOutcome::Json(response) => {
                if matches!(self.operation, Operation::ExtendedCard)
                    && response["error"]["code"] == A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED
                {
                    response["error"]["code"] = json!(UNSUPPORTED_OPERATION);
                }
                if let Some(result) = response.get_mut("result") {
                    match self.operation {
                        Operation::Send => {
                            limit_history(result, self.history_length);
                            *result = json!({"task": std::mem::take(result)});
                        }
                        Operation::Get | Operation::Cancel => {
                            limit_history(result, self.history_length);
                        }
                        Operation::ExtendedCard => {
                            encode_card(result);
                            if let Some(secret) = server.card_signing_secret.as_deref() {
                                schema::sign_card(result, secret);
                            }
                        }
                        _ => {}
                    }
                }
            }
            RpcOutcome::Sse(receiver) => {
                let mut source = std::mem::replace(receiver, transport::empty_stream());
                let (sender, target) = unbounded();
                let id = self.id;
                tokio::spawn(async move {
                    while let Some(event) = source.next().await {
                        let event = &event["result"];
                        if let Some(task) = event.get("task") {
                            let mut task = task.clone();
                            limit_history(&mut task, self.history_length);
                            let payload = json!({"task": task});
                            if sender
                                .unbounded_send(harn_vm::jsonrpc::response(id.clone(), payload))
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        let task_id = event["taskId"].as_str().unwrap_or_default();
                        let task = server.task_json(task_id);
                        let context = task["contextId"].clone();
                        let payload = if let Some(status) =
                            event.get("status").filter(|value| value.is_object())
                        {
                            let mut update =
                                json!({"taskId": task_id, "contextId": context, "status": status});
                            if let Some(error) = event.get("error").and_then(JsonValue::as_str) {
                                update["status"]["message"] = json!({"id": Uuid::now_v7().to_string(), "role": "agent", "parts": [{"text": error}]});
                            }
                            if let Some(metadata) = event.get("metadata") {
                                update["metadata"] = metadata.clone();
                            }
                            encode_status(&mut update["status"], task_id, &context);
                            json!({"statusUpdate": update})
                        } else if let Some(artifact) = event.get("artifact") {
                            let mut artifact = artifact.clone();
                            encode_artifact(&mut artifact);
                            json!({"artifactUpdate": {"taskId": task_id, "contextId": context,
                                "artifact": artifact, "append": event["append"], "lastChunk": event["lastChunk"]}})
                        } else if let Some(message) =
                            event.get("message").filter(|value| value.is_object())
                        {
                            let mut message = message.clone();
                            encode_message(&mut message, task_id, &context);
                            json!({"statusUpdate": {"taskId": task_id, "contextId": context, "status": {"state": "TASK_STATE_WORKING", "message": message}}})
                        } else {
                            json!({"statusUpdate": {"taskId": task_id, "contextId": context, "status": {"state": "TASK_STATE_WORKING"}, "metadata": {"harn": {"event": event}}}})
                        };
                        if sender
                            .unbounded_send(harn_vm::jsonrpc::response(id.clone(), payload))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                *receiver = target;
            }
        }
        processed
    }
}

fn decode_part(part: &mut JsonValue) -> Result<(), &'static str> {
    let object = part.as_object_mut().ok_or("Part must be an object")?;
    if object.keys().any(|key| {
        ![
            "text",
            "raw",
            "url",
            "data",
            "metadata",
            "filename",
            "mediaType",
        ]
        .contains(&key.as_str())
    }) {
        return Err("Unknown A2A 1.0 part field");
    }
    if object
        .get("metadata")
        .is_some_and(|value| !value.is_object())
        || ["filename", "mediaType"]
            .into_iter()
            .any(|key| object.get(key).is_some_and(|value| !value.is_string()))
    {
        return Err("Invalid A2A 1.0 part metadata or media fields");
    }
    let content: Vec<_> = ["text", "raw", "url", "data"]
        .into_iter()
        .filter(|key| object.contains_key(*key))
        .collect();
    if content.len() != 1 {
        return Err("Part must have exactly one content field");
    }
    let key = content[0];
    if key != "data" && !object[key].is_string() {
        return Err("Text, raw and URL content must be strings");
    }
    if key == "raw" {
        let bytes = harn_vm::a2a::decode_raw_bytes(object[key].as_str().unwrap_or_default())
            .map_err(|_| "Raw content must be base64")?;
        object["raw"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
    }
    if matches!(key, "raw" | "url") {
        let content = object.remove(key).expect("validated content");
        let mut file = json!({if key == "raw" { "bytes" } else { "uri" }: content});
        if let Some(name) = object.remove("filename") {
            file["name"] = name;
        }
        if let Some(media) = object.remove("mediaType") {
            file["mimeType"] = media;
        }
        object.insert("file".into(), file);
        object.insert("type".into(), json!("file"));
    } else {
        object.insert("type".into(), json!(key));
    }
    Ok(())
}

fn encode_parts(value: &mut JsonValue) {
    let Some(parts) = value.as_array_mut() else {
        return;
    };
    for part in parts {
        let Some(object) = part.as_object_mut() else {
            continue;
        };
        object.remove("kind");
        object.remove("type");
        if let Some(file) = object.remove("file") {
            if let Some(bytes) = file.get("bytes") {
                object.insert("raw".into(), bytes.clone());
            }
            if let Some(uri) = file.get("uri") {
                object.insert("url".into(), uri.clone());
            }
            if let Some(name) = file.get("name") {
                object.insert("filename".into(), name.clone());
            }
            if let Some(media) = file.get("mimeType") {
                object.insert("mediaType".into(), media.clone());
            }
        }
    }
}

fn encode_message(value: &mut JsonValue, task_id: &str, context: &JsonValue) {
    if let Some(object) = value.as_object_mut() {
        object.remove("kind");
    }
    if let Some(id) = value.as_object_mut().and_then(|o| o.remove("id")) {
        value["messageId"] = id;
    }
    value["role"] = json!(if value["role"] == "user" {
        "ROLE_USER"
    } else {
        "ROLE_AGENT"
    });
    value["taskId"] = json!(task_id);
    value["contextId"] = context.clone();
    encode_parts(&mut value["parts"]);
}

pub(super) fn state_name(state: &str) -> &'static str {
    match state {
        "submitted" => "TASK_STATE_SUBMITTED",
        "working" => "TASK_STATE_WORKING",
        "completed" => "TASK_STATE_COMPLETED",
        "failed" => "TASK_STATE_FAILED",
        "cancelled" | "canceled" => "TASK_STATE_CANCELED",
        "rejected" => "TASK_STATE_REJECTED",
        "input-required" => "TASK_STATE_INPUT_REQUIRED",
        "auth-required" => "TASK_STATE_AUTH_REQUIRED",
        _ => "TASK_STATE_UNSPECIFIED",
    }
}

fn encode_status(status: &mut JsonValue, task_id: &str, context: &JsonValue) {
    status["state"] = json!(state_name(status["state"].as_str().unwrap_or_default()));
    if let Some(message) = status.get_mut("message") {
        encode_message(message, task_id, context);
    }
}

fn encode_artifact(artifact: &mut JsonValue) {
    encode_parts(&mut artifact["parts"]);
}

pub(super) fn encode_task(value: &JsonValue, limit: Option<usize>) -> JsonValue {
    let mut task = value.clone();
    if let Some(object) = task.as_object_mut() {
        object.remove("kind");
    }
    let id = task["id"].as_str().unwrap_or_default().to_owned();
    let context = task["contextId"].clone();
    task["contextId"] = context.clone();
    encode_status(&mut task["status"], &id, &context);
    if let Some(history) = task.get_mut("history").and_then(JsonValue::as_array_mut) {
        for message in history {
            encode_message(message, &id, &context);
        }
    }
    if let Some(artifacts) = task.get_mut("artifacts").and_then(JsonValue::as_array_mut) {
        for artifact in artifacts {
            encode_artifact(artifact);
        }
    }
    limit_history(&mut task, limit);
    task
}

fn limit_history(task: &mut JsonValue, limit: Option<usize>) {
    if let (Some(history), Some(limit)) = (
        task.get_mut("history").and_then(JsonValue::as_array_mut),
        limit,
    ) {
        history.drain(..history.len().saturating_sub(limit));
    }
}

pub(super) fn encode_card(card: &mut JsonValue) {
    let url = card["url"].clone();
    let object = card.as_object_mut().expect("agent card object");
    for key in [
        "protocolVersion",
        "url",
        "preferredTransport",
        "additionalInterfaces",
        "signatures",
        "metadata",
    ] {
        object.remove(key);
    }
    object.insert(
        "supportedInterfaces".into(),
        json!([
            {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
            {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "0.3.0"}
        ]),
    );
    if let Some(extended) = object.remove("supportsAuthenticatedExtendedCard") {
        card["capabilities"]["extendedAgentCard"] = extended;
    }
    card["capabilities"]["pushNotifications"] = json!(false);
    if let Some(skills) = card.get_mut("skills").and_then(JsonValue::as_array_mut) {
        for skill in skills {
            if let Some(skill) = skill.as_object_mut() {
                skill.remove("inputSchema");
                skill.remove("outputSchema");
            }
        }
    }
    if let Some(schemes) = card
        .get_mut("securitySchemes")
        .and_then(JsonValue::as_object_mut)
    {
        for scheme in schemes.values_mut() {
            let kind = scheme.as_object_mut().and_then(|s| s.remove("type"));
            let key = match kind.as_ref().and_then(JsonValue::as_str) {
                Some("apiKey") => {
                    if let Some(location) = scheme.as_object_mut().and_then(|s| s.remove("in")) {
                        scheme["location"] = location;
                    }
                    "apiKeySecurityScheme"
                }
                Some("http") => "httpAuthSecurityScheme",
                Some("oauth2") => {
                    *scheme = json!({"scheme": "Bearer", "bearerFormat": "JWT", "description": "OAuth access token validated by the transport"});
                    "httpAuthSecurityScheme"
                }
                Some("openIdConnect") => "openIdConnectSecurityScheme",
                Some("mutualTLS") => "mtlsSecurityScheme",
                _ => continue,
            };
            *scheme = json!({key: scheme.clone()});
        }
    }
    let security = card
        .as_object_mut()
        .and_then(|o| o.remove("security"))
        .unwrap_or_else(|| json!([]));
    card["securityRequirements"] = json!(security
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|requirement| {
            let schemes: serde_json::Map<String, JsonValue> = requirement
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, scopes)| (key.clone(), json!({"list": scopes})))
                .collect();
            json!({"schemes": schemes})
        })
        .collect::<Vec<_>>());
}
