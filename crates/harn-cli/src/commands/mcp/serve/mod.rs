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
    self::http::initialize_api_key,
    self::types::{
        ConnectionState, McpListChangeKind, McpLogNotification, McpResourceNotification,
        McpTaskNotification, TriggerReplayRequest,
    },
    self::util::trigger_replay_steering_from_request,
    self::watchers::severity_for_event,
    crate::cli::OrchestratorLocalArgs,
    crate::commands::orchestrator::common::{load_local_runtime, read_topic},
    axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE},
    axum::http::StatusCode,
    axum::Json,
    futures::StreamExt,
    harn_vm::event_log::{EventLog, LogEvent, Topic},
    harn_vm::mcp_protocol,
    serde_json::{json, Value as JsonValue},
    std::collections::BTreeMap,
    time::OffsetDateTime,
    tokio::sync::broadcast,
};

#[cfg(test)]
#[path = "../serve_tests.rs"]
mod serve_tests;

#[cfg(test)]
#[path = "../mcp_rc_compat_tests.rs"]
mod mcp_rc_compat_tests;

pub(super) const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub(super) const MCP_SESSION_HEADER: &str = "mcp-session-id";
pub(super) const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
pub(super) const DEPRECATION_HEADER: &str = "deprecation";
pub(super) const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
pub(super) const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
pub(super) const DEFAULT_TASK_TTL_MS: u64 = 10 * 60 * 1000;
pub(super) const MAX_TASK_TTL_MS: u64 = 60 * 60 * 1000;
pub(super) const LOG_NOTIFICATION_CAPACITY: usize = 256;

pub(in crate::commands::mcp::serve) const LOG_STREAM_BINDINGS: &[types::McpLogStreamBinding] = &[
    types::McpLogStreamBinding {
        topic: harn_vm::SECRET_SCAN_AUDIT_TOPIC,
        logger: "harn.audit.secret_scan",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Notice,
    },
    types::McpLogStreamBinding {
        topic: harn_vm::SIGNATURE_VERIFY_AUDIT_TOPIC,
        logger: "harn.audit.signature_verify",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Notice,
    },
    types::McpLogStreamBinding {
        topic: harn_vm::egress::EGRESS_AUDIT_TOPIC,
        logger: "harn.connectors.egress.audit",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Notice,
    },
    types::McpLogStreamBinding {
        topic: harn_vm::TRIGGER_OPERATION_AUDIT_TOPIC,
        logger: "harn.trigger.operations.audit",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Notice,
    },
    types::McpLogStreamBinding {
        topic: crate::commands::orchestrator::common::TRIGGER_DLQ_TOPIC,
        logger: "harn.trigger.dlq",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Warning,
    },
    types::McpLogStreamBinding {
        topic: ACTION_GRAPH_TOPIC,
        logger: "harn.observability.action_graph",
        default_level: harn_vm::mcp_protocol::McpLogLevel::Debug,
    },
];

pub(crate) async fn run(args: &McpServeArgs) -> Result<(), String> {
    let service = Arc::new(McpOrchestratorService::new(args)?);
    match args.transport {
        McpServeTransport::Stdio => stdio::run_stdio(service).await,
        McpServeTransport::Http => http::run_http(service, args).await,
    }
}
