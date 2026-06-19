//! MCP `sampling/createMessage` plumbing — server-to-client LLM sampling.
//!
//! When Harn is acting as an MCP **client**, an inbound
//! `sampling/createMessage` request from a peer server is parsed,
//! gated through the embedder via the `HostCallBridge`
//! (`capability="mcp"`, `operation="sample"`), and — if approved —
//! dispatched to Harn's own `llm_call` execution path. The assistant's
//! reply is returned to the server in the spec response shape:
//! `{role: "assistant", content: {type: "text", text}, model, stopReason}`.
//!
//! When no host bridge is wired up, inbound sampling requests are
//! declined with a structured JSON-RPC error so the originating server
//! can fall back to a sensible default. This is the safe default
//! because sampling spends the user's API budget — a connected MCP
//! server should never get to drive an LLM call without an explicit
//! approval surface.
//!
//! See the spec at
//! <https://modelcontextprotocol.io/specification/2025-11-25/client/sampling>.

use crate::value::VmDictExt;

use serde_json::{json, Value as JsonValue};

use crate::schema::json_to_vm_value;
use crate::stdlib::host::{dispatch_host_call_bridge, dispatch_mock_host_call};
use crate::value::{VmError, VmValue};

/// JSON-RPC method name for sampling requests.
pub const SAMPLING_METHOD: &str = "sampling/createMessage";

/// Parsed sampling request — the script-shape of a `sampling/createMessage`
/// payload after we translate it out of the raw JSON-RPC envelope.
#[derive(Debug, Clone)]
struct SamplingRequest {
    /// Conversation history. Each message is a `{role, content}` shape
    /// where `content` may be a single block or an array of blocks.
    messages: Vec<JsonValue>,
    /// Optional system prompt prepended to the conversation.
    system: Option<String>,
    /// Required token budget. Mapped to `llm_call`'s `max_tokens`.
    max_tokens: i64,
    /// Sampling temperature in `[0, 1]`. Optional.
    temperature: Option<f64>,
    /// Stop sequences. Optional.
    stop_sequences: Option<Vec<String>>,
    /// Model preferences hint chain — see [`pick_model_hint`].
    model_preferences: Option<JsonValue>,
    /// Tool definitions (2025-11-25 sampling additions). Forwarded to
    /// `llm_call`'s `tools` option when present.
    tools: Option<JsonValue>,
    /// `tool_choice` directive — forwarded as-is to `llm_call`.
    tool_choice: Option<JsonValue>,
    /// Spec-aligned thinking config — forwarded to `llm_call`'s
    /// `thinking` option when present.
    thinking: Option<JsonValue>,
    /// Pass-through metadata from the originating server. Surfaced to
    /// the host bridge so policy decisions can consider it.
    metadata: Option<JsonValue>,
    /// Soft-deprecated `includeContext` hint. Forwarded to the host
    /// bridge for visibility but otherwise ignored — Harn's
    /// orchestrator never auto-attaches host context.
    include_context: Option<String>,
}

/// Outcome of asking the embedder whether to honor the sampling request.
#[derive(Debug, Clone)]
enum ApprovalDecision {
    /// Approved — proceed with the listed `llm_call` option overrides
    /// merged on top of the request-derived defaults.
    Accept(crate::value::DictMap),
    /// Declined — propagate as a JSON-RPC error to the server with the
    /// reason string the host supplied (or a default).
    Decline(String),
}

/// Dispatch an inbound server-to-client `sampling/createMessage`
/// request (received while Harn is acting as an MCP client) and return
/// the JSON-RPC response we should send back to the server.
///
/// The implementation order matches existing HITL primitives:
///   1. If a `host_mock("mcp", "sample", ...)` matches, use that.
///   2. Otherwise, dispatch through the installed `HostCallBridge`.
///   3. If no host can take the call, decline with a structured error
///      so the server can fall back to a sensible default.
///
/// On approval, the request is translated into Harn's `llm_call`
/// boundary (`extract_llm_options` + `execute_llm_call`) so providers
/// pick up the same routing, capability gating, mock interception,
/// and budget plumbing as a script-side `llm_call`.
pub async fn dispatch_inbound_sampling(server_name: &str, request: &JsonValue) -> JsonValue {
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    // Surface the inbound sampling request to live observers (the ACP
    // adapter renders it as an `_harn/agentEvent` of kind
    // `mcp_notification`) so a thin client can show the affordance. This
    // is observability only — the request still resolves through the host
    // approval / decline path below with unchanged response semantics.
    if let Some(session_id) = crate::llm::current_agent_session_id() {
        crate::agent_events::emit_event(&crate::agent_events::AgentEvent::McpNotification {
            session_id,
            server: server_name.to_string(),
            method: SAMPLING_METHOD.to_string(),
            direction: "request".to_string(),
            params: params.clone(),
        });
    }

    let parsed = match parse_sampling_request(&params) {
        Ok(p) => p,
        Err(detail) => return crate::jsonrpc::error_response(id, -32602, &detail),
    };

    let approval = ask_host_approval(server_name, &params).await;
    let overrides = match approval {
        ApprovalDecision::Accept(map) => map,
        ApprovalDecision::Decline(reason) => {
            return crate::jsonrpc::error_response_with_data(
                id,
                -32603,
                &format!("Sampling declined: {reason}"),
                json!({
                    "type": "mcp.samplingDeclined",
                    "method": SAMPLING_METHOD,
                    "reason": reason,
                }),
            );
        }
    };

    match run_llm_call(&parsed, overrides).await {
        Ok(outcome) => crate::jsonrpc::response(id, build_spec_response(outcome, &parsed)),
        Err(detail) => crate::jsonrpc::error_response_with_data(
            id,
            -32000,
            &format!("Sampling failed: {detail}"),
            json!({
                "type": "mcp.samplingFailed",
                "method": SAMPLING_METHOD,
                "reason": detail,
            }),
        ),
    }
}

/// Parse a `sampling/createMessage` params payload. Returns the
/// `Err(detail)` shape (a flat string) so the caller can wrap it in
/// the appropriate `-32602` JSON-RPC error.
fn parse_sampling_request(params: &JsonValue) -> Result<SamplingRequest, String> {
    let object = params
        .as_object()
        .ok_or_else(|| "sampling params must be a JSON object".to_string())?;

    let messages = match object.get("messages") {
        Some(JsonValue::Array(items)) => items.clone(),
        Some(_) => return Err("sampling params 'messages' must be an array".into()),
        None => return Err("sampling params 'messages' is required".into()),
    };
    if messages.is_empty() {
        return Err("sampling params 'messages' must not be empty".into());
    }
    for (idx, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("sampling messages[{idx}].role is required"))?;
        if !matches!(role, "user" | "assistant" | "system") {
            return Err(format!(
                "sampling messages[{idx}].role must be 'user'/'assistant'/'system' (got {role:?})"
            ));
        }
        if message.get("content").is_none() {
            return Err(format!("sampling messages[{idx}].content is required"));
        }
    }

    let system = object
        .get("systemPrompt")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let max_tokens = object
        .get("maxTokens")
        .and_then(|value| value.as_i64())
        .ok_or_else(|| {
            "sampling params 'maxTokens' is required and must be an integer".to_string()
        })?;
    if max_tokens <= 0 {
        return Err(format!(
            "sampling params 'maxTokens' must be positive (got {max_tokens})"
        ));
    }

    let temperature =
        match object.get("temperature") {
            Some(JsonValue::Number(n)) => Some(n.as_f64().ok_or_else(|| {
                "sampling params 'temperature' must be a finite number".to_string()
            })?),
            Some(JsonValue::Null) | None => None,
            Some(_) => return Err("sampling params 'temperature' must be a number".into()),
        };

    let stop_sequences = match object.get("stopSequences") {
        Some(JsonValue::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    format!("sampling params 'stopSequences[{idx}]' must be a string")
                })?;
                out.push(s.to_string());
            }
            Some(out)
        }
        Some(JsonValue::Null) | None => None,
        Some(_) => return Err("sampling params 'stopSequences' must be an array".into()),
    };

    let include_context = object
        .get("includeContext")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    // MCP spec uses camelCase for the 2025-11-25 sampling additions
    // (`toolChoice`); accept the snake_case form as a tolerance for
    // servers that follow Harn's own option naming.
    let tool_choice = object
        .get("toolChoice")
        .or_else(|| object.get("tool_choice"))
        .cloned();

    Ok(SamplingRequest {
        messages,
        system,
        max_tokens,
        temperature,
        stop_sequences,
        model_preferences: object.get("modelPreferences").cloned(),
        tools: object.get("tools").cloned(),
        tool_choice,
        thinking: object.get("thinking").cloned(),
        metadata: object.get("metadata").cloned(),
        include_context,
    })
}

/// Ask the host bridge whether to honor the sampling request and what
/// `llm_call` overrides to apply. The bridge sees the full original
/// `params` payload so it can run its own approval UX or rate-limit
/// against the originating server name.
///
/// Bridge response coercion (mirrors `mcp_elicit`):
/// - `null` / no bridge → decline ("no host bridge")
/// - `{action: "decline", message?}` → decline with the message
/// - `{action: "accept", options?}` → accept with `options` as overrides
/// - `{action: "accept"}` → accept with no overrides
/// - bare dict → accept and treat the whole dict as overrides (so a
///   minimal embedder that just wants to force `provider: "mock"` can
///   return `{provider: "mock"}` without ceremony)
async fn ask_host_approval(server_name: &str, params: &JsonValue) -> ApprovalDecision {
    let mut bridge_params: crate::value::DictMap = crate::value::DictMap::new();
    bridge_params.put_str("server", server_name);
    bridge_params.insert(crate::value::intern_key("params"), json_to_vm_value(params));

    let result = dispatch_mock_host_call("mcp", "sample", &bridge_params)
        .or_else(|| dispatch_host_call_bridge("mcp", "sample", &bridge_params));

    let raw = match result {
        Some(Ok(value)) => value,
        Some(Err(error)) => {
            return ApprovalDecision::Decline(host_error_to_string(error));
        }
        None => {
            return ApprovalDecision::Decline(
                "no host bridge installed for ('mcp', 'sample')".into(),
            );
        }
    };

    coerce_bridge_response(raw)
}

fn coerce_bridge_response(value: VmValue) -> ApprovalDecision {
    match value {
        VmValue::Nil => ApprovalDecision::Decline("host bridge returned nil".into()),
        VmValue::Bool(false) => ApprovalDecision::Decline("host bridge declined".into()),
        VmValue::Bool(true) => ApprovalDecision::Accept(crate::value::DictMap::new()),
        VmValue::Dict(dict) => {
            let map = dict.as_ref().clone();
            match map.get("action").and_then(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            }) {
                Some(action) if action == "decline" || action == "cancel" => {
                    let reason = map
                        .get("message")
                        .or_else(|| map.get("reason"))
                        .map(VmValue::display)
                        .unwrap_or_else(|| "host bridge declined".to_string());
                    ApprovalDecision::Decline(reason)
                }
                Some(action) if action == "accept" => {
                    let overrides = map
                        .get("options")
                        .and_then(|v| match v {
                            VmValue::Dict(d) => Some(d.as_ref().clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    ApprovalDecision::Accept(overrides)
                }
                Some(other) => ApprovalDecision::Decline(format!(
                    "host bridge returned unknown action {other:?}"
                )),
                None => {
                    // No `action` field — treat the whole dict as a flat
                    // overrides map. Keeps the trivial embedder happy.
                    ApprovalDecision::Accept(map)
                }
            }
        }
        other => ApprovalDecision::Decline(format!(
            "host bridge returned unsupported value: {}",
            other.display()
        )),
    }
}

fn host_error_to_string(error: VmError) -> String {
    match error {
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        VmError::Thrown(other) => other.display(),
        VmError::Runtime(s) | VmError::TypeError(s) => s,
        other => format!("{other:?}"),
    }
}

/// Outcome of a successful `llm_call` for a sampling request — what
/// the response builder needs to fill in `text` and the actual model
/// the provider settled on.
#[derive(Debug, Clone)]
struct LlmOutcome {
    text: String,
    model: String,
}

/// Run Harn's `llm_call` against a parsed sampling request, returning
/// the assistant text plus the actual model name. Errors are flattened
/// to a short string so the dispatcher can wrap them in JSON-RPC.
async fn run_llm_call(
    parsed: &SamplingRequest,
    overrides: crate::value::DictMap,
) -> Result<LlmOutcome, String> {
    let (vm_args, options_dict) = build_llm_call_args(parsed, overrides);

    let opts = crate::llm::extract_llm_options(&vm_args).map_err(host_error_to_string)?;
    let result = crate::llm::execute_llm_call(None, opts, Some(options_dict), None)
        .await
        .map_err(host_error_to_string)?;

    extract_assistant_outcome(&result)
}

fn extract_assistant_outcome(result: &VmValue) -> Result<LlmOutcome, String> {
    match result {
        VmValue::String(s) => Ok(LlmOutcome {
            text: s.to_string(),
            model: String::new(),
        }),
        VmValue::Dict(d) => {
            let text = match d.get("text") {
                Some(VmValue::String(s)) => s.to_string(),
                Some(other) => other.display(),
                None => return Err("llm_call result missing 'text' field".into()),
            };
            let model = d.get("model").map(VmValue::display).unwrap_or_default();
            Ok(LlmOutcome { text, model })
        }
        other => Ok(LlmOutcome {
            text: other.display(),
            model: String::new(),
        }),
    }
}

/// Build the `[prompt, system, options]` arg list that
/// `extract_llm_options` consumes, plus a clone of the options map for
/// `execute_llm_call`'s second parameter (it consults the same dict
/// for retry/tool-format settings that aren't on `LlmCallOptions`).
fn build_llm_call_args(
    parsed: &SamplingRequest,
    overrides: crate::value::DictMap,
) -> (Vec<VmValue>, crate::value::DictMap) {
    let mut options: crate::value::DictMap = crate::value::DictMap::new();

    // Translate the sampling messages into the VM `messages` shape
    // `extract_llm_options` accepts (a list of `{role, content}` dicts).
    let messages_vm: Vec<VmValue> = parsed.messages.iter().map(json_to_vm_value).collect();
    options.insert(
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(messages_vm)),
    );

    options.insert(
        crate::value::intern_key("max_tokens"),
        VmValue::Int(parsed.max_tokens),
    );

    if let Some(temperature) = parsed.temperature {
        options.insert(
            crate::value::intern_key("temperature"),
            VmValue::Float(temperature),
        );
    }

    if let Some(stop) = parsed.stop_sequences.as_ref() {
        let stop_vm: Vec<VmValue> = stop
            .iter()
            .map(|s| VmValue::String(arcstr::ArcStr::from(s.as_str())))
            .collect();
        options.insert(
            crate::value::intern_key("stop"),
            VmValue::List(std::sync::Arc::new(stop_vm)),
        );
    }

    if let Some(hint) = pick_model_hint(parsed.model_preferences.as_ref()) {
        options.put_str("model", hint.as_str());
    }

    if let Some(tools) = parsed.tools.as_ref() {
        options.insert(crate::value::intern_key("tools"), json_to_vm_value(tools));
    }
    if let Some(tool_choice) = parsed.tool_choice.as_ref() {
        options.insert(
            crate::value::intern_key("tool_choice"),
            json_to_vm_value(tool_choice),
        );
    }
    if let Some(thinking) = parsed.thinking.as_ref() {
        options.insert(
            crate::value::intern_key("thinking"),
            json_to_vm_value(thinking),
        );
    }

    // Pass-through fields kept on the options map so transcripts and
    // mocks see the original server's intent.
    if let Some(metadata) = parsed.metadata.as_ref() {
        options.insert(
            crate::value::intern_key("metadata"),
            json_to_vm_value(metadata),
        );
    }
    if let Some(include_context) = parsed.include_context.as_ref() {
        options.put_str("include_context", include_context.as_str());
    }

    // Host-bridge overrides win over request-derived defaults so an
    // embedder can force `provider: "mock"` or rewrite the model.
    for (key, value) in overrides {
        options.insert(key, value);
    }

    let system_value = parsed
        .system
        .as_ref()
        .map(|s| VmValue::String(arcstr::ArcStr::from(s.as_str())))
        .unwrap_or(VmValue::Nil);

    let args = vec![
        VmValue::String(arcstr::ArcStr::from("")),
        system_value,
        VmValue::dict(options.clone()),
    ];

    (args, options)
}

/// Pick a model hint from the spec's `modelPreferences.hints` chain.
/// We honor the first entry whose `name` is non-empty — that's the
/// MCP convention for "the server suggests this model first".
fn pick_model_hint(prefs: Option<&JsonValue>) -> Option<String> {
    let prefs = prefs?;
    let hints = prefs.get("hints")?.as_array()?;
    for hint in hints {
        if let Some(name) = hint.get("name").and_then(|value| value.as_str()) {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Build the spec response shape for a successful sampling exchange.
/// `model` is the actual model the provider settled on (which may
/// differ from the request's hint chain when the host bridge or
/// router overrode it). `stopReason` defaults to `"endTurn"`; we
/// surface `"stopSequence"` when the request set explicit stop
/// strings, mirroring the reference SDK behavior — providers don't
/// currently bubble a fine-grained reason up to `llm_call`'s result.
fn build_spec_response(outcome: LlmOutcome, parsed: &SamplingRequest) -> JsonValue {
    let stop_reason = if parsed.stop_sequences.is_some() {
        "stopSequence"
    } else {
        "endTurn"
    };

    let model = if outcome.model.is_empty() {
        pick_model_hint(parsed.model_preferences.as_ref()).unwrap_or_default()
    } else {
        outcome.model
    };

    json!({
        "role": "assistant",
        "content": {
            "type": "text",
            "text": outcome.text,
        },
        "model": model,
        "stopReason": stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::value::VmDictExt;
    use std::sync::Arc;

    fn minimal_request() -> JsonValue {
        json!({
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "hi"}}
            ],
            "maxTokens": 64,
        })
    }

    #[test]
    fn parse_rejects_missing_messages() {
        let err = parse_sampling_request(&json!({"maxTokens": 1})).unwrap_err();
        assert!(err.contains("messages"));
    }

    #[test]
    fn parse_rejects_empty_messages() {
        let err = parse_sampling_request(&json!({"messages": [], "maxTokens": 1})).unwrap_err();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn parse_rejects_missing_max_tokens() {
        let err = parse_sampling_request(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]
        }))
        .unwrap_err();
        assert!(err.contains("maxTokens"));
    }

    #[test]
    fn parse_rejects_zero_max_tokens() {
        let err = parse_sampling_request(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
            "maxTokens": 0,
        }))
        .unwrap_err();
        assert!(err.contains("positive"));
    }

    #[test]
    fn parse_rejects_unknown_role() {
        let err = parse_sampling_request(&json!({
            "messages": [{"role": "tool", "content": {}}],
            "maxTokens": 1,
        }))
        .unwrap_err();
        assert!(err.contains("'user'/'assistant'/'system'"));
    }

    #[test]
    fn parse_extracts_optional_fields() {
        let parsed = parse_sampling_request(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
            "maxTokens": 32,
            "systemPrompt": "be brief",
            "temperature": 0.2,
            "stopSequences": ["END"],
            "modelPreferences": {"hints": [{"name": "claude-3-5-sonnet"}]},
            "includeContext": "thisServer",
            "metadata": {"trace": "abc"},
        }))
        .unwrap();
        assert_eq!(parsed.max_tokens, 32);
        assert_eq!(parsed.system.as_deref(), Some("be brief"));
        assert_eq!(parsed.temperature, Some(0.2));
        assert_eq!(
            parsed.stop_sequences.as_deref(),
            Some(&["END".to_string()][..])
        );
        assert_eq!(parsed.include_context.as_deref(), Some("thisServer"));
        assert_eq!(
            pick_model_hint(parsed.model_preferences.as_ref()),
            Some("claude-3-5-sonnet".to_string())
        );
    }

    #[test]
    fn pick_model_hint_picks_first_non_empty() {
        let prefs = json!({"hints": [{"name": ""}, {"name": "gpt-4"}]});
        assert_eq!(pick_model_hint(Some(&prefs)), Some("gpt-4".to_string()));
    }

    #[test]
    fn pick_model_hint_returns_none_for_empty_chain() {
        assert!(pick_model_hint(None).is_none());
        assert!(pick_model_hint(Some(&json!({"hints": []}))).is_none());
        assert!(pick_model_hint(Some(&json!({}))).is_none());
    }

    #[test]
    fn coerce_bridge_response_nil_declines() {
        match coerce_bridge_response(VmValue::Nil) {
            ApprovalDecision::Decline(_) => {}
            other => panic!("expected decline, got {other:?}"),
        }
    }

    #[test]
    fn coerce_bridge_response_true_accepts_with_no_overrides() {
        match coerce_bridge_response(VmValue::Bool(true)) {
            ApprovalDecision::Accept(map) => assert!(map.is_empty()),
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn coerce_bridge_response_accept_with_options() {
        let mut dict = crate::value::DictMap::new();
        dict.put_str("action", "accept");
        let mut options = crate::value::DictMap::new();
        options.put_str("provider", "mock");
        dict.insert(crate::value::intern_key("options"), VmValue::dict(options));
        match coerce_bridge_response(VmValue::dict(dict)) {
            ApprovalDecision::Accept(map) => {
                assert_eq!(
                    map.get("provider").map(|v| v.display()).as_deref(),
                    Some("mock")
                );
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn coerce_bridge_response_decline_with_message() {
        let mut dict = crate::value::DictMap::new();
        dict.put_str("action", "decline");
        dict.put_str("message", "rate limit");
        match coerce_bridge_response(VmValue::dict(dict)) {
            ApprovalDecision::Decline(reason) => assert_eq!(reason, "rate limit"),
            other => panic!("expected decline, got {other:?}"),
        }
    }

    #[test]
    fn coerce_bridge_response_bare_dict_is_overrides() {
        let mut dict = crate::value::DictMap::new();
        dict.put_str("provider", "mock");
        match coerce_bridge_response(VmValue::dict(dict)) {
            ApprovalDecision::Accept(map) => {
                assert_eq!(
                    map.get("provider").map(|v| v.display()).as_deref(),
                    Some("mock")
                );
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    fn outcome(text: &str, model: &str) -> LlmOutcome {
        LlmOutcome {
            text: text.to_string(),
            model: model.to_string(),
        }
    }

    #[test]
    fn build_spec_response_flags_stop_sequence() {
        let parsed = parse_sampling_request(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
            "maxTokens": 4,
            "stopSequences": ["END"],
        }))
        .unwrap();
        let response = build_spec_response(outcome("done", "actual-model"), &parsed);
        assert_eq!(response["stopReason"], json!("stopSequence"));
        assert_eq!(response["role"], json!("assistant"));
        assert_eq!(response["content"]["type"], json!("text"));
        assert_eq!(response["content"]["text"], json!("done"));
        assert_eq!(response["model"], json!("actual-model"));
    }

    #[test]
    fn build_spec_response_default_stop_reason_is_end_turn() {
        let parsed = parse_sampling_request(&minimal_request()).unwrap();
        let response = build_spec_response(outcome("done", ""), &parsed);
        assert_eq!(response["stopReason"], json!("endTurn"));
    }

    #[test]
    fn build_spec_response_falls_back_to_hint_when_outcome_model_missing() {
        let parsed = parse_sampling_request(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
            "maxTokens": 4,
            "modelPreferences": {"hints": [{"name": "claude-3-5-sonnet"}]},
        }))
        .unwrap();
        let response = build_spec_response(outcome("done", ""), &parsed);
        assert_eq!(response["model"], json!("claude-3-5-sonnet"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_with_no_bridge_declines() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "s-1",
            "method": SAMPLING_METHOD,
            "params": minimal_request(),
        });
        let response = dispatch_inbound_sampling("mock", &request).await;
        assert_eq!(response["id"], json!("s-1"));
        assert_eq!(response["error"]["code"], json!(-32603));
        assert_eq!(
            response["error"]["data"]["type"],
            json!("mcp.samplingDeclined")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_with_invalid_params_returns_invalid_params() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": SAMPLING_METHOD,
            "params": {"messages": []},
        });
        let response = dispatch_inbound_sampling("mock", &request).await;
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["error"]["code"], json!(-32602));
    }

    /// Test-only `HostCallBridge` that approves any sampling request
    /// and injects the supplied options as overrides on the inbound
    /// `llm_call`. Lets the integration test below avoid the private
    /// `HostMock` registration API while still exercising the bridge
    /// → llm_call path end-to-end.
    struct ApproveSamplingBridge {
        overrides: crate::value::DictMap,
    }

    impl crate::stdlib::host::HostCallBridge for ApproveSamplingBridge {
        fn dispatch(
            &self,
            capability: &str,
            operation: &str,
            _params: &crate::value::DictMap,
        ) -> Result<Option<VmValue>, VmError> {
            if capability == "mcp" && operation == "sample" {
                let mut envelope: crate::value::DictMap = crate::value::DictMap::new();
                envelope.put_str("action", "accept");
                envelope.insert(
                    crate::value::intern_key("options"),
                    VmValue::dict(self.overrides.clone()),
                );
                Ok(Some(VmValue::dict(envelope)))
            } else {
                Ok(None)
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_with_mock_bridge_routes_to_llm_call() {
        // Reset before installing mocks so this test is order-independent.
        crate::llm::mock::reset_llm_mock_state();

        // Push a builtin LLM mock so `llm_call` returns deterministic
        // text without hitting any real provider. Mock interception
        // applies whenever any builtin mock is installed, regardless
        // of the requested provider — see `MockProvider::should_intercept`.
        crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
            text: "sampled text".to_string(),
            tool_calls: Vec::new(),
            match_pattern: None,
            consume_on_match: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            model: "mock-model".to_string(),
            provider: Some("mock".to_string()),
            blocks: None,
            logprobs: Vec::new(),
            error: None,
        });

        // Install an approving bridge that forces provider=mock so the
        // call resolves through MockProvider deterministically.
        let mut overrides: crate::value::DictMap = crate::value::DictMap::new();
        overrides.put_str("provider", "mock");
        overrides.put_str("model", "mock-model");
        crate::stdlib::host::set_host_call_bridge(Arc::new(ApproveSamplingBridge { overrides }));

        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": SAMPLING_METHOD,
            "params": {
                "messages": [
                    {"role": "user", "content": {"type": "text", "text": "ping"}}
                ],
                "maxTokens": 32,
                "modelPreferences": {"hints": [{"name": "mock-model"}]},
            },
        });

        let response = dispatch_inbound_sampling("test-server", &request).await;

        crate::llm::mock::reset_llm_mock_state();
        crate::stdlib::host::clear_host_call_bridge();

        assert_eq!(response["id"], json!(7));
        assert!(
            response.get("result").is_some(),
            "expected success result, got {response:?}"
        );
        assert_eq!(response["result"]["role"], json!("assistant"));
        assert_eq!(response["result"]["content"]["type"], json!("text"));
        assert_eq!(response["result"]["content"]["text"], json!("sampled text"));
        assert_eq!(response["result"]["stopReason"], json!("endTurn"));
        assert_eq!(response["result"]["model"], json!("mock-model"));
    }
}
