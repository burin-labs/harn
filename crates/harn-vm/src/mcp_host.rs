//! Supervised external-tool MCP host primitive (harn#2504, A.7).
//!
//! Wraps the connection-level state owned by [`crate::mcp_registry`] with
//! supervision, response caching, cross-server discovery, and allowlist
//! enforcement. The registry knows *which* servers are declared and how to
//! lazy-connect them; this module is the runtime that callers actually
//! reach for spawn/call/stop/discover/reload, and is the single home for
//! restart budgets, circuit-breaker state, and per-(server, tool, args)
//! response memoization.
//!
//! The split is deliberate: [`crate::mcp.rs`] owns the wire protocol,
//! [`crate::mcp_registry`] owns lifecycle bookkeeping, and this module
//! owns *policy* — the rules a hosted MCP server must follow before its
//! calls are dispatched and after they return. Allowlist evaluation is
//! delegated to a swappable [`McpAllowlist`] callback so harn-serve can
//! plug `AuthPolicy` in without harn-vm depending on harn-serve.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::mcp::{call_mcp_tool_with_hint, VmMcpClientHandle};
use crate::mcp_protocol::McpCacheHint;
use crate::mcp_registry::{self, RegisteredMcpServer};
use crate::value::VmError;

/// Default per-server restart budget. The supervisor allows this many
/// auto-restarts inside [`DEFAULT_RESTART_WINDOW`] before the server is
/// marked ejected; an ejected server fails fast until an explicit
/// `reload()` clears the state.
pub const DEFAULT_MAX_RESTARTS: u32 = 5;
pub const DEFAULT_RESTART_WINDOW: Duration = Duration::from_mins(5);

/// Default circuit-breaker thresholds applied to a hosted server when the
/// caller does not override them. Mirrors the
/// `circuit_breaker(name, threshold?, reset_ms?)` stdlib builtin defaults
/// (5 failures, 30s reset) so .harn-side and Rust-side breakers behave
/// consistently.
pub const DEFAULT_CIRCUIT_THRESHOLD: u32 = 5;
pub const DEFAULT_CIRCUIT_RESET: Duration = Duration::from_secs(30);

/// Initial backoff applied between restart attempts. Doubled on every
/// consecutive restart up to [`MAX_RESTART_BACKOFF`].
pub const INITIAL_RESTART_BACKOFF: Duration = Duration::from_millis(100);
pub const MAX_RESTART_BACKOFF: Duration = Duration::from_secs(5);

/// Cap on the response cache entries kept per (server, tool) pair so a
/// run-away script with high-arity arguments can't grow the cache without
/// bound. LRU semantics are approximated by dropping the oldest insertion
/// timestamp when the cap is reached.
pub const RESPONSE_CACHE_MAX_ENTRIES_PER_TOOL: usize = 64;

/// Per-server supervision state. Holds the restart-budget bookkeeping and
/// circuit-breaker state. Carved off from `mcp_registry::ActiveConnection`
/// so the registry can stay focused on lazy-boot / ref-counting and not
/// gain unrelated knobs.
#[derive(Debug)]
struct SupervisionState {
    /// Timestamps of recent restart attempts, oldest first. Older than
    /// [`DEFAULT_RESTART_WINDOW`] entries are pruned on every observation.
    restart_attempts: Vec<Instant>,
    /// Consecutive failures since the last success. Reset by a successful
    /// `tools/call`.
    consecutive_failures: u32,
    /// When `Some`, the breaker is open until this instant. While open,
    /// `call` returns `circuit_open` without touching the network. After
    /// the instant elapses, the next call probes (half-open); success
    /// closes the breaker and resets the failure count.
    breaker_opens_until: Option<Instant>,
    /// When `true`, the server has burned through its restart budget and
    /// will not be auto-restarted again until `reload(server)` is invoked.
    ejected: bool,
    /// Per-server breaker threshold (overridable via `spawn()` options).
    circuit_threshold: u32,
    /// Per-server breaker reset window (overridable via `spawn()` options).
    circuit_reset: Duration,
    /// Per-server restart budget (overridable via `spawn()` options).
    max_restarts: u32,
    /// Per-server restart window (overridable via `spawn()` options).
    restart_window: Duration,
}

impl SupervisionState {
    fn new(policy: SupervisionPolicy) -> Self {
        Self {
            restart_attempts: Vec::new(),
            consecutive_failures: 0,
            breaker_opens_until: None,
            ejected: false,
            circuit_threshold: policy.circuit_threshold,
            circuit_reset: policy.circuit_reset,
            max_restarts: policy.max_restarts,
            restart_window: policy.restart_window,
        }
    }

    /// Returns the breaker state at `now`. Transitions Open→HalfOpen
    /// implicitly when the reset window has elapsed; the caller is
    /// expected to close (or re-open) the breaker on success/failure.
    fn breaker_state(&mut self, now: Instant) -> BreakerState {
        match self.breaker_opens_until {
            Some(deadline) if now < deadline => BreakerState::Open,
            Some(_) => BreakerState::HalfOpen,
            None => BreakerState::Closed,
        }
    }

    /// Record a successful call. Closes the breaker and resets the
    /// failure counter regardless of prior state.
    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.breaker_opens_until = None;
    }

    /// Record a failure. Opens the breaker once consecutive failures hit
    /// the threshold; subsequent failures stretch the open-window so
    /// flapping doesn't get free passes.
    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.circuit_threshold {
            self.breaker_opens_until = Some(now + self.circuit_reset);
        }
    }

    /// Record a restart attempt and report whether the server is still
    /// within its budget. Returns `false` when the budget is exhausted —
    /// the caller should mark the server ejected and surface a fatal
    /// error.
    fn record_restart(&mut self, now: Instant) -> bool {
        self.prune_restart_window(now);
        self.restart_attempts.push(now);
        if self.restart_attempts.len() as u32 > self.max_restarts {
            self.ejected = true;
            return false;
        }
        true
    }

    /// Next backoff delay before retrying. Exponential, capped at
    /// [`MAX_RESTART_BACKOFF`].
    fn backoff_delay(&self) -> Duration {
        let attempt = self.restart_attempts.len() as u32;
        let exp = attempt.saturating_sub(1).min(6);
        let mul = 1u64 << exp;
        let nanos = INITIAL_RESTART_BACKOFF.as_nanos() as u64 * mul;
        Duration::from_nanos(nanos).min(MAX_RESTART_BACKOFF)
    }

    fn prune_restart_window(&mut self, now: Instant) {
        let window = self.restart_window;
        self.restart_attempts
            .retain(|t| now.duration_since(*t) <= window);
    }

    fn clear(&mut self) {
        self.restart_attempts.clear();
        self.consecutive_failures = 0;
        self.breaker_opens_until = None;
        self.ejected = false;
    }
}

/// Public reading of the circuit breaker state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerState {
    pub fn as_str(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
}

/// Per-server supervision policy. Each field maps to a knob exposed via
/// `harn.mcp.spawn(..., options)`. Defaults match the constants at the
/// top of this module.
#[derive(Clone, Copy, Debug)]
pub struct SupervisionPolicy {
    pub circuit_threshold: u32,
    pub circuit_reset: Duration,
    pub max_restarts: u32,
    pub restart_window: Duration,
}

impl Default for SupervisionPolicy {
    fn default() -> Self {
        Self {
            circuit_threshold: DEFAULT_CIRCUIT_THRESHOLD,
            circuit_reset: DEFAULT_CIRCUIT_RESET,
            max_restarts: DEFAULT_MAX_RESTARTS,
            restart_window: DEFAULT_RESTART_WINDOW,
        }
    }
}

/// One cached response keyed by (server, tool, args_hash). `expires_at`
/// is derived from the MCP cache hint that came back with the original
/// result; entries without a positive TTL are never inserted.
#[derive(Clone, Debug)]
struct CachedResponse {
    payload: JsonValue,
    inserted_at: Instant,
    expires_at: Instant,
    /// `scope` echoes the MCP `cacheScope` field. The cache is
    /// process-local regardless, so today's logic ignores it — but
    /// `status()` surfaces it via `cache_entries` and a future
    /// per-tenant cache partition (A.2) will key on it.
    #[allow(dead_code)]
    scope: Option<&'static str>,
}

/// Decision returned by an [`AllowlistGuard`] before dispatching a call.
/// `Allow` proceeds; `Deny` short-circuits with a typed error built from
/// the supplied reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllowlistDecision {
    Allow,
    Deny { reason: String },
}

/// Pluggable policy hook so harn-serve can wire `AuthPolicy` (per-tenant
/// allowlists) without harn-vm depending on harn-serve. The closure is
/// invoked once per `spawn` / `call`, before any network traffic, with
/// the server name and optional tool name (`None` for spawn checks).
pub type AllowlistGuard = Arc<dyn Fn(&str, Option<&str>) -> AllowlistDecision + Send + Sync>;

/// Diagnostic snapshot of a hosted server. Returned by
/// `harn.mcp.status()` and consumed by tests and dashboards.
#[derive(Clone, Debug, Serialize)]
pub struct McpHostStatus {
    pub name: String,
    pub transport: String,
    pub url: Option<String>,
    pub active: bool,
    pub lazy: bool,
    pub ref_count: usize,
    pub restart_count: u32,
    pub consecutive_failures: u32,
    pub circuit: BreakerState,
    pub ejected: bool,
    /// Number of cached response entries for this server (across all
    /// tools).
    pub cache_entries: usize,
    /// Human-readable authenticated identity for connected OAuth-backed HTTP
    /// servers when a vetted identity descriptor can render from the stored
    /// token response.
    pub display_identity: Option<String>,
}

/// Options accepted by [`spawn`]. Mirrors the dict surface in
/// `harn.mcp.spawn({...}, options)`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SpawnOptions {
    /// When `true`, register the server but do not connect eagerly.
    /// Defaults to `false` — `spawn` always boots so callers get a
    /// fail-fast error if the spec is broken.
    #[serde(default)]
    pub lazy: bool,
    /// Keep-alive grace period (milliseconds) before a fully-released
    /// lazy connection is closed. `None` → close immediately.
    #[serde(default)]
    pub keep_alive_ms: Option<u64>,
    /// Optional path or URL to a server card to associate with the
    /// registration so `mcp_server_card(name)` can resolve it later.
    #[serde(default)]
    pub card: Option<String>,
    /// Supervision policy overrides. Unset fields fall back to the
    /// module-level constants.
    #[serde(default)]
    pub circuit_threshold: Option<u32>,
    #[serde(default)]
    pub circuit_reset_ms: Option<u64>,
    #[serde(default)]
    pub max_restarts: Option<u32>,
    #[serde(default)]
    pub restart_window_ms: Option<u64>,
}

impl SpawnOptions {
    fn into_policy(self) -> (SupervisionPolicy, RegisteredMcpServerMeta) {
        let default = SupervisionPolicy::default();
        let policy = SupervisionPolicy {
            circuit_threshold: self.circuit_threshold.unwrap_or(default.circuit_threshold),
            circuit_reset: self
                .circuit_reset_ms
                .map(Duration::from_millis)
                .unwrap_or(default.circuit_reset),
            max_restarts: self.max_restarts.unwrap_or(default.max_restarts),
            restart_window: self
                .restart_window_ms
                .map(Duration::from_millis)
                .unwrap_or(default.restart_window),
        };
        let meta = RegisteredMcpServerMeta {
            lazy: self.lazy,
            keep_alive: self.keep_alive_ms.map(Duration::from_millis),
            card: self.card,
        };
        (policy, meta)
    }
}

struct RegisteredMcpServerMeta {
    lazy: bool,
    keep_alive: Option<Duration>,
    card: Option<String>,
}

/// Bag of mutable state owned by the process-global host. One per
/// process; tests reset via [`reset_for_tests`].
struct HostInner {
    /// Per-server supervision state. Keys mirror
    /// `mcp_registry::REGISTRY.servers`. Entries are inserted at
    /// `spawn` and pruned at `stop`/`reload`.
    supervision: HashMap<String, SupervisionState>,
    /// Per-(server, tool) → args_hash → cached response. The args-hash
    /// keying lets two distinct argument shapes coexist for the same
    /// tool without collisions.
    response_cache: HashMap<(String, String), HashMap<String, CachedResponse>>,
    /// Active allowlist guard, if any. `None` means allow-all.
    allowlist: Option<AllowlistGuard>,
    /// Cache statistics. Surfaced via `status()`'s `cache_entries`
    /// counter and the standalone `cache_stats()` helper for telemetry.
    cache_hits: u64,
    cache_misses: u64,
}

impl HostInner {
    fn new() -> Self {
        Self {
            supervision: HashMap::new(),
            response_cache: HashMap::new(),
            allowlist: None,
            cache_hits: 0,
            cache_misses: 0,
        }
    }
}

static HOST: Mutex<Option<HostInner>> = Mutex::new(None);

fn with_inner<F, R>(f: F) -> R
where
    F: FnOnce(&mut HostInner) -> R,
{
    let mut guard = HOST.lock().expect("mcp host mutex poisoned");
    if guard.is_none() {
        *guard = Some(HostInner::new());
    }
    f(guard.as_mut().expect("host inner just initialized"))
}

/// Install (or replace) the allowlist guard. Pass `None` to clear.
pub fn set_allowlist(guard: Option<AllowlistGuard>) {
    with_inner(|inner| inner.allowlist = guard);
}

/// Wipe every host-side bit of state. Called by `reset_thread_local_state`
/// and by tests that need a clean slate between runs.
pub fn reset_for_tests() {
    with_inner(|inner| {
        inner.supervision.clear();
        inner.response_cache.clear();
        inner.allowlist = None;
        inner.cache_hits = 0;
        inner.cache_misses = 0;
    });
    mcp_registry::reset();
}

/// Cache hit/miss counters. Tests use these to assert that caching is
/// actually engaged; the agent loop's observability layer reads them to
/// emit `harn.mcp.cache.*` metrics.
#[derive(Clone, Copy, Debug)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

pub fn cache_stats() -> CacheStats {
    with_inner(|inner| CacheStats {
        hits: inner.cache_hits,
        misses: inner.cache_misses,
    })
}

/// Spawn (register + connect) an MCP server. Returns the server name as
/// the `server_id` — names are unique within the registry and stable
/// across the process lifetime, so we don't need a separate opaque id.
pub async fn spawn(spec: JsonValue, options: SpawnOptions) -> Result<String, VmError> {
    let name = spec
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| VmError::Runtime("mcp.spawn: spec must include a `name` field".into()))?
        .to_string();
    if name.is_empty() {
        return Err(VmError::Runtime(
            "mcp.spawn: spec.name must be a non-empty string".into(),
        ));
    }

    if let Some(guard) = current_allowlist() {
        if let AllowlistDecision::Deny { reason } = guard(&name, None) {
            return Err(VmError::Runtime(format!(
                "mcp.spawn({name}): denied by allowlist: {reason}"
            )));
        }
    }

    let (policy, meta) = options.into_policy();
    mcp_registry::register_servers(vec![RegisteredMcpServer {
        name: name.clone(),
        spec: spec.clone(),
        lazy: meta.lazy,
        card: meta.card,
        keep_alive: meta.keep_alive,
    }]);

    with_inner(|inner| {
        inner
            .supervision
            .insert(name.clone(), SupervisionState::new(policy));
    });

    if !meta.lazy {
        // Eager spawn — connect right away so a broken spec fails before
        // the first call. Lazy servers stay idle until first use.
        let _ = mcp_registry::ensure_active(&name).await.inspect_err(|_| {
            with_inner(|inner| {
                inner.supervision.remove(&name);
            });
        })?;
    }

    Ok(name)
}

/// Drop a hosted server: tears down the active connection, prunes
/// supervision + cache entries. The registration itself is left in place
/// so a subsequent `ensure_active` from declarative `harn.toml` flows
/// still works; callers that want to fully forget a server should also
/// re-register a new spec via `spawn`.
pub fn stop(name: &str) -> Result<(), VmError> {
    if !mcp_registry::is_registered(name) {
        return Err(VmError::Runtime(format!(
            "mcp.stop: no server named '{name}' is hosted"
        )));
    }
    mcp_registry::release(name);
    with_inner(|inner| {
        inner.supervision.remove(name);
        inner.response_cache.retain(|(s, _), _| s != name);
    });
    Ok(())
}

/// Hot-reload a hosted server: drops the active connection but preserves
/// the registration spec. The next `call` reconnects automatically.
/// Reset supervision state so a previously-ejected server gets a fresh
/// budget after the operator fixes the underlying problem.
pub fn reload(name: &str) -> Result<(), VmError> {
    if !mcp_registry::is_registered(name) {
        return Err(VmError::Runtime(format!(
            "mcp.reload: no server named '{name}' is hosted"
        )));
    }
    mcp_registry::release(name);
    with_inner(|inner| {
        if let Some(state) = inner.supervision.get_mut(name) {
            state.clear();
        }
        inner.response_cache.retain(|(s, _), _| s != name);
    });
    Ok(())
}

/// Return the cached tool list for a server, performing a fresh
/// `tools/list` only when the registered cache hint has expired. The
/// returned tools are annotated with `_mcp_server` so downstream
/// indexers (BM25 search, etc.) can match by server.
pub async fn tools(name: &str) -> Result<Vec<JsonValue>, VmError> {
    let handle = ensure_or_restart(name).await?;
    let result = supervised_call(name, || async {
        handle.call("tools/list", serde_json::json!({})).await
    })
    .await?;

    let mut tools = result
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    for tool in tools.iter_mut() {
        if let Some(obj) = tool.as_object_mut() {
            obj.entry("_mcp_server")
                .or_insert_with(|| JsonValue::String(name.to_string()));
        }
    }
    // MCP tool-integrity (Layer 0b): pin each tool's schema and flag any whose
    // description/inputSchema changed since first sighting (rug-pull defense).
    // The flag rides on the tool dict so the host's approval UI can force
    // re-approval; harn surfaces the fact, the host decides.
    let security_policy = crate::security::current_policy();
    if security_policy.pin_mcp_schemas && !security_policy.server_is_trusted(name) {
        for tool in tools.iter_mut() {
            let hash = crate::security::tool_schema_hash(tool);
            let tool_name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if tool_name.is_empty() {
                continue;
            }
            if crate::security::pin_and_detect_change(name, &tool_name, &hash) {
                if let Some(obj) = tool.as_object_mut() {
                    obj.insert("_schema_changed".to_string(), JsonValue::Bool(true));
                }
            }
        }
    }
    Ok(tools)
}

/// Invoke a tool with the supervised wrapper applied. Honors:
/// - allowlist (refuses unallowed (server, tool) pairs),
/// - circuit breaker (fails fast when open),
/// - response cache (returns cached payload when the server-declared
///   TTL has not expired and the args hash matches),
/// - auto-restart on transport failure with exponential backoff.
pub async fn call(name: &str, tool: &str, args: JsonValue) -> Result<JsonValue, VmError> {
    if let Some(guard) = current_allowlist() {
        if let AllowlistDecision::Deny { reason } = guard(name, Some(tool)) {
            return Err(VmError::Runtime(format!(
                "mcp.call({name}/{tool}): denied by allowlist: {reason}"
            )));
        }
    }

    // Charge the logical call against any active `@budget(mcp_calls: …)`
    // ceiling before doing work — including cache hits, since the budget
    // caps how many tool calls a dispatch may *issue*, not how many miss
    // the cache. Caps a runaway tool loop at the dispatcher boundary.
    crate::call_budget::charge_mcp_call()?;

    let now = Instant::now();
    let args_hash = hash_args(&args);
    if let Some(payload) = take_cache_hit(name, tool, &args_hash, now) {
        return Ok(payload);
    }
    with_inner(|inner| inner.cache_misses = inner.cache_misses.saturating_add(1));

    breaker_gate(name, now)?;

    let handle = ensure_or_restart(name).await?;
    // `supervised_call` operates on a single `JsonValue` payload so it
    // can be shared with the `tools()` path. Stash the envelope's cache
    // hint in a separate slot the closure can update without breaking
    // that contract.
    let envelope_hint: Arc<Mutex<Option<McpCacheHint>>> = Arc::new(Mutex::new(None));
    let hint_slot = Arc::clone(&envelope_hint);
    let result = supervised_call(name, move || {
        let handle = handle.clone();
        let tool = tool.to_string();
        let args = args.clone();
        let hint_slot = Arc::clone(&hint_slot);
        async move {
            let (content, hint) = call_mcp_tool_with_hint(&handle, &tool, args).await?;
            if let Ok(mut slot) = hint_slot.lock() {
                *slot = hint;
            }
            Ok(content)
        }
    })
    .await?;

    let hint = envelope_hint.lock().ok().and_then(|slot| *slot);
    if let Some(hint) = hint {
        insert_cache(name, tool, &args_hash, &result, hint, now);
    }

    Ok(result)
}

/// Cross-server tool discovery — calls `tools/list` against every
/// registered (and reachable) server and returns a flat list of
/// `{ server, tool, schema }` entries.
pub async fn discover() -> Result<Vec<JsonValue>, VmError> {
    let names: Vec<String> = mcp_registry::snapshot_status()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let mut out: Vec<JsonValue> = Vec::new();
    for name in names {
        // Skip servers that the allowlist (if any) wouldn't even let us
        // spawn — `discover()` is a tooling primitive, not a probe.
        if let Some(guard) = current_allowlist() {
            if matches!(guard(&name, None), AllowlistDecision::Deny { .. }) {
                continue;
            }
        }
        // Best-effort: a single unreachable server should not poison the
        // whole discovery sweep. Surface the error inline so callers can
        // tell why a server's tools are missing.
        match tools(&name).await {
            Ok(tools) => {
                for tool in tools {
                    let tool_name = tool
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(serde_json::json!({
                        "server": name,
                        "tool": tool_name,
                        "schema": tool,
                    }));
                }
            }
            Err(err) => {
                out.push(serde_json::json!({
                    "server": name,
                    "error": err.to_string(),
                }));
            }
        }
    }
    Ok(out)
}

/// Diagnostic snapshot across all hosted servers.
pub async fn status() -> Vec<McpHostStatus> {
    let registry: BTreeMap<String, mcp_registry::RegistryStatus> = mcp_registry::snapshot_status()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let mut statuses = with_inner(|inner| {
        let mut out = Vec::new();
        let now = Instant::now();
        for (name, reg) in &registry {
            let (restart_count, consecutive_failures, circuit, ejected) =
                if let Some(state) = inner.supervision.get_mut(name) {
                    let st = state.breaker_state(now);
                    (
                        state.restart_attempts.len() as u32,
                        state.consecutive_failures,
                        st,
                        state.ejected,
                    )
                } else {
                    (0, 0, BreakerState::Closed, false)
                };
            let cache_entries = inner
                .response_cache
                .iter()
                .filter(|((s, _), _)| s == name)
                .map(|(_, v)| v.len())
                .sum();
            out.push(McpHostStatus {
                name: name.clone(),
                transport: reg.transport.clone(),
                url: reg.url.clone(),
                active: reg.active,
                lazy: reg.lazy,
                ref_count: reg.ref_count,
                restart_count,
                consecutive_failures,
                circuit,
                ejected,
                cache_entries,
                display_identity: None,
            });
        }
        out
    });
    for status in &mut statuses {
        if !status.active || status.transport != "http" {
            continue;
        }
        let Some(url) = status.url.as_deref() else {
            continue;
        };
        status.display_identity = crate::mcp_identity::display_identity_from_store(url, None).await;
    }
    statuses
}

fn current_allowlist() -> Option<AllowlistGuard> {
    with_inner(|inner| inner.allowlist.clone())
}

fn breaker_gate(name: &str, now: Instant) -> Result<(), VmError> {
    with_inner(|inner| {
        let Some(state) = inner.supervision.get_mut(name) else {
            return Ok(());
        };
        if state.ejected {
            return Err(VmError::Runtime(format!(
                "mcp.call({name}): server is ejected after exhausting its restart budget; call `harn.mcp.reload({name:?})` to clear"
            )));
        }
        match state.breaker_state(now) {
            BreakerState::Open => Err(VmError::Runtime(format!(
                "mcp.call({name}): circuit breaker is open (last {n} consecutive failures); retry after the breaker resets",
                n = state.consecutive_failures
            ))),
            // Closed and HalfOpen both proceed — HalfOpen lets one probe
            // through and the success path closes the breaker.
            BreakerState::Closed | BreakerState::HalfOpen => Ok(()),
        }
    })
}

async fn ensure_or_restart(name: &str) -> Result<VmMcpClientHandle, VmError> {
    // Fast path: the registry already has a live handle.
    if let Some(handle) = mcp_registry::active_handle(name) {
        return Ok(handle);
    }

    // Cold path: the server is registered but the connection is gone
    // (lazy boot, crashed transport, or `reload()` dropped it). Try to
    // reconnect through the registry; budget enforcement happens
    // inside `supervised_call`'s error path on the next failure, not
    // here.
    mcp_registry::ensure_active(name).await
}

/// Run `op` against the hosted server and wrap any error in supervision
/// bookkeeping: record the failure, attempt an automatic restart if the
/// budget allows, and try `op` again once. A second failure surfaces.
async fn supervised_call<F, Fut>(name: &str, op: F) -> Result<JsonValue, VmError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<JsonValue, VmError>>,
{
    let span = tracing::info_span!(
        "harn.mcp.call",
        otel.name = "harn.mcp.call",
        harn.mcp.server = name,
    );
    let _enter = span.enter();

    let first = op().await;
    match first {
        Ok(v) => {
            with_inner(|inner| {
                if let Some(state) = inner.supervision.get_mut(name) {
                    state.record_success();
                }
            });
            Ok(v)
        }
        Err(err) => {
            let now = Instant::now();
            let (should_retry, backoff) = with_inner(|inner| {
                let Some(state) = inner.supervision.get_mut(name) else {
                    return (false, Duration::ZERO);
                };
                state.record_failure(now);
                // Only auto-restart on transport-shaped errors. The
                // surface is broad here on purpose — any failure between
                // "could not write to child stdin" and "server closed
                // connection" warrants a fresh transport.
                if !looks_like_transport_failure(&err) {
                    return (false, Duration::ZERO);
                }
                let ok = state.record_restart(now);
                if !ok {
                    return (false, Duration::ZERO);
                }
                (true, state.backoff_delay())
            });
            if !should_retry {
                tracing::warn!(
                    server = name,
                    error = %err,
                    "harn.mcp.call: failure (no retry)"
                );
                return Err(err);
            }

            tracing::info!(
                server = name,
                error = %err,
                backoff_ms = backoff.as_millis() as u64,
                "harn.mcp.call: retrying after transport failure"
            );

            // Force the registry to drop the dead handle so the next
            // `ensure_or_restart` will reconnect from spec.
            mcp_registry::release(name);
            tokio::time::sleep(backoff).await;
            let _handle = ensure_or_restart(name).await?;
            let second = op().await;
            match &second {
                Ok(_) => with_inner(|inner| {
                    if let Some(state) = inner.supervision.get_mut(name) {
                        state.record_success();
                    }
                }),
                Err(err) => with_inner(|inner| {
                    if let Some(state) = inner.supervision.get_mut(name) {
                        state.record_failure(Instant::now());
                    }
                    tracing::warn!(
                        server = name,
                        error = %err,
                        "harn.mcp.call: second attempt failed"
                    );
                }),
            }
            second
        }
    }
}

fn looks_like_transport_failure(err: &VmError) -> bool {
    let text = err.to_string();
    let needles = [
        "server closed connection",
        "disconnected",
        "MCP read error",
        "MCP write error",
        "did not respond to",
        "MCP flush error",
        "connect",
    ];
    needles.iter().any(|n| text.contains(n))
}

fn hash_args(args: &JsonValue) -> String {
    let mut hasher = Sha256::new();
    let canonical = crate::canonical_json::to_string(args);
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn take_cache_hit(server: &str, tool: &str, args_hash: &str, now: Instant) -> Option<JsonValue> {
    with_inner(|inner| {
        let key = (server.to_string(), tool.to_string());
        let entry = inner.response_cache.get_mut(&key)?;
        let cached = entry.get(args_hash)?;
        if now >= cached.expires_at {
            entry.remove(args_hash);
            return None;
        }
        let payload = cached.payload.clone();
        inner.cache_hits = inner.cache_hits.saturating_add(1);
        Some(payload)
    })
}

/// Cache a server-supplied response payload under (`server`, `tool`,
/// `args_hash`) for the TTL the server declared. Insertion is a no-op
/// when the hint has no positive TTL — server implementations that
/// want a result memoized must surface a `ttlMs > 0` in the envelope.
fn insert_cache(
    server: &str,
    tool: &str,
    args_hash: &str,
    payload: &JsonValue,
    hint: McpCacheHint,
    now: Instant,
) {
    let Some(ttl_ms) = hint.ttl_ms else {
        return;
    };
    if ttl_ms == 0 {
        return;
    }
    let expires_at = now + Duration::from_millis(ttl_ms);
    let cached = CachedResponse {
        payload: payload.clone(),
        inserted_at: now,
        expires_at,
        scope: hint.scope,
    };
    with_inner(|inner| {
        let key = (server.to_string(), tool.to_string());
        let bucket = inner.response_cache.entry(key).or_default();
        if bucket.len() >= RESPONSE_CACHE_MAX_ENTRIES_PER_TOOL {
            // Drop the oldest insertion as a cheap LRU approximation.
            if let Some(oldest_key) = bucket
                .iter()
                .min_by_key(|(_, v)| v.inserted_at)
                .map(|(k, _)| k.clone())
            {
                bucket.remove(&oldest_key);
            }
        }
        bucket.insert(args_hash.to_string(), cached);
    });
}

/// Compatibility shim for tests that previously inserted a cached
/// payload from a JSON envelope. Resolves the hint via
/// [`McpCacheHint::from_result`] and delegates to [`insert_cache`].
#[cfg(test)]
fn insert_cache_if_hinted(
    server: &str,
    tool: &str,
    args_hash: &str,
    payload: &JsonValue,
    now: Instant,
) {
    if let Some(hint) = McpCacheHint::from_result(payload) {
        insert_cache(server, tool, args_hash, payload, hint, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn supervision_breaker_opens_after_threshold() {
        let _g = lock();
        let mut state = SupervisionState::new(SupervisionPolicy {
            circuit_threshold: 3,
            circuit_reset: Duration::from_millis(100),
            ..SupervisionPolicy::default()
        });
        let t0 = Instant::now();
        assert_eq!(state.breaker_state(t0), BreakerState::Closed);
        state.record_failure(t0);
        state.record_failure(t0);
        assert_eq!(state.breaker_state(t0), BreakerState::Closed);
        state.record_failure(t0);
        assert_eq!(state.breaker_state(t0), BreakerState::Open);
        // After reset window, breaker transitions to half-open.
        assert_eq!(
            state.breaker_state(t0 + Duration::from_millis(200)),
            BreakerState::HalfOpen
        );
    }

    #[test]
    fn supervision_restart_budget_ejects_after_n_attempts() {
        let _g = lock();
        let mut state = SupervisionState::new(SupervisionPolicy {
            max_restarts: 2,
            restart_window: Duration::from_mins(1),
            ..SupervisionPolicy::default()
        });
        let t = Instant::now();
        assert!(state.record_restart(t));
        assert!(state.record_restart(t));
        assert!(!state.record_restart(t));
        assert!(state.ejected);
    }

    #[test]
    fn supervision_backoff_grows_exponentially_then_caps() {
        let _g = lock();
        let mut state = SupervisionState::new(SupervisionPolicy::default());
        let t = Instant::now();
        state.record_restart(t);
        let d1 = state.backoff_delay();
        state.record_restart(t);
        let d2 = state.backoff_delay();
        state.record_restart(t);
        let d3 = state.backoff_delay();
        assert!(
            d2 > d1,
            "second backoff ({d2:?}) should exceed first ({d1:?})"
        );
        assert!(d3 > d2);
        for _ in 0..16 {
            state.record_restart(t);
        }
        assert!(state.backoff_delay() <= MAX_RESTART_BACKOFF);
    }

    #[test]
    fn hash_args_is_stable_across_key_order() {
        let h1 = hash_args(&serde_json::json!({"x": 1, "y": [1, 2]}));
        let h2 = hash_args(&serde_json::json!({"y": [1, 2], "x": 1}));
        assert_eq!(h1, h2);
    }

    #[test]
    fn cache_insert_and_take_respects_ttl() {
        let _g = lock();
        reset_for_tests();
        let payload = serde_json::json!({
            "ttlMs": 100,
            "cacheScope": "private",
            "value": 1
        });
        let now = Instant::now();
        insert_cache_if_hinted("srv", "ping", "deadbeef", &payload, now);
        let hit = take_cache_hit("srv", "ping", "deadbeef", now);
        assert!(hit.is_some(), "fresh entry should hit");
        let stale = take_cache_hit("srv", "ping", "deadbeef", now + Duration::from_millis(200));
        assert!(stale.is_none(), "expired entry should miss");
    }

    #[test]
    fn cache_skips_payload_without_hint() {
        let _g = lock();
        reset_for_tests();
        insert_cache_if_hinted(
            "srv",
            "ping",
            "h",
            &serde_json::json!({"value": 1}),
            Instant::now(),
        );
        assert!(take_cache_hit("srv", "ping", "h", Instant::now()).is_none());
    }

    #[test]
    fn allowlist_denies_disallowed_tool() {
        let _g = lock();
        reset_for_tests();
        set_allowlist(Some(Arc::new(|server, tool| {
            if server == "github" && tool == Some("delete_repo") {
                AllowlistDecision::Deny {
                    reason: "destructive tool blocked".into(),
                }
            } else {
                AllowlistDecision::Allow
            }
        })));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime
            .block_on(call("github", "delete_repo", serde_json::json!({})))
            .unwrap_err();
        assert!(err.to_string().contains("denied by allowlist"));
        set_allowlist(None);
    }

    #[test]
    fn stop_unregistered_server_errors() {
        let _g = lock();
        reset_for_tests();
        let err = stop("nope").unwrap_err();
        assert!(err.to_string().contains("no server named 'nope'"));
    }

    #[test]
    fn supervision_record_success_resets_counters() {
        let _g = lock();
        let mut state = SupervisionState::new(SupervisionPolicy::default());
        let t = Instant::now();
        state.record_failure(t);
        state.record_failure(t);
        state.record_success();
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.breaker_opens_until.is_none());
    }

    #[test]
    fn looks_like_transport_failure_matches_common_errors() {
        let cases = [
            "MCP: server closed connection",
            "MCP: server did not respond to 'tools/call' within 60s",
            "MCP write error: broken pipe",
            "MCP client is disconnected",
        ];
        for msg in cases {
            assert!(
                looks_like_transport_failure(&VmError::Runtime(msg.into())),
                "expected {msg:?} to be classified as transport failure"
            );
        }
        assert!(
            !looks_like_transport_failure(&VmError::Runtime(
                "tool 'foo' rejected arguments".into()
            )),
            "tool-level errors must not trigger an auto-restart"
        );
    }
}
