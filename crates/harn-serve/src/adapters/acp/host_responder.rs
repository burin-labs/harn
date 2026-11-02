//! Embedder-owned answers to `host/capabilities` and `host/call`.
//!
//! Without a responder these requests go to the connected client, which only
//! works for a client that is itself the host. An embedder that serves ACP to
//! clients it does not control (any standard editor) answers them in-process
//! instead, so the session's host is the server, not whoever connected.

use std::path::Path;
use std::sync::Arc;

use super::AcpJsonRpcError;

/// Answers `host/capabilities` and `host/call` for one session's workspace.
///
/// Implementors enforce their concrete operation's authority and return
/// structured ACP errors for refused or unsupported operations. Calls run on
/// the blocking pool.
pub trait HostResponder: Send + Sync + 'static {
    fn respond(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, AcpJsonRpcError>;
}

/// Binds a [`HostResponder`] to the workspace root of the session it serves.
///
/// One server hosts sessions for different workspaces, so a responder is
/// never shared across roots: the server binds one for each prompt turn from
/// that session's current root. Binding runs on the blocking pool; implementors
/// may cache by root. Workspace anchor changes take effect on the next turn.
pub trait HostResponderBinder: Send + Sync + 'static {
    fn bind(&self, session_root: &Path) -> Result<Arc<dyn HostResponder>, String>;
}

/// The methods a [`HostResponder`] owns when one is configured.
pub(super) fn is_host_method(method: &str) -> bool {
    matches!(method, "host/capabilities" | "host/call")
}
