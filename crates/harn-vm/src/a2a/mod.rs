use reqwest::Url;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::triggers::TriggerEvent;

const A2A_AGENT_CARD_PATH: &str = ".well-known/a2a-agent";
const A2A_PROTOCOL_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedA2aEndpoint {
    pub card_url: String,
    pub rpc_url: String,
    pub agent_id: Option<String>,
    pub target_agent: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DispatchAck {
    InlineResult {
        task_id: String,
        result: Value,
    },
    PendingTask {
        task_id: String,
        state: String,
        handle: Value,
    },
}

#[derive(Debug)]
pub enum A2aClientError {
    InvalidTarget(String),
    Discovery(String),
    Protocol(String),
    Cancelled(String),
}

impl std::fmt::Display for A2aClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget(message)
            | Self::Discovery(message)
            | Self::Protocol(message)
            | Self::Cancelled(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for A2aClientError {}

pub async fn dispatch_trigger_event(
    raw_target: &str,
    binding_id: &str,
    binding_key: &str,
    event: &TriggerEvent,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<(ResolvedA2aEndpoint, DispatchAck), A2aClientError> {
    let target = parse_target(raw_target)?;
    let endpoint = resolve_endpoint(&target, cancel_rx).await?;
    let message_id = format!("{}.{}", event.trace_id.0, event.id.0);
    let envelope = serde_json::json!({
        "kind": "harn.trigger.dispatch",
        "message_id": message_id,
        "trace_id": event.trace_id.0,
        "event_id": event.id.0,
        "trigger_id": binding_id,
        "binding_key": binding_key,
        "target_agent": endpoint.target_agent,
        "event": event,
    });
    let text = serde_json::to_string(&envelope)
        .map_err(|error| A2aClientError::Protocol(format!("serialize A2A envelope: {error}")))?;
    let request = crate::jsonrpc::request(
        message_id.clone(),
        "a2a.SendMessage",
        serde_json::json!({
            "contextId": event.trace_id.0,
            "message": {
                "messageId": message_id,
                "role": "user",
                "parts": [{
                    "type": "text",
                    "text": text,
                }],
                "metadata": {
                    "kind": "harn.trigger.dispatch",
                    "trace_id": event.trace_id.0,
                    "event_id": event.id.0,
                    "trigger_id": binding_id,
                    "binding_key": binding_key,
                    "target_agent": endpoint.target_agent,
                },
            },
        }),
    );

    let body = send_jsonrpc(&endpoint.rpc_url, &request, cancel_rx).await?;
    let result = body.get("result").cloned().ok_or_else(|| {
        if let Some(error) = body.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown A2A error");
            A2aClientError::Protocol(format!("A2A task dispatch failed: {message}"))
        } else {
            A2aClientError::Protocol("A2A task dispatch response missing result".to_string())
        }
    })?;

    let task_id = result
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| A2aClientError::Protocol("A2A task response missing result.id".to_string()))?
        .to_string();
    let state = task_state(&result)?.to_string();

    if state == "completed" {
        let inline = extract_inline_result(&result);
        return Ok((
            endpoint,
            DispatchAck::InlineResult {
                task_id,
                result: inline,
            },
        ));
    }

    Ok((
        endpoint.clone(),
        DispatchAck::PendingTask {
            task_id: task_id.clone(),
            state: state.clone(),
            handle: serde_json::json!({
                "kind": "a2a_task_handle",
                "task_id": task_id,
                "state": state,
                "target_agent": endpoint.target_agent,
                "rpc_url": endpoint.rpc_url,
                "card_url": endpoint.card_url,
                "agent_id": endpoint.agent_id,
            }),
        },
    ))
}

pub fn target_agent_label(raw_target: &str) -> String {
    parse_target(raw_target)
        .map(|target| target.target_agent_label())
        .unwrap_or_else(|_| raw_target.to_string())
}

#[derive(Clone, Debug)]
struct ParsedTarget {
    authority: String,
    target_agent: String,
}

impl ParsedTarget {
    fn target_agent_label(&self) -> String {
        if self.target_agent.is_empty() {
            self.authority.clone()
        } else {
            self.target_agent.clone()
        }
    }
}

fn parse_target(raw_target: &str) -> Result<ParsedTarget, A2aClientError> {
    let parsed = Url::parse(&format!("http://{raw_target}")).map_err(|error| {
        A2aClientError::InvalidTarget(format!(
            "invalid a2a dispatch target '{raw_target}': {error}"
        ))
    })?;
    let host = parsed.host_str().ok_or_else(|| {
        A2aClientError::InvalidTarget(format!(
            "invalid a2a dispatch target '{raw_target}': missing host"
        ))
    })?;
    let authority = if let Some(port) = parsed.port() {
        format!("{host}:{port}")
    } else {
        host.to_string()
    };
    Ok(ParsedTarget {
        authority,
        target_agent: parsed.path().trim_start_matches('/').to_string(),
    })
}

async fn resolve_endpoint(
    target: &ParsedTarget,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<ResolvedA2aEndpoint, A2aClientError> {
    let mut last_error = None;
    for scheme in ["http", "https"] {
        let card_url = format!("{scheme}://{}/{A2A_AGENT_CARD_PATH}", target.authority);
        match fetch_agent_card(&card_url, cancel_rx).await {
            Ok(card) => return endpoint_from_card(card_url, target.target_agent.clone(), &card),
            Err(A2aClientError::Cancelled(message)) => {
                return Err(A2aClientError::Cancelled(message));
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(A2aClientError::Discovery(format!(
        "could not resolve A2A agent card for '{}': {}",
        target.authority,
        last_error.unwrap_or_else(|| "unknown discovery error".to_string())
    )))
}

async fn fetch_agent_card(
    card_url: &str,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, A2aClientError> {
    let response = send_http(
        crate::llm::shared_utility_client().get(card_url),
        cancel_rx,
        "A2A agent-card fetch cancelled",
    )
    .await?;
    if !response.status().is_success() {
        return Err(A2aClientError::Discovery(format!(
            "GET {card_url} returned HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| A2aClientError::Discovery(format!("parse {card_url}: {error}")))
}

fn endpoint_from_card(
    card_url: String,
    target_agent: String,
    card: &Value,
) -> Result<ResolvedA2aEndpoint, A2aClientError> {
    let base_url = card
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| A2aClientError::Discovery("A2A agent card missing url".to_string()))?;
    let base_url = Url::parse(base_url).map_err(|error| {
        A2aClientError::Discovery(format!("invalid A2A card url '{base_url}': {error}"))
    })?;
    let interfaces = card
        .get("interfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            A2aClientError::Discovery("A2A agent card missing interfaces".to_string())
        })?;
    let jsonrpc_interfaces: Vec<&Value> = interfaces
        .iter()
        .filter(|entry| entry.get("protocol").and_then(Value::as_str) == Some("jsonrpc"))
        .collect();
    if jsonrpc_interfaces.len() != 1 {
        return Err(A2aClientError::Discovery(format!(
            "A2A agent card must expose exactly one jsonrpc interface, found {}",
            jsonrpc_interfaces.len()
        )));
    }
    let interface_url = jsonrpc_interfaces[0]
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            A2aClientError::Discovery("A2A jsonrpc interface missing url".to_string())
        })?;
    let rpc_url = base_url.join(interface_url).map_err(|error| {
        A2aClientError::Discovery(format!(
            "invalid A2A interface url '{interface_url}': {error}"
        ))
    })?;
    Ok(ResolvedA2aEndpoint {
        card_url,
        rpc_url: rpc_url.to_string(),
        agent_id: card.get("id").and_then(Value::as_str).map(str::to_string),
        target_agent,
    })
}

async fn send_jsonrpc(
    rpc_url: &str,
    request: &Value,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, A2aClientError> {
    let response = send_http(
        crate::llm::shared_blocking_client()
            .post(rpc_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("A2A-Version", A2A_PROTOCOL_VERSION)
            .json(request),
        cancel_rx,
        "A2A task dispatch cancelled",
    )
    .await?;
    if !response.status().is_success() {
        return Err(A2aClientError::Protocol(format!(
            "A2A task dispatch returned HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| A2aClientError::Protocol(format!("parse A2A dispatch response: {error}")))
}

async fn send_http(
    request: reqwest::RequestBuilder,
    cancel_rx: &mut broadcast::Receiver<()>,
    cancelled_message: &'static str,
) -> Result<reqwest::Response, A2aClientError> {
    tokio::select! {
        response = request.send() => response
            .map_err(|error| A2aClientError::Protocol(format!("A2A HTTP request failed: {error}"))),
        _ = recv_cancel(cancel_rx) => Err(A2aClientError::Cancelled(cancelled_message.to_string())),
    }
}

fn task_state(task: &Value) -> Result<&str, A2aClientError> {
    task.pointer("/status/state")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            A2aClientError::Protocol("A2A task response missing result.status.state".to_string())
        })
}

fn extract_inline_result(task: &Value) -> Value {
    let text = task
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history.iter().rev().find_map(|message| {
                let role = message.get("role").and_then(Value::as_str)?;
                if role != "agent" {
                    return None;
                }
                message
                    .get("parts")
                    .and_then(Value::as_array)
                    .and_then(|parts| {
                        parts.iter().find_map(|part| {
                            if part.get("type").and_then(Value::as_str) == Some("text") {
                                part.get("text").and_then(Value::as_str).map(str::trim_end)
                            } else {
                                None
                            }
                        })
                    })
            })
        });
    match text {
        Some(text) if !text.is_empty() => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
        }
        _ => task.clone(),
    }
}

async fn recv_cancel(cancel_rx: &mut broadcast::Receiver<()>) {
    let _ = cancel_rx.recv().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_agent_label_prefers_path() {
        assert_eq!(target_agent_label("reviewer.prod/triage"), "triage");
        assert_eq!(target_agent_label("reviewer.prod"), "reviewer.prod");
    }

    #[test]
    fn extract_inline_result_parses_json_text() {
        let task = serde_json::json!({
            "history": [
                {"role": "user", "parts": [{"type": "text", "text": "ignored"}]},
                {"role": "agent", "parts": [{"type": "text", "text": "{\"trace_id\":\"trace_123\"}\n"}]},
            ]
        });
        assert_eq!(
            extract_inline_result(&task),
            serde_json::json!({"trace_id": "trace_123"})
        );
    }
}
