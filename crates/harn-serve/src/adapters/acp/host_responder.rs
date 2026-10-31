//! Embedder-owned answers to `host/capabilities` and `host/call`.
//!
//! Without a responder these requests go to the connected client, which only
//! works for a client that is itself the host. An embedder that serves ACP to
//! clients it does not control (any standard editor) answers them in-process
//! instead, so the session's host is the server, not whoever connected.

use super::AcpJsonRpcError;

/// Answers `host/capabilities` and `host/call` for every session of a server.
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

/// The methods a [`HostResponder`] owns when one is configured.
pub(super) fn is_host_method(method: &str) -> bool {
    matches!(method, "host/capabilities" | "host/call")
}
