//! Process-local MCP server registry for lazy boot + skill-scoped
//! binding (harn#75).
//!
//! Holds the declared MCP server specs from `harn.toml` along with a
//! live client handle once a server has been booted. Servers marked
//! `lazy = true` stay idle until the first `mcp_ensure_active` or
//! `mcp_call` targets them.
//!
//! Ref-counting semantics:
//! - `ensure_active(name)` — connects if needed; each call increments
//!   the active-binder count by 1.
//! - `release(name)` — decrements the binder count; when it reaches 0
//!   AND `keep_alive_ms` has elapsed, the client is disconnected.
//! - A non-lazy connection is held "forever" (ref count pinned at 1)
//!   until process exit.
//!
//! The registry lives per-process as a `Mutex<RegistryInner>` — agent
//! loops, skill activations, and the CLI's `connect_mcp_servers`
//! function all operate on the same instance.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::mcp::{connect_mcp_server_from_json, VmMcpClientHandle};
use crate::value::VmError;

pub const DEFAULT_MCP_TRANSPORT: &str = "stdio";

/// One server's registration. Mirrors `McpServerConfig` but is owned
/// by the VM side so harn-cli can hand off and forget.
#[derive(Clone, Debug)]
pub struct RegisteredMcpServer {
    pub name: String,
    pub spec: serde_json::Value,
    pub lazy: bool,
    /// Optional card source (URL or local path) from `harn.toml`.
    pub card: Option<String>,
    /// How long to keep the connection alive after the last release.
    /// `None` → disconnect immediately at refcount 0.
    pub keep_alive: Option<Duration>,
}

struct ActiveConnection {
    handle: VmMcpClientHandle,
    /// Number of active binders (skills holding the server open).
    /// Non-lazy connections pin this to `usize::MAX / 2` so `release`
    /// on them is a no-op.
    ref_count: usize,
    /// Timestamp of the last `release` call — used to honor
    /// `keep_alive` without immediate disconnect.
    last_released_at: Option<Instant>,
}

struct RegistryInner {
    servers: BTreeMap<String, RegisteredMcpServer>,
    active: BTreeMap<String, ActiveConnection>,
}

impl RegistryInner {
    const fn new() -> Self {
        Self {
            servers: BTreeMap::new(),
            active: BTreeMap::new(),
        }
    }
}

static REGISTRY: Mutex<RegistryInner> = Mutex::new(RegistryInner::new());

/// Register every MCP server declared in `harn.toml`. Idempotent —
/// re-registering a server replaces its spec and card but preserves any
/// live connection.
pub fn register_servers(servers: Vec<RegisteredMcpServer>) {
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    for server in servers {
        guard.servers.insert(server.name.clone(), server);
    }
}

/// Returns `true` when a server with `name` is registered (lazy or
/// eager). Used by `mcp_ensure_active` / skill activation paths to
/// produce useful "not found" errors.
pub fn is_registered(name: &str) -> bool {
    REGISTRY
        .lock()
        .expect("mcp registry poisoned")
        .servers
        .contains_key(name)
}

/// Return a cloned registration record, or `None`. Used by the CLI
/// helper so manifest lookups don't need to reparse harn.toml.
pub fn get_registration(name: &str) -> Option<RegisteredMcpServer> {
    REGISTRY
        .lock()
        .expect("mcp registry poisoned")
        .servers
        .get(name)
        .cloned()
}

/// Snapshot every declared MCP server's registration. Lets harn-vm enumerate
/// the configured servers (e.g. to find OAuth-backed ones for bulk re-auth,
/// harn#3358) without reparsing `harn.toml` — the CLI already registered them.
pub fn all_registrations() -> Vec<RegisteredMcpServer> {
    REGISTRY
        .lock()
        .expect("mcp registry poisoned")
        .servers
        .values()
        .cloned()
        .collect()
}

/// Drop every registration and active connection. Used by
/// `reset_thread_local_state` and tests.
pub fn reset() {
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    guard.servers.clear();
    guard.active.clear();
}

/// Install a pre-connected handle against a server name so eager-start
/// flows can register "already running" servers without going through
/// the lazy-boot path. Used by the CLI for non-lazy servers.
pub fn install_active(name: &str, handle: VmMcpClientHandle) {
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    guard.active.insert(
        name.to_string(),
        ActiveConnection {
            handle,
            ref_count: usize::MAX / 2,
            last_released_at: None,
        },
    );
}

/// Look up the live client handle by server name. Returns `None` when
/// the server is registered but not currently connected (use
/// `ensure_active` to force a lazy boot).
pub fn active_handle(name: &str) -> Option<VmMcpClientHandle> {
    REGISTRY
        .lock()
        .expect("mcp registry poisoned")
        .active
        .get(name)
        .map(|a| a.handle.clone())
}

/// Connect to a registered server if not already connected, and bump
/// its binder count. Returns the live client handle.
///
/// Fails with `VmError::Runtime` when:
/// - `name` isn't registered.
/// - The connection attempt itself fails.
pub async fn ensure_active(name: &str) -> Result<VmMcpClientHandle, VmError> {
    // Fast path: connection exists. Bump ref count under the lock.
    {
        let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
        if let Some(active) = guard.active.get_mut(name) {
            if active.ref_count != usize::MAX / 2 {
                active.ref_count = active.ref_count.saturating_add(1);
            }
            active.last_released_at = None;
            return Ok(active.handle.clone());
        }
    }

    // Slow path: fetch spec, connect outside the lock (connect is
    // async, can't await while holding Mutex), then install.
    let spec = {
        let guard = REGISTRY.lock().expect("mcp registry poisoned");
        guard.servers.get(name).cloned()
    };
    let registration = spec.ok_or_else(|| {
        VmError::Runtime(format!(
            "mcp: no server named '{name}' is registered (check harn.toml)"
        ))
    })?;

    let handle = connect_mcp_server_from_json(&registration.spec).await?;

    // Install under the lock. Handle race: another task may have
    // connected the same server in the meantime; keep the incumbent handle.
    // Dropping the redundant handle tears down its transport via the MCP
    // client's drop guards.
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    match guard.active.get_mut(name) {
        Some(existing) => {
            if existing.ref_count != usize::MAX / 2 {
                existing.ref_count = existing.ref_count.saturating_add(1);
            }
            existing.last_released_at = None;
            Ok(existing.handle.clone())
        }
        None => {
            guard.active.insert(
                name.to_string(),
                ActiveConnection {
                    handle: handle.clone(),
                    ref_count: 1,
                    last_released_at: None,
                },
            );
            Ok(handle)
        }
    }
}

/// Decrement the binder count for `name`. When it reaches 0 (and the
/// registration was lazy), marks the timestamp so `sweep_expired` can
/// disconnect after the keep-alive window.
pub fn release(name: &str) {
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    let keep_alive = guard
        .servers
        .get(name)
        .and_then(|s| s.keep_alive)
        .unwrap_or(Duration::ZERO);
    let to_drop = match guard.active.get_mut(name) {
        Some(active) => {
            // Non-lazy servers have ref_count pinned; release is no-op.
            if active.ref_count == usize::MAX / 2 {
                return;
            }
            if active.ref_count > 1 {
                active.ref_count -= 1;
                None
            } else {
                active.ref_count = 0;
                active.last_released_at = Some(Instant::now());
                if keep_alive.is_zero() {
                    Some(active.handle.clone())
                } else {
                    None
                }
            }
        }
        None => None,
    };
    if to_drop.is_some() {
        guard.active.remove(name);
    }
}

/// Force-disconnect servers whose keep-alive has elapsed. Called
/// periodically by the agent loop's post-turn housekeeping — never
/// blocks on network, just drops the handle.
pub fn sweep_expired() {
    let mut guard = REGISTRY.lock().expect("mcp registry poisoned");
    let now = Instant::now();
    let mut expired: Vec<String> = Vec::new();
    for (name, active) in guard.active.iter() {
        if active.ref_count != 0 {
            continue;
        }
        let Some(last) = active.last_released_at else {
            continue;
        };
        let ka = guard
            .servers
            .get(name)
            .and_then(|s| s.keep_alive)
            .unwrap_or(Duration::ZERO);
        if now.duration_since(last) >= ka {
            expired.push(name.clone());
        }
    }
    for name in expired {
        guard.active.remove(&name);
    }
}

/// Diagnostic snapshot of the registry — used by the `mcp_server_info`
/// builtin's extended mode and by tests.
#[derive(Clone, Debug)]
pub struct RegistryStatus {
    pub name: String,
    pub transport: String,
    pub url: Option<String>,
    pub lazy: bool,
    pub active: bool,
    pub ref_count: usize,
    pub card: Option<String>,
}

fn spec_string(spec: &serde_json::Value, key: &str) -> Option<String> {
    spec.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn spec_transport(spec: &serde_json::Value) -> String {
    spec_string(spec, "transport").unwrap_or_else(|| DEFAULT_MCP_TRANSPORT.to_string())
}

pub fn snapshot_status() -> Vec<RegistryStatus> {
    let guard = REGISTRY.lock().expect("mcp registry poisoned");
    let mut out = Vec::new();
    for (name, server) in guard.servers.iter() {
        let active = guard.active.get(name);
        out.push(RegistryStatus {
            name: name.clone(),
            transport: spec_transport(&server.spec),
            url: spec_string(&server.spec, "url"),
            lazy: server.lazy,
            active: active.is_some(),
            ref_count: active.map(|a| a.ref_count).unwrap_or(0),
            card: server.card.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is process-global, so unit tests must serialize to
    /// avoid interfering with each other. `TEST_LOCK` wraps every test
    /// body — cheaper than spinning up a new process per test.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn make_spec(name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "command": "/bin/true",
            "args": [],
        })
    }

    #[test]
    fn spec_transport_defaults_to_stdio() {
        assert_eq!(spec_transport(&make_spec("x")), DEFAULT_MCP_TRANSPORT);
        assert_eq!(
            spec_transport(&serde_json::json!({"name": "x", "transport": "http"})),
            "http",
        );
    }

    #[test]
    fn register_and_snapshot() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        register_servers(vec![RegisteredMcpServer {
            name: "x".into(),
            spec: make_spec("x"),
            lazy: true,
            card: Some("card.json".into()),
            keep_alive: None,
        }]);
        let snap = snapshot_status();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "x");
        assert_eq!(snap[0].transport, DEFAULT_MCP_TRANSPORT);
        assert!(snap[0].url.is_none());
        assert!(snap[0].lazy);
        assert!(!snap[0].active);
        assert_eq!(snap[0].card.as_deref(), Some("card.json"));
    }

    #[test]
    fn release_on_unknown_is_noop() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        release("doesnt-exist");
    }

    #[test]
    fn is_registered_reflects_state() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        assert!(!is_registered("a"));
        register_servers(vec![RegisteredMcpServer {
            name: "a".into(),
            spec: make_spec("a"),
            lazy: false,
            card: None,
            keep_alive: None,
        }]);
        assert!(is_registered("a"));
    }
}
