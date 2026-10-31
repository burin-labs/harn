//! Reconstructible client inputs in the same tape as the model execution.

use super::{
    client_capabilities::ClientCapabilities, AcpOutput, AcpSessionNewParams, AcpSessionPromptParams,
};
use harn_vm::testbench::tape::{EventTape, TapeRecordKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum InputEvent {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Completed {
        id: Value,
        succeeded: bool,
        session_id: Option<String>,
        stop_reason: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

/// One admitted, completed request. Replay replaces only workspace and session identity.
#[derive(Clone, Debug)]
pub struct ReplayRequest {
    method: String,
    params: Value,
    created_session: Option<String>,
}

impl ReplayRequest {
    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn created_session(&self) -> Option<&str> {
        self.created_session.as_deref()
    }

    pub fn params(
        &self,
        workspace: &str,
        sessions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Value, String> {
        let mut params = self.params.clone();
        if self.method == "session/new" {
            params["cwd"] = json!(workspace);
        } else if params.get("sessionId").is_some() {
            let original = params["sessionId"]
                .as_str()
                .ok_or("recorded session identity is invalid")?;
            params["sessionId"] = json!(sessions
                .get(original)
                .ok_or("replay session has not been created")?);
        }
        Ok(params)
    }
}

/// A complete, serial input history for sessions sharing one workspace. Missing or unsupported
/// inputs refuse before the caller starts an engine or restores files.
pub fn replay_requests(tape: &EventTape) -> Result<Vec<ReplayRequest>, String> {
    let mut requests = Vec::new();
    let mut pending: Option<(Value, ReplayRequest)> = None;
    let mut initialized = false;
    let mut sessions = std::collections::BTreeSet::new();
    let mut workspace: Option<String> = None;
    let mut prompts = 0;
    let mut request_ids = std::collections::BTreeSet::new();
    for record in &tape.records {
        let TapeRecordKind::SessionControl { event } = &record.kind else {
            continue;
        };
        let event: InputEvent = serde_json::from_slice(&tape.resolve_payload(event)?)
            .map_err(|error| format!("invalid recorded session input: {error}"))?;
        match event {
            InputEvent::Unavailable { reason } => return Err(reason),
            InputEvent::Request { id, method, params } => {
                if !request_ids.insert(id.to_string()) {
                    return Err("recording reuses a client request identity".into());
                }
                if pending.is_some() {
                    return Err(
                        "overlapping client requests need an explicit replay schedule".into(),
                    );
                }
                let params = normalize(&method, &params)?;
                match method.as_str() {
                    "initialize" if !initialized => initialized = true,
                    "session/new" if initialized => {
                        let cwd = params["cwd"]
                            .as_str()
                            .ok_or("recorded session has no workspace")?;
                        if workspace.as_deref().is_some_and(|previous| previous != cwd) {
                            return Err("recording spans multiple workspaces".into());
                        }
                        workspace = Some(cwd.to_owned());
                    }
                    "session/prompt"
                    | "session/set_mode"
                    | "session/set_config_option"
                    | "session/set_budget"
                    | "session/remind"
                    | "session/close"
                        if initialized =>
                    {
                        let session = params["sessionId"]
                            .as_str()
                            .ok_or("recorded request has no session identity")?;
                        if !sessions.contains(session) {
                            return Err(
                                "recorded request targets a session that is not open".into()
                            );
                        }
                        if method == "session/prompt" {
                            prompts += 1;
                        }
                    }
                    _ => {
                        return Err(
                            "recorded session initialization is incomplete or repeated".into()
                        )
                    }
                }
                pending = Some((
                    id,
                    ReplayRequest {
                        method,
                        params,
                        created_session: None,
                    },
                ));
            }
            InputEvent::Completed {
                id,
                succeeded,
                session_id,
                stop_reason,
            } => {
                let Some((expected, mut request)) = pending.take() else {
                    continue;
                };
                if expected != id {
                    pending = Some((expected, request));
                    continue;
                }
                if !succeeded {
                    return Err("recording includes a failed client request".into());
                }
                if request.method == "session/prompt" && stop_reason.as_deref() != Some("end_turn")
                {
                    return Err(
                        "recorded prompt did not complete normally; record a completed baseline"
                            .into(),
                    );
                }
                if request.method == "session/new" {
                    let session = session_id
                        .filter(|value| !value.is_empty())
                        .ok_or("recorded session creation has no identity")?;
                    if !sessions.insert(session.clone()) {
                        return Err("recording reuses a session identity".into());
                    }
                    request.created_session = Some(session);
                } else if request.method == "session/close" {
                    sessions.remove(
                        request.params["sessionId"]
                            .as_str()
                            .expect("validated session"),
                    );
                }
                requests.push(request);
            }
        }
    }
    if pending.is_some() || !initialized || workspace.is_none() || prompts == 0 {
        return Err("recording has no complete session input history; record a new run".into());
    }
    Ok(requests)
}

pub(super) struct InputRecorder {
    recorder: Arc<harn_vm::testbench::tape::TapeRecorder>,
    refused: AtomicBool,
}

impl InputRecorder {
    pub(super) fn capture() -> Option<Arc<Self>> {
        harn_vm::run_tape::process_recorder().map(|recorder| {
            Arc::new(Self {
                recorder,
                refused: AtomicBool::new(false),
            })
        })
    }

    fn record(&self, event: InputEvent) {
        if matches!(event, InputEvent::Unavailable { .. }) {
            self.refused.store(true, Ordering::Release);
        }
        let bytes = serde_json::to_vec(&event).expect("session input serializes");
        let event = self.recorder.payload_from_bytes(bytes);
        self.recorder
            .record(TapeRecordKind::SessionControl { event });
    }

    pub(super) fn capture_request(&self, frame: &Value) {
        if self.refused.load(Ordering::Acquire) {
            return;
        }
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return;
        };
        if matches!(
            method,
            super::HARN_PROVIDER_CATALOG_METHOD
                | harn_vm::session_timeline::SESSION_TIMELINE_QUERY_METHOD
                | harn_vm::session_recap::SESSION_RECAP_QUERY_METHOD
                | harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD
                | harn_vm::session_timeline::SESSION_TIMELINE_SUBSCRIBE_METHOD
                | harn_vm::session_timeline::SESSION_TIMELINE_UNSUBSCRIBE_METHOD
        ) {
            return;
        }
        let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = normalize(method, &params).and_then(|params| {
            let id = frame
                .get("id")
                .filter(|id| id.is_string() || id.is_number())
                .ok_or("recorded control is a notification without a completion".to_owned())?;
            let redacted = harn_vm::redact::current_policy().redact_json(&params);
            if redacted != params {
                return Err(
                    "session inputs require redaction and cannot be reproduced from this tape"
                        .into(),
                );
            }
            Ok(InputEvent::Request {
                id: id.clone(),
                method: method.into(),
                params,
            })
        });
        self.record(result.unwrap_or_else(|reason| InputEvent::Unavailable { reason }));
    }

    pub(super) fn observe_output(self: &Arc<Self>, output: AcpOutput) -> AcpOutput {
        let recorder = self.clone();
        AcpOutput::callback(move |line| {
            if let Ok(frame) = serde_json::from_str::<Value>(line) {
                if frame.get("method").is_none() {
                    if let Some(id) = frame.get("id") {
                        recorder.record(InputEvent::Completed {
                            id: id.clone(),
                            succeeded: frame.get("error").is_none()
                                && frame.get("result").is_some(),
                            session_id: frame["result"]["sessionId"].as_str().map(str::to_owned),
                            stop_reason: frame["result"]["stopReason"].as_str().map(str::to_owned),
                        });
                    }
                }
            }
            output.write_line(line);
        })
    }
}

fn normalize(method: &str, params: &Value) -> Result<Value, String> {
    let fields: &[&str] = match method {
        "initialize" => &["protocolVersion", "clientInfo", "clientCapabilities"],
        "session/new" => &[
            "cwd",
            "environmentPolicy",
            "workspaceCheckpoint",
            "mcpServers",
        ],
        "session/prompt" => &["sessionId", "prompt", "messageId"],
        "session/close" => &["sessionId"],
        "session/set_mode" => &["sessionId", "modeId"],
        "session/set_config_option" => &["sessionId", "configId", "value"],
        "session/set_budget" => &["sessionId", "llm_cost_usd", "llm_tokens"],
        "session/remind" => &[
            "sessionId",
            "mode",
            "id",
            "body",
            "tags",
            "dedupe_key",
            "ttl_turns",
            "preserve_on_compact",
            "propagate",
            "role_hint",
        ],
        _ => return Err("recording includes a client operation without a replay schedule".into()),
    };
    let object = params
        .as_object()
        .ok_or("client parameters must be an object")?;
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(format!(
            "client operation {method} includes unsupported inputs"
        ));
    }
    match method {
        "initialize" => Ok(
            json!({"protocolVersion": 1, "clientCapabilities": ClientCapabilities::from_initialize(params)}),
        ),
        "session/new" => {
            if object
                .get("mcpServers")
                .is_some_and(|servers| servers.as_array().is_none_or(|servers| !servers.is_empty()))
            {
                return Err("replay cannot reconstruct external tool-server configuration".into());
            }
            let typed: AcpSessionNewParams = serde_json::from_value(params.clone())
                .map_err(|_| "invalid recorded session configuration")?;
            if typed.environment_policy.is_none() || typed.cwd.is_none() {
                return Err("recorded session needs a workspace and environment policy".into());
            }
            if object
                .get("workspaceCheckpoint")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("invalid recorded checkpoint setting".into());
            }
            serde_json::to_value(typed).map_err(|error| error.to_string())
        }
        "session/prompt" => {
            let typed: AcpSessionPromptParams =
                serde_json::from_value(params.clone()).map_err(|_| "invalid recorded prompt")?;
            if typed
                .prompt
                .iter()
                .any(|block| !matches!(block, super::AcpContentBlock::Text { .. }))
            {
                return Err("replay inputs currently require self-contained text prompts".into());
            }
            serde_json::to_value(typed).map_err(|error| error.to_string())
        }
        _ => Ok(params.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_tool_server_list_does_not_admit_external_configuration() {
        for servers in [
            json!([{"name":"external", "command":"server"}]),
            json!({}),
            Value::Null,
        ] {
            assert!(normalize("session/new", &json!({
                "cwd":"/workspace", "environmentPolicy":{"kind":"isolated"}, "mcpServers":servers
            })).unwrap_err().contains("external tool-server"));
        }
    }
}
