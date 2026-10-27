//! Version-specific A2A JSON-RPC wire decisions.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::A2aClientError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum A2aWireVersion {
    #[default]
    #[serde(rename = "0.3")]
    V0_3,
    #[serde(rename = "1.0")]
    V1_0,
}

impl A2aWireVersion {
    pub fn is_legacy(&self) -> bool {
        *self == Self::V0_3
    }

    pub fn from_advertised(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let major = parts.next()?;
        let minor = parts.next()?;
        if let Some(patch) = parts.next() {
            if patch.is_empty() || !patch.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
        }
        if parts.next().is_some() {
            return None;
        }
        match (major, minor) {
            ("0", "3") => Some(Self::V0_3),
            ("1", "0") => Some(Self::V1_0),
            _ => None,
        }
    }

    pub fn header(self) -> &'static str {
        match self {
            Self::V0_3 => super::A2A_PROTOCOL_VERSION,
            Self::V1_0 => "1.0",
        }
    }

    pub fn send_method(self) -> &'static str {
        match self {
            Self::V0_3 => "message/send",
            Self::V1_0 => "SendMessage",
        }
    }

    pub fn role_user(self) -> &'static str {
        match self {
            Self::V0_3 => "user",
            Self::V1_0 => "ROLE_USER",
        }
    }

    pub(super) fn task_from_send_response<'a>(
        self,
        value: &'a Value,
    ) -> Result<SendResponse<'a>, A2aClientError> {
        if self == Self::V0_3 {
            return Ok(SendResponse::Task(value));
        }
        match (value.get("task"), value.get("message")) {
            (Some(task), None) if task.is_object() => Ok(SendResponse::Task(task)),
            (None, Some(message)) if message.is_object() => Ok(SendResponse::Message(message)),
            _ => Err(A2aClientError::Protocol(
                "A2A 1.0 send response must contain exactly one task or message".to_string(),
            )),
        }
    }

    pub fn task_state<'a>(self, state: &'a str) -> Result<&'a str, A2aClientError> {
        if self == Self::V0_3 {
            return Ok(state);
        }
        match state {
            "TASK_STATE_SUBMITTED" => Ok("submitted"),
            "TASK_STATE_WORKING" => Ok("working"),
            "TASK_STATE_COMPLETED" => Ok("completed"),
            "TASK_STATE_FAILED" => Ok("failed"),
            "TASK_STATE_CANCELED" => Ok("canceled"),
            "TASK_STATE_INPUT_REQUIRED" => Ok("input-required"),
            "TASK_STATE_REJECTED" => Ok("rejected"),
            "TASK_STATE_AUTH_REQUIRED" => Ok("auth-required"),
            _ => Err(A2aClientError::Protocol(format!(
                "unknown A2A 1.0 task state: {state}"
            ))),
        }
    }
}

pub(super) enum SendResponse<'a> {
    Task(&'a Value),
    Message(&'a Value),
}

pub(super) fn push_configuration(
    version: A2aWireVersion,
    tenant: Option<&str>,
    mut config: Value,
) -> Value {
    match version {
        A2aWireVersion::V0_3 => serde_json::json!({
            "blocking": false,
            "returnImmediately": true,
            "pushNotificationConfig": config,
        }),
        A2aWireVersion::V1_0 => {
            if let Some(tenant) = tenant {
                config["tenant"] = serde_json::json!(tenant);
            }
            serde_json::json!({
                "returnImmediately": true,
                "taskPushNotificationConfig": config,
            })
        }
    }
}

fn single_part_result(parts: &[Value]) -> Option<Value> {
    let [part] = parts else {
        return None;
    };
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        return Some(
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())),
        );
    }
    part.get("data").cloned()
}

fn validate_parts(value: Option<&Value>) -> Result<&[Value], A2aClientError> {
    let parts = value
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| A2aClientError::Protocol("A2A response must contain parts".to_string()))?;
    for part in parts {
        let object = part
            .as_object()
            .ok_or_else(|| A2aClientError::Protocol("A2A part must be an object".to_string()))?;
        let content: Vec<_> = ["text", "raw", "url", "data"]
            .into_iter()
            .filter(|key| object.contains_key(*key))
            .collect();
        if content.len() != 1 {
            return Err(A2aClientError::Protocol(
                "A2A part must contain exactly one content field".to_string(),
            ));
        }
        let value = &object[content[0]];
        if content[0] != "data" && !value.is_string() {
            return Err(A2aClientError::Protocol(
                "A2A text, raw and URL content must be strings".to_string(),
            ));
        }
        if content[0] == "raw" {
            let bytes = value.as_str().unwrap();
            let engines = [
                &base64::engine::general_purpose::STANDARD,
                &base64::engine::general_purpose::STANDARD_NO_PAD,
                &base64::engine::general_purpose::URL_SAFE,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            ];
            if !engines.iter().any(|engine| engine.decode(bytes).is_ok()) {
                return Err(A2aClientError::Protocol(
                    "A2A raw content must be valid base64".to_string(),
                ));
            }
        }
    }
    Ok(parts.as_slice())
}

pub(super) fn message_result(message: &Value) -> Result<Value, A2aClientError> {
    let parts = validate_parts(message.get("parts"))?;
    Ok(single_part_result(parts).unwrap_or_else(|| message.clone()))
}

pub(super) fn message_response(message: &Value) -> Result<(String, Value), A2aClientError> {
    let message_id = message
        .get("messageId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            A2aClientError::Protocol("A2A message response missing messageId".to_string())
        })?;
    if message.get("role").and_then(Value::as_str) != Some("ROLE_AGENT") {
        return Err(A2aClientError::Protocol(
            "A2A message response must have ROLE_AGENT".to_string(),
        ));
    }
    if !message
        .get("contextId")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(A2aClientError::Protocol(
            "A2A message response missing contextId".to_string(),
        ));
    }
    Ok((message_id.to_string(), message_result(message)?))
}

pub(super) fn task_result(task: &Value) -> Result<Value, A2aClientError> {
    if let Some(value) = task.get("artifacts") {
        let artifacts = value.as_array().ok_or_else(|| {
            A2aClientError::Protocol("A2A task artifacts must be an array".to_string())
        })?;
        for artifact in artifacts {
            if !artifact
                .get("artifactId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
            {
                return Err(A2aClientError::Protocol(
                    "A2A artifact missing artifactId".to_string(),
                ));
            }
            validate_parts(artifact.get("parts"))?;
        }
        if let [artifact] = artifacts.as_slice() {
            if let Some(result) = artifact
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| single_part_result(parts))
            {
                return Ok(result);
            }
        }
        if !artifacts.is_empty() {
            return Ok(task.clone());
        }
    }
    if let Some(message) =
        task.get("history")
            .and_then(Value::as_array)
            .and_then(|history| {
                history.iter().rev().find(|message| {
                    message.get("role").and_then(Value::as_str) == Some("ROLE_AGENT")
                })
            })
    {
        return message_result(message);
    }
    Ok(task.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn push_configuration_preserves_version_and_tenant() {
        let config = json!({"url": "https://example.test/push", "token": "opaque"});
        let current = push_configuration(A2aWireVersion::V1_0, Some("team-a"), config.clone());
        assert_eq!(current["taskPushNotificationConfig"]["tenant"], "team-a");
        assert_eq!(current["taskPushNotificationConfig"]["token"], "opaque");
        assert!(current["taskPushNotificationConfig"]
            .get("taskId")
            .is_none());
        assert!(current.get("pushNotificationConfig").is_none());
        let legacy = push_configuration(A2aWireVersion::V0_3, None, config.clone());
        assert_eq!(legacy["pushNotificationConfig"], config);
        assert_eq!(legacy["blocking"], false);
        assert!(legacy.get("taskPushNotificationConfig").is_none());
    }

    fn endpoint(card: &Value) -> Result<super::super::ResolvedA2aEndpoint, A2aClientError> {
        super::super::endpoint_from_card(
            "https://trusted.example/.well-known/agent-card.json".to_string(),
            false,
            "trusted.example",
            "reviewer".to_string(),
            card,
        )
    }

    #[test]
    fn selects_supported_v1_interface_and_preserves_tenant_without_crossing_authority() {
        let card = json!({"supportedInterfaces": [
            {"protocolBinding": "JSONRPC", "protocolVersion": "2.0", "url": "https://trusted.example/future"},
            {"protocolBinding": "JSONRPC", "protocolVersion": "1.0", "url": "https://trusted.example/v1", "tenant": "team-a"},
            {"protocolBinding": "JSONRPC", "protocolVersion": "0.3", "url": "https://trusted.example/v0"}
        ]});
        let selected = endpoint(&card).unwrap();
        assert_eq!(selected.protocol_version, A2aWireVersion::V1_0);
        assert_eq!(selected.tenant.as_deref(), Some("team-a"));
        assert_eq!(selected.rpc_url, "https://trusted.example/v1");
        let mut foreign = card.clone();
        foreign["supportedInterfaces"][1]["url"] = json!("https://other.example/v1");
        assert!(endpoint(&foreign).is_err());
        let mut malformed = card;
        malformed["supportedInterfaces"][1]["tenant"] = json!(7);
        assert!(endpoint(&malformed).is_err());
        assert!(endpoint(&json!({"supportedInterfaces": [{
            "protocolBinding": "JSONRPC", "protocolVersion": "2.0", "url": "https://trusted.example/v2"
        }]})).is_err());
    }

    #[test]
    fn versions_match_major_minor_and_reject_malformed_values() {
        assert_eq!(
            A2aWireVersion::from_advertised("1.0"),
            Some(A2aWireVersion::V1_0)
        );
        assert_eq!(
            A2aWireVersion::from_advertised("0.3.17"),
            Some(A2aWireVersion::V0_3)
        );
        for invalid in ["1.1", "1.0.", "1.0.+1", "1.0.1.2", "garbage"] {
            assert_eq!(A2aWireVersion::from_advertised(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn v1_send_response_requires_one_valid_variant() {
        let version = A2aWireVersion::V1_0;
        assert!(matches!(
            version.task_from_send_response(&json!({"task": {}})),
            Ok(SendResponse::Task(_))
        ));
        assert!(matches!(
            version.task_from_send_response(&json!({"message": {}})),
            Ok(SendResponse::Message(_))
        ));
        for invalid in [
            json!({}),
            json!({"task": {}, "message": {}}),
            json!({"task": null}),
        ] {
            assert!(version.task_from_send_response(&invalid).is_err());
        }
        assert_eq!(
            version.task_state("TASK_STATE_COMPLETED").unwrap(),
            "completed"
        );
        assert!(version.task_state("completed").is_err());
    }

    #[test]
    fn v1_results_keep_multiple_parts_and_decode_single_payloads() {
        let message = json!({"parts": [{"text": "{\"ok\":true}"}]});
        assert_eq!(message_result(&message).unwrap(), json!({"ok": true}));
        let multi = json!({"parts": [{"text": "first"}, {"data": 2}]});
        assert_eq!(message_result(&multi).unwrap(), multi);
        assert!(message_result(&json!({"parts": []})).is_err());
        let task =
            json!({"artifacts": [{"artifactId": "a1", "parts": [{"data": {"answer": 42}}]}]});
        assert_eq!(task_result(&task).unwrap(), json!({"answer": 42}));
        let many = json!({"artifacts": [{"artifactId": "a1", "parts": [{"text": "a"}]}, {"artifactId": "a2", "parts": [{"text": "b"}]}]});
        assert_eq!(task_result(&many).unwrap(), many);
        assert!(task_result(&json!({"artifacts": [{"parts": [{"text": "missing id"}]}]})).is_err());
        for invalid in [
            json!({"parts": [{}]}),
            json!({"parts": [{"text": "x", "data": 1}]}),
            json!({"parts": [{"raw": "not-base64!"}]}),
        ] {
            assert!(message_result(&invalid).is_err());
        }
    }

    #[test]
    fn v1_direct_message_requires_sender_and_context() {
        let valid = json!({"messageId": "m1", "contextId": "c1", "role": "ROLE_AGENT", "parts": [{"text": "ok"}]});
        assert_eq!(
            message_response(&valid).unwrap(),
            ("m1".to_string(), json!("ok"))
        );
        for field in ["messageId", "contextId", "role", "parts"] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(message_response(&invalid).is_err(), "{field}");
        }
    }
}
