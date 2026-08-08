use std::sync::Arc;

use crate::cli::{McpServeArgs, McpServeTransport};

mod http;
mod protocol;
mod resources;
mod rpc_bridge;
mod service;
mod stdio;
mod tools;
mod types;
mod util;
mod watchers;

pub(crate) use http::http_router_for_service;
pub(crate) use types::McpOrchestratorService;

#[cfg(test)]
pub(crate) use http::http_router_for_local;

// Re-exports consumed by `serve_tests.rs` via its `use super::*;`
// import. The pre-split serve.rs put all of these directly in scope as
// `use` statements at module level; we surface the same surface here so
// the existing tests keep compiling without changes. Visibility is
// scoped to `serve` so child modules (notably `serve_tests`) can see
// them while keeping the surface invisible from outside the module.
#[cfg(test)]
#[allow(unused_imports)]
pub(in crate::commands::mcp::serve) use {
    self::types::{ConnectionState, TriggerReplayRequest},
    self::util::trigger_replay_steering_from_request,
    crate::cli::OrchestratorLocalArgs,
    crate::commands::orchestrator::common::{load_local_runtime, read_topic},
    axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE},
    axum::http::StatusCode,
    axum::Json,
    futures::StreamExt,
    harn_vm::event_log::{EventLog, Topic},
    harn_vm::mcp_protocol,
    serde_json::{json, Value as JsonValue},
    std::collections::BTreeMap,
    time::OffsetDateTime,
};

#[cfg(test)]
#[path = "../serve_tests.rs"]
mod serve_tests;

#[cfg(test)]
#[path = "../mcp_compat_tests.rs"]
mod mcp_compat_tests;

pub(super) use harn_vm::mcp_protocol::{
    MCP_HEADER_PROTOCOL_VERSION as MCP_PROTOCOL_HEADER, PROTOCOL_VERSION as MCP_PROTOCOL_VERSION,
};
pub(super) const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
pub(super) const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
pub(super) const DEFAULT_TASK_TTL_MS: u64 = 10 * 60 * 1000;

pub(crate) async fn run(args: &McpServeArgs) -> Result<(), String> {
    let service = Arc::new(McpOrchestratorService::new(args)?);
    match args.transport {
        McpServeTransport::Stdio => stdio::run_stdio(service).await,
        McpServeTransport::Http => http::run_http(service, args).await,
    }
}
