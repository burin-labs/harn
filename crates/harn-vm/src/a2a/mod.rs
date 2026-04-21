use std::error::Error as _;

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

#[derive(Debug)]
enum AgentCardFetchError {
    Cancelled(String),
    Discovery(String),
    ConnectRefused(String),
}

pub async fn dispatch_trigger_event(
    raw_target: &str,
    allow_cleartext: bool,
    binding_id: &str,
    binding_key: &str,
    event: &TriggerEvent,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<(ResolvedA2aEndpoint, DispatchAck), A2aClientError> {
    let target = parse_target(raw_target)?;
    let endpoint = resolve_endpoint(&target, allow_cleartext, cancel_rx).await?;
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

    let body = send_jsonrpc(&endpoint.rpc_url, &request, &event.trace_id.0, cancel_rx).await?;
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
    allow_cleartext: bool,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<ResolvedA2aEndpoint, A2aClientError> {
    let mut last_error = None;
    for scheme in card_resolution_schemes(allow_cleartext) {
        let card_url = format!("{scheme}://{}/{A2A_AGENT_CARD_PATH}", target.authority);
        match fetch_agent_card(&card_url, cancel_rx).await {
            Ok(card) => {
                return endpoint_from_card(
                    card_url,
                    allow_cleartext,
                    &target.authority,
                    target.target_agent.clone(),
                    &card,
                );
            }
            Err(AgentCardFetchError::Cancelled(message)) => {
                return Err(A2aClientError::Cancelled(message));
            }
            Err(error) => {
                let message = agent_card_fetch_error_message(&error);
                last_error = Some(message);
                if should_try_cleartext_fallback(scheme, allow_cleartext, &error, &target.authority)
                {
                    continue;
                }
                break;
            }
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
) -> Result<Value, AgentCardFetchError> {
    let response = tokio::select! {
        response = crate::llm::shared_utility_client().get(card_url).send() => {
            match response {
                Ok(response) => Ok(response),
                Err(error) if is_connect_refused(&error) => Err(AgentCardFetchError::ConnectRefused(
                    format!("A2A HTTP request failed: {error}")
                )),
                Err(error) => Err(AgentCardFetchError::Discovery(
                    format!("A2A HTTP request failed: {error}")
                )),
            }
        }
        _ = recv_cancel(cancel_rx) => Err(AgentCardFetchError::Cancelled(
            "A2A agent-card fetch cancelled".to_string()
        )),
    }?;
    if !response.status().is_success() {
        return Err(AgentCardFetchError::Discovery(format!(
            "GET {card_url} returned HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| AgentCardFetchError::Discovery(format!("parse {card_url}: {error}")))
}

fn endpoint_from_card(
    card_url: String,
    allow_cleartext: bool,
    requested_authority: &str,
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
    ensure_cleartext_allowed(&base_url, allow_cleartext, "agent card")?;
    let card_authority = url_authority(&base_url)?;
    if !authorities_equivalent(&card_authority, requested_authority) {
        return Err(A2aClientError::Discovery(format!(
            "A2A agent card url authority mismatch: requested '{requested_authority}', card returned '{card_authority}'"
        )));
    }
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
    ensure_cleartext_allowed(&rpc_url, allow_cleartext, "jsonrpc interface")?;
    Ok(ResolvedA2aEndpoint {
        card_url,
        rpc_url: rpc_url.to_string(),
        agent_id: card.get("id").and_then(Value::as_str).map(str::to_string),
        target_agent,
    })
}

fn card_resolution_schemes(allow_cleartext: bool) -> &'static [&'static str] {
    if allow_cleartext {
        &["https", "http"]
    } else {
        &["https"]
    }
}

/// Decide whether an HTTPS discovery failure should fall through to cleartext.
///
/// External targets only fall back on `ConnectionRefused` — the common "HTTPS
/// port isn't listening" case. TLS handshake failures to an external host MUST
/// NOT silently downgrade to HTTP, because an active network attacker can
/// forge TLS errors to trigger a downgrade.
///
/// Loopback targets (`127.0.0.0/8`, `::1`, `localhost`) fall back on any
/// discovery-style error. They cover the standard local-dev case where
/// `harn serve` binds HTTP-only on `127.0.0.1:PORT`, and the SSRF threat
/// model for loopback is already bounded — any attacker who can reach the
/// local loopback already has code execution on the box.
fn should_try_cleartext_fallback(
    scheme: &str,
    allow_cleartext: bool,
    error: &AgentCardFetchError,
    authority: &str,
) -> bool {
    if !allow_cleartext || scheme != "https" {
        return false;
    }
    match error {
        AgentCardFetchError::Cancelled(_) => false,
        AgentCardFetchError::ConnectRefused(_) => true,
        AgentCardFetchError::Discovery(_) => is_loopback_authority(authority),
    }
}

fn ensure_cleartext_allowed(
    url: &Url,
    allow_cleartext: bool,
    label: &str,
) -> Result<(), A2aClientError> {
    if allow_cleartext || url.scheme() != "http" {
        return Ok(());
    }
    Err(A2aClientError::Discovery(format!(
        "cleartext A2A {label} '{url}' requires `allow_cleartext = true` on the trigger binding"
    )))
}

fn is_loopback_authority(authority: &str) -> bool {
    let (host, _) = split_authority(authority);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}

/// Return true when two authority strings refer to the same A2A endpoint.
///
/// Exact string equality is the default — an agent card that reports a
/// different host than the one the client asked for is a security-relevant
/// discrepancy (see harn#248 SSRF hardening). The one well-defined exception
/// is loopback: `localhost`, `127.0.0.1`, `::1`, and the rest of
/// `127.0.0.0/8` are all the same socket on this machine, and `harn serve`
/// hardcodes `http://localhost:PORT` in its agent card even when a caller
/// dials `127.0.0.1:PORT`. Treating both sides as loopback avoids a spurious
/// mismatch in that case without widening the external-host trust boundary.
fn authorities_equivalent(card_authority: &str, requested_authority: &str) -> bool {
    if card_authority == requested_authority {
        return true;
    }
    let (_, card_port) = split_authority(card_authority);
    let (_, requested_port) = split_authority(requested_authority);
    if card_port != requested_port {
        return false;
    }
    is_loopback_authority(card_authority) && is_loopback_authority(requested_authority)
}

/// Split an authority into `(host, port_or_empty)`. Strips IPv6 brackets so
/// `[::1]:8080` becomes `("::1", "8080")`.
fn split_authority(authority: &str) -> (&str, &str) {
    let (host_raw, port) = if authority.starts_with('[') {
        // IPv6 bracketed form: "[addr]:port" or "[addr]".
        if let Some(end) = authority.rfind(']') {
            let host = &authority[..=end];
            let rest = &authority[end + 1..];
            let port = rest.strip_prefix(':').unwrap_or("");
            (host, port)
        } else {
            (authority, "")
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port),
            None => (authority, ""),
        }
    };
    let host = host_raw.trim_start_matches('[').trim_end_matches(']');
    (host, port)
}

fn agent_card_fetch_error_message(error: &AgentCardFetchError) -> String {
    match error {
        AgentCardFetchError::Cancelled(message)
        | AgentCardFetchError::Discovery(message)
        | AgentCardFetchError::ConnectRefused(message) => message.clone(),
    }
}

fn is_connect_refused(error: &reqwest::Error) -> bool {
    if !error.is_connect() {
        return false;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            if io_error.kind() == std::io::ErrorKind::ConnectionRefused {
                return true;
            }
        }
        source = cause.source();
    }
    false
}

fn url_authority(url: &Url) -> Result<String, A2aClientError> {
    let host = url
        .host_str()
        .ok_or_else(|| A2aClientError::Discovery(format!("A2A card url '{url}' missing host")))?;
    Ok(if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.to_string()
    })
}

async fn send_jsonrpc(
    rpc_url: &str,
    request: &Value,
    trace_id: &str,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, A2aClientError> {
    let response = send_http(
        crate::llm::shared_blocking_client()
            .post(rpc_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("A2A-Version", A2A_PROTOCOL_VERSION)
            .header("A2A-Trace-Id", trace_id)
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

    #[test]
    fn discovery_prefers_https_before_http() {
        assert_eq!(card_resolution_schemes(false), ["https"]);
        assert_eq!(card_resolution_schemes(true), ["https", "http"]);
    }

    #[test]
    fn cleartext_fallback_only_after_https_connect_refused() {
        assert!(should_try_cleartext_fallback(
            "https",
            true,
            &AgentCardFetchError::ConnectRefused("connect refused".to_string()),
            "reviewer.example:443",
        ));
        assert!(!should_try_cleartext_fallback(
            "http",
            true,
            &AgentCardFetchError::ConnectRefused("connect refused".to_string()),
            "reviewer.example:443",
        ));
        assert!(!should_try_cleartext_fallback(
            "https",
            true,
            &AgentCardFetchError::Discovery("tls handshake failed".to_string()),
            "reviewer.example:443",
        ));
    }

    #[test]
    fn cleartext_fallback_requires_opt_in_even_for_loopback_authorities() {
        for authority in [
            "127.0.0.1:8080",
            "localhost:8080",
            "[::1]:8080",
            "127.1.2.3:9000",
        ] {
            assert!(
                !should_try_cleartext_fallback(
                    "https",
                    false,
                    &AgentCardFetchError::Discovery("tls handshake failed".to_string()),
                    authority,
                ),
                "cleartext fallback must stay disabled without opt-in for '{authority}'"
            );
        }
    }

    #[test]
    fn cleartext_fallback_allows_loopback_after_opt_in() {
        // Local dev: harn serve is HTTP-only, so TLS handshake fails but we
        // still need the HTTP fallback to succeed.
        for authority in [
            "127.0.0.1:8080",
            "localhost:8080",
            "[::1]:8080",
            "127.1.2.3:9000",
        ] {
            assert!(
                should_try_cleartext_fallback(
                    "https",
                    true,
                    &AgentCardFetchError::Discovery("tls handshake failed".to_string()),
                    authority,
                ),
                "expected cleartext fallback for loopback authority '{authority}'"
            );
        }
    }

    #[test]
    fn cleartext_fallback_denies_external_tls_failures() {
        // External target + TLS handshake failure must not downgrade — an
        // attacker able to forge TLS errors shouldn't force cleartext.
        for authority in [
            "reviewer.example:443",
            "8.8.8.8:443",
            "192.168.1.10:8080",
            "10.0.0.5:8443",
        ] {
            assert!(
                !should_try_cleartext_fallback(
                    "https",
                    true,
                    &AgentCardFetchError::Discovery("tls handshake failed".to_string()),
                    authority,
                ),
                "cleartext fallback must be denied for external authority '{authority}'"
            );
        }
    }

    #[test]
    fn is_loopback_authority_recognises_loopback_forms() {
        assert!(is_loopback_authority("127.0.0.1:8080"));
        assert!(is_loopback_authority("localhost:8080"));
        assert!(is_loopback_authority("LOCALHOST:9000"));
        assert!(is_loopback_authority("[::1]:8080"));
        assert!(is_loopback_authority("127.5.5.5:1234"));
        assert!(!is_loopback_authority("8.8.8.8:443"));
        assert!(!is_loopback_authority("192.168.1.10:8080"));
        assert!(!is_loopback_authority("example.com:443"));
        assert!(!is_loopback_authority("reviewer.prod"));
    }

    #[test]
    fn endpoint_from_card_rejects_card_url_authority_mismatch() {
        let error = endpoint_from_card(
            "https://trusted.example/.well-known/a2a-agent".to_string(),
            false,
            "trusted.example",
            "triage".to_string(),
            &serde_json::json!({
                "url": "https://evil.example",
                "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
            }),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "A2A agent card url authority mismatch: requested 'trusted.example', card returned 'evil.example'"
        );
    }

    #[test]
    fn endpoint_from_card_rejects_cleartext_without_opt_in() {
        let error = endpoint_from_card(
            "https://127.0.0.1:8080/.well-known/a2a-agent".to_string(),
            false,
            "127.0.0.1:8080",
            "triage".to_string(),
            &serde_json::json!({
                "url": "http://localhost:8080",
                "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
            }),
        )
        .expect_err("cleartext card should require explicit opt-in");
        assert!(error
            .to_string()
            .contains("requires `allow_cleartext = true`"));
    }

    #[test]
    fn endpoint_from_card_accepts_loopback_alias_pairs_when_cleartext_opted_in() {
        // harn serve reports `http://localhost:PORT` in its card, but clients
        // commonly dial `127.0.0.1:PORT`. Both refer to the same socket, so
        // the authority check must not spuriously reject the pair.
        let card = serde_json::json!({
            "url": "http://localhost:8080",
            "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
        });
        let endpoint = endpoint_from_card(
            "http://127.0.0.1:8080/.well-known/a2a-agent".to_string(),
            true,
            "127.0.0.1:8080",
            "triage".to_string(),
            &card,
        )
        .expect("loopback alias pair should be accepted");
        assert_eq!(endpoint.rpc_url, "http://localhost:8080/rpc");

        // IPv6 loopback `[::1]` also aliases to `127.0.0.1` / `localhost`.
        let card_v6 = serde_json::json!({
            "url": "http://[::1]:8080",
            "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
        });
        let endpoint_v6 = endpoint_from_card(
            "http://localhost:8080/.well-known/a2a-agent".to_string(),
            true,
            "localhost:8080",
            "triage".to_string(),
            &card_v6,
        )
        .expect("IPv6 loopback alias should be accepted");
        assert_eq!(endpoint_v6.rpc_url, "http://[::1]:8080/rpc");

        // Port mismatch is still rejected even on loopback.
        let card_wrong_port = serde_json::json!({
            "url": "http://localhost:9000",
            "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
        });
        let error = endpoint_from_card(
            "http://127.0.0.1:8080/.well-known/a2a-agent".to_string(),
            true,
            "127.0.0.1:8080",
            "triage".to_string(),
            &card_wrong_port,
        )
        .expect_err("mismatched ports must still be rejected even on loopback");
        assert!(error
            .to_string()
            .contains("A2A agent card url authority mismatch"));
    }

    #[test]
    fn authorities_equivalent_rejects_non_loopback_host_mismatch() {
        assert!(!authorities_equivalent(
            "internal.corp.example:443",
            "trusted.example:443",
        ));
        assert!(!authorities_equivalent("10.0.0.5:8080", "127.0.0.1:8080",));
        assert!(authorities_equivalent(
            "trusted.example:443",
            "trusted.example:443",
        ));
    }
}
