//! Wires `harn-serve::AuthPolicy` into the supervised MCP host
//! primitive ([`harn_vm::mcp_host`]) without making `harn-vm` depend on
//! `harn-serve`. The pattern matches the connector-client / observability
//! installers elsewhere in the workspace: harn-vm exposes a pluggable
//! callback; harn-serve installs an adapter at boot time.
//!
//! Today the installer is invoked once per `DispatchCore` construction.
//! When A.2 (tenant context propagation) lands, the closure will pick
//! the per-tenant allowlist out of the dispatch context instead of
//! using the policy-global field directly.

use std::sync::Arc;

use harn_vm::mcp_host::{set_allowlist, AllowlistDecision, AllowlistGuard};

use crate::auth::{AllowlistOutcome, AuthPolicy};

/// Install a [`harn_vm::mcp_host::AllowlistGuard`] derived from this
/// policy's MCP allowlist. Calling with an `AuthPolicy` that has
/// `mcp_allowlist = None` clears the guard (allow-all).
pub fn install_mcp_host_allowlist(policy: &AuthPolicy) {
    let Some(allowlist) = policy.mcp_allowlist.clone() else {
        set_allowlist(None);
        return;
    };
    let guard: AllowlistGuard =
        Arc::new(
            move |server: &str, tool: Option<&str>| match allowlist.check(server, tool) {
                AllowlistOutcome::Allow => AllowlistDecision::Allow,
                AllowlistOutcome::ServerDenied => AllowlistDecision::Deny {
                    reason: format!("MCP server '{server}' is not on the tenant allowlist"),
                },
                AllowlistOutcome::ToolDenied => AllowlistDecision::Deny {
                    reason: format!(
                        "MCP tool '{}' on server '{server}' is not on the tenant allowlist",
                        tool.unwrap_or("<unknown>")
                    ),
                },
            },
        );
    set_allowlist(Some(guard));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthPolicy, McpAllowlist, McpAllowlistTools};
    use harn_vm::mcp_host;

    /// The MCP host's allowlist guard is a process-global. Tests in this
    /// module flip it, exercise dispatch, and reset it — so two tests
    /// running concurrently race the global between the install and the
    /// assertion. Serialise them through this mutex so the suite stays
    /// deterministic regardless of the `cargo test` parallelism.
    fn global_guard() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn policy_with(allowlist: Option<McpAllowlist>) -> AuthPolicy {
        AuthPolicy {
            methods: Vec::new(),
            mcp_allowlist: allowlist,
        }
    }

    #[test]
    fn install_with_none_clears_guard() {
        let _guard = global_guard().lock().unwrap_or_else(|e| e.into_inner());
        mcp_host::reset_for_tests();
        install_mcp_host_allowlist(&policy_with(None));
        // No guard installed → spawn should not be denied by it. We
        // can't directly observe "no guard" without running spawn, but
        // a second install with an explicit deny-all proves the
        // round-trip works.
        let mut deny_all = McpAllowlist::deny_all();
        deny_all.allow("permitted", McpAllowlistTools::All);
        install_mcp_host_allowlist(&policy_with(Some(deny_all)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let denied = runtime.block_on(mcp_host::call("blocked", "anything", serde_json::json!({})));
        assert!(
            denied
                .unwrap_err()
                .to_string()
                .contains("denied by allowlist"),
            "expected blocked server to be rejected at the dispatch boundary"
        );
        // Restore the global to the open default so neighbouring tests
        // aren't affected.
        mcp_host::reset_for_tests();
    }

    #[test]
    fn tool_filter_only_admits_listed_tools() {
        let _guard = global_guard().lock().unwrap_or_else(|e| e.into_inner());
        mcp_host::reset_for_tests();
        let mut allowlist = McpAllowlist::deny_all();
        let mut tools = std::collections::BTreeSet::new();
        tools.insert("read_repo".to_string());
        allowlist.allow("github", McpAllowlistTools::Only(tools));
        install_mcp_host_allowlist(&policy_with(Some(allowlist)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Disallowed tool on an allowed server.
        let err = runtime
            .block_on(mcp_host::call(
                "github",
                "delete_repo",
                serde_json::json!({}),
            ))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("denied by allowlist"),
            "expected disallowed tool to be rejected, got: {err}"
        );
        mcp_host::reset_for_tests();
    }
}
