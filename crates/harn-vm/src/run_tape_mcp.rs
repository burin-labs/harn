//! MCP servers on a run tape and in a branch prefix.
//!
//! A recording keeps each server start (what ran and the server info it
//! negotiated) and every JSON-RPC exchange. A branch never starts a server
//! inside its prefix: the client is created deferred, its exchanges are served
//! from the tape, and the server starts on the first exchange after the branch
//! goes live. That start must be the server the recording ran: the same
//! command, server version and tool list, or the branch diverges.

use std::collections::{BTreeMap, VecDeque};

use super::{current_branch, divergence_error};
use crate::testbench::tape::{active_recorder, content_hash, EventTape, TapeRecordKind};
use crate::value::VmError;

/// What the recording knew about one server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedMcpServer {
    pub command: String,
    pub discovery: serde_json::Value,
    /// Digest of the recorded `tools/list` result, when the run listed tools.
    pub tools_digest: Option<String>,
}

/// The MCP part of a branch: recorded starts and the prefix's exchanges.
#[derive(Default)]
pub(super) struct BranchMcp {
    servers: BTreeMap<String, RecordedMcpServer>,
    /// Prefix exchanges per (server, method, request key), in recorded order.
    exchanges: BTreeMap<(String, String, String), VecDeque<serde_json::Value>>,
}

/// Read the MCP records a branch at `k` serves.
pub(super) fn recorded_mcp(tape: &EventTape, k: u64) -> Result<BranchMcp, String> {
    let mut mcp = BranchMcp::default();
    let decode = |payload| -> Result<serde_json::Value, String> {
        serde_json::from_slice(&tape.resolve_payload(payload)?)
            .map_err(|err| format!("tape MCP payload: {err}"))
    };
    let mut model_calls = 0u64;
    for record in &tape.records {
        match &record.kind {
            TapeRecordKind::LlmCall {
                call_index: Some(_),
                ..
            } => model_calls += 1,
            TapeRecordKind::McpServerStart {
                server,
                command,
                discovery,
            } => {
                mcp.servers
                    .entry(server.clone())
                    .or_insert(RecordedMcpServer {
                        command: command.clone(),
                        discovery: decode(discovery)?,
                        tools_digest: None,
                    });
            }
            TapeRecordKind::McpJsonRpc {
                server,
                method,
                request_payload,
                response_payload,
                ..
            } => {
                let response = decode(response_payload)?;
                if method == "tools/list" {
                    if let Some(recorded) = mcp.servers.get_mut(server) {
                        recorded
                            .tools_digest
                            .get_or_insert_with(|| result_digest(&response));
                    }
                }
                if model_calls <= k {
                    let key = request_key(&decode(request_payload)?);
                    mcp.exchanges
                        .entry((server.clone(), method.clone(), key))
                        .or_default()
                        .push_back(response);
                }
            }
            _ => {}
        }
    }
    Ok(mcp)
}

/// A request's identity across runs: its params without the per-call
/// progress token the client mints.
fn request_key(request: &serde_json::Value) -> String {
    let mut params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(meta) = params
        .get_mut("_meta")
        .and_then(serde_json::Value::as_object_mut)
    {
        meta.remove("progressToken");
        if meta.is_empty() {
            params
                .as_object_mut()
                .expect("params with _meta is an object")
                .remove("_meta");
        }
    }
    content_hash(&crate::canonical_json::to_vec(&params))
}

fn result_digest(response: &serde_json::Value) -> String {
    let result = response.get("result").unwrap_or(response);
    content_hash(&crate::canonical_json::to_vec(result))
}

/// Record a server start on the active tape, redacted like its exchanges.
pub(crate) fn record_mcp_start(server: &str, command: &str, discovery: &serde_json::Value) {
    let Some(recorder) = active_recorder() else {
        return;
    };
    let discovery = crate::redact::current_policy().redact_json(discovery);
    let discovery = recorder.payload_from_bytes(crate::canonical_json::to_vec(&discovery));
    recorder.record(TapeRecordKind::McpServerStart {
        server: server.to_string(),
        command: command.to_string(),
        discovery,
    });
}

/// How a client for `server` starts.
pub(crate) enum McpStart {
    /// Start it now.
    Live,
    /// Inside a branch prefix: do not start it; serve from the recording.
    Deferred(RecordedMcpServer),
}

/// Decide whether `server`, run as `command`, starts now. In a branch
/// prefix a server the recording never started, or started with another
/// command, diverges.
pub(crate) fn mcp_start(server: &str, command: &str) -> Result<McpStart, VmError> {
    let Some(branch) = current_branch() else {
        return Ok(McpStart::Live);
    };
    let mut state = branch.state();
    if state.live {
        return Ok(McpStart::Live);
    }
    let call = state.report.served_calls + 1;
    match state.mcp.servers.get(server).cloned() {
        Some(recorded) if recorded.command == command => Ok(McpStart::Deferred(recorded)),
        Some(recorded) => Err(state.diverge(
            call,
            "MCP server",
            format!(
                "`{server}` would run `{command}`; the recording ran `{}`",
                recorded.command
            ),
        )),
        None => Err(state.diverge(
            call,
            "MCP server",
            format!("`{server}` would start, which the recording never did"),
        )),
    }
}

/// Serve one exchange from a branch prefix. `None` means it runs live.
pub(crate) fn serve_mcp_exchange(
    server: &str,
    method: &str,
    request: &serde_json::Value,
) -> Option<Result<serde_json::Value, VmError>> {
    let branch = current_branch()?;
    let mut state = branch.state();
    if let Some(divergence) = &state.report.divergence {
        return Some(Err(divergence_error(divergence)));
    }
    if state.live {
        return None;
    }
    let request = crate::redact::current_policy().redact_json(request);
    let slot = (
        server.to_string(),
        method.to_string(),
        request_key(&request),
    );
    let served = state
        .mcp
        .exchanges
        .get_mut(&slot)
        .and_then(VecDeque::pop_front);
    let call = state.report.served_calls + 1;
    Some(match served {
        Some(response) => {
            state.report.served_reads += 1;
            Ok(response)
        }
        None => Err(state.diverge(
            call,
            "MCP exchange",
            format!("`{method}` to `{server}` is not in the recorded prefix"),
        )),
    })
}

/// Check a server started after the branch went live against the one the
/// recording ran: its server version and its tool list.
pub(crate) fn check_mcp_identity(
    server: &str,
    recorded: &RecordedMcpServer,
    discovery: &serde_json::Value,
    tools: Option<&serde_json::Value>,
) -> Result<(), VmError> {
    let version = |discovery: &serde_json::Value| {
        discovery
            .pointer("/serverInfo/version")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let mismatch = if version(&recorded.discovery) != version(discovery) {
        Some(format!(
            "`{server}` is version {}; the recording ran {}",
            version(discovery),
            version(&recorded.discovery)
        ))
    } else {
        match (&recorded.tools_digest, tools) {
            (Some(expected), Some(tools)) if *expected != result_digest(tools) => Some(format!(
                "`{server}` lists different tools than the recording's"
            )),
            _ => None,
        }
    };
    let Some(detail) = mismatch else {
        return Ok(());
    };
    let Some(branch) = current_branch() else {
        return Ok(());
    };
    let mut state = branch.state();
    let call = state.report.served_calls + 1;
    Err(state.diverge(call, "MCP server identity", detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_key_ignores_only_the_progress_token() {
        let request = |meta: serde_json::Value| {
            serde_json::json!({"method": "tools/call", "params": {"name": "a", "_meta": meta}})
        };
        let plain = serde_json::json!({"method": "tools/call", "params": {"name": "a"}});
        assert_eq!(
            request_key(&request(serde_json::json!({"progressToken": "t-1"}))),
            request_key(&plain)
        );
        assert_ne!(
            request_key(&request(serde_json::json!({"progressToken": "t-1", "trace": 1}))),
            request_key(&plain)
        );
    }
}
