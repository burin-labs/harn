//! Rate-limiting + backpressure + call-budget primitive for `.harn`
//! handlers hosted on `harn-serve`. Implements epic A.11 — the
//! cross-tenant production-readiness shape a cloud gateway enforces
//! today via bespoke middleware (`http_utils::check_rate_limits`), lifted
//! into one declarative primitive every adapter on `harn-serve` shares.
//!
//! ## Author-facing shape
//!
//! ```harn
//! @limits(
//!     per_tenant: "100/min",      //  rate · window
//!     per_scope:  "1000/min",
//!     per_route:  "5000/min",
//!     burst:      50,             //  capacity above sustained rate
//!     algorithm:  "token_bucket", //  token_bucket | sliding_window | leaky_bucket
//!     in_flight_max: 20,          //  backpressure watermark
//! )
//! @budget(
//!     llm_cost_usd: 0.50,         //  pinned to harn-vm's LLM_BUDGET on dispatch
//!     llm_tokens:   10000,
//!     pg_queries:   50,
//!     mcp_calls:    20,
//! )
//! pub fn create_session(req: dict) -> dict { ... }
//! ```
//!
//! ## Decision flow (one dispatch)
//!
//! 1. `LimitRegistry::check` consults each declared bucket
//!    (`per_route` → `per_tenant` → `per_scope`) and the backpressure
//!    watermark; first violator wins, smallest `retry_after_ms` is
//!    returned.
//! 2. Allowed dispatches hold a `LimitGuard` that decrements the
//!    in-flight counter on drop.
//! 3. Rejected dispatches surface as
//!    [`crate::DispatchError::RateLimited`] → HTTP 429 with `Retry-After`
//!    via [`crate::http_codec`].
//! 4. Budget caps install on the dispatch's thread-local via
//!    `harn-vm` guards ([`harn_vm::install_llm_cost_budget`],
//!    [`harn_vm::install_llm_token_budget`],
//!    [`harn_vm::install_mcp_call_budget`],
//!    [`harn_vm::install_pg_query_budget`]). Mid-call exhaustion raises a
//!    `BudgetExceeded`-categorised runtime error which the codec maps to
//!    429 with `code = "budget_exceeded"`. The `details.limit` field
//!    names the dimension that fired using the same identifier as the
//!    `@budget(...)` argument — one of `llm_cost_usd`, `llm_tokens`,
//!    `mcp_calls`, or `pg_queries` — so clients can attribute the
//!    rejection without parsing the message.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harn_clock::Clock;

use harn_vm::TenantId;

pub mod algorithms;
pub mod parse;
pub mod store;

pub use algorithms::{Algorithm, CellDecision, RateAlgorithm};
pub use parse::limits_and_budget_from_attributes;
pub use store::{BucketSpec, InMemoryLimitStore, LimitStore};

/// Parsed `N/(sec|min|hour)` quota. The window is implicit in `per` —
/// the registry collapses everything to per-second rates internally for
/// the algorithm cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    pub count: u32,
    pub per: QuotaWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaWindow {
    Second,
    Minute,
    Hour,
}

impl QuotaWindow {
    fn seconds(self) -> f64 {
        match self {
            Self::Second => 1.0,
            Self::Minute => 60.0,
            Self::Hour => 3_600.0,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "s" | "sec" | "second" | "seconds" => Some(Self::Second),
            "m" | "min" | "minute" | "minutes" => Some(Self::Minute),
            "h" | "hour" | "hours" => Some(Self::Hour),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Second => "sec",
            Self::Minute => "min",
            Self::Hour => "hour",
        }
    }
}

impl Quota {
    pub fn new(count: u32, per: QuotaWindow) -> Self {
        Self { count, per }
    }

    /// Parse a `"N/window"` string into a quota. Returns `None` for any
    /// shape the rest of the primitive cannot enforce — callers should
    /// pass the original string through to the parser as `Option<String>`
    /// and silently drop unrecognised forms (the attribute parser
    /// already validates that the source token was a string literal).
    pub fn parse(raw: impl AsRef<str>) -> Option<Self> {
        let raw = raw.as_ref();
        let (count_part, window_part) = raw.split_once('/')?;
        let count: u32 = count_part
            .trim()
            .replace('_', "")
            .parse()
            .ok()
            .filter(|n| *n > 0)?;
        let per = QuotaWindow::parse(window_part)?;
        Some(Self { count, per })
    }

    /// Per-second rate this quota sustains. Burst is layered on top via
    /// [`RouteLimits::burst`].
    pub fn rate_per_sec(self) -> f64 {
        self.count as f64 / self.per.seconds()
    }
}

impl std::fmt::Display for Quota {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.count, self.per.as_str())
    }
}

/// Limits declared on one route mount. All fields are independent: a
/// route can declare any combination (or none — `RouteLimits::default()`
/// is unlimited and the registry short-circuits cheaply).
#[derive(Clone, Debug, Default)]
pub struct RouteLimits {
    pub per_tenant: Option<Quota>,
    pub per_scope: Option<Quota>,
    pub per_route: Option<Quota>,
    /// Capacity above sustained rate — token bucket / leaky bucket
    /// burst tolerance. When unset, the bucket capacity equals the
    /// per-window count (no burst above the steady-state quota).
    pub burst: Option<u32>,
    pub algorithm: Algorithm,
    /// Maximum concurrent in-flight dispatches on this route before
    /// backpressure rejects. The registry tracks an in-flight counter
    /// per route key and returns 429 + Retry-After when it would
    /// exceed this watermark. `None` = unbounded.
    pub in_flight_max: Option<u32>,
}

impl RouteLimits {
    pub fn is_unlimited(&self) -> bool {
        self.per_tenant.is_none()
            && self.per_scope.is_none()
            && self.per_route.is_none()
            && self.in_flight_max.is_none()
    }

    fn bucket_for(&self, quota: Quota) -> BucketSpec {
        let capacity = self.burst.unwrap_or(quota.count).max(1);
        BucketSpec::new(self.algorithm, quota.rate_per_sec(), capacity)
    }
}

/// Per-call resource budget declared on the route. Enforced inside the
/// dispatch (mid-call) so a runaway tool loop hits the ceiling before
/// it can melt the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BudgetSpec {
    pub llm_cost_usd: Option<f64>,
    pub llm_tokens: Option<u64>,
    pub pg_queries: Option<u64>,
    pub mcp_calls: Option<u64>,
}

impl BudgetSpec {
    pub fn is_empty(&self) -> bool {
        self.llm_cost_usd.is_none()
            && self.llm_tokens.is_none()
            && self.pg_queries.is_none()
            && self.mcp_calls.is_none()
    }

    /// Install the resource ceilings represented by this budget on the
    /// current thread. The returned guard restores all prior ceilings when
    /// dropped, so nested dispatches can temporarily override budget state.
    pub(crate) fn install(&self) -> Option<BudgetGuard> {
        if self.is_empty() {
            return None;
        }
        Some(BudgetGuard {
            _llm_cost: self.llm_cost_usd.map(harn_vm::install_llm_cost_budget),
            _llm_tokens: self.llm_tokens.map(harn_vm::install_llm_token_budget),
            _mcp_calls: self.mcp_calls.map(harn_vm::install_mcp_call_budget),
            _pg_queries: self.pg_queries.map(harn_vm::install_pg_query_budget),
        })
    }
}

/// Aggregate of per-cap guards held for the lifetime of one dispatch.
/// Dropping the aggregate restores every cap simultaneously, keeping nested
/// dispatches safe even when guards land in different thread-locals.
pub(crate) struct BudgetGuard {
    _llm_cost: Option<harn_vm::LlmBudgetGuard>,
    _llm_tokens: Option<harn_vm::LlmTokenBudgetGuard>,
    _mcp_calls: Option<harn_vm::McpCallBudgetGuard>,
    _pg_queries: Option<harn_vm::PgQueryBudgetGuard>,
}

/// Context the registry needs to evaluate a route's limits for one
/// dispatch — names the route, the authenticated principal's scopes,
/// and (when present) the tenant.
#[derive(Clone, Debug)]
pub struct LimitContext<'a> {
    pub route: &'a str,
    pub tenant_id: Option<&'a TenantId>,
    pub scopes: &'a std::collections::BTreeSet<String>,
}

/// The bucket dimension that rejected a dispatch. Surfaces via 429
/// detail body so callers can tell whether they hit a tenant quota
/// vs. a global route ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitScope {
    Route,
    Tenant,
    Scope,
    Backpressure,
}

impl LimitScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Tenant => "tenant",
            Self::Scope => "scope",
            Self::Backpressure => "backpressure",
        }
    }
}

/// One trip through the registry — either admitted (and holding the
/// in-flight slot), or rejected with the offending dimension and the
/// smallest `retry_after_ms` from the violators.
#[derive(Debug)]
pub enum LimitDecision {
    Allowed(LimitGuard),
    Rejected {
        scope: LimitScope,
        retry_after_ms: u64,
    },
}

/// RAII guard returned by [`LimitRegistry::check`] when a dispatch is
/// admitted. Decrements the backpressure counter on drop so the in-flight
/// gauge stays balanced across panics/errors.
#[derive(Debug)]
pub struct LimitGuard {
    counter: Option<Arc<AtomicUsize>>,
}

impl LimitGuard {
    /// Build a guard that decrements `counter` on drop, assuming the
    /// caller has *already* incremented it. Pairing the increment with
    /// the guard constructor introduces a race window between the
    /// "would this exceed the watermark?" check and the increment; the
    /// registry instead does an atomic `fetch_add` and rolls back when
    /// the prior value exceeded the cap, then hands the guard the
    /// already-incremented counter via this constructor.
    fn for_incremented(counter: Arc<AtomicUsize>) -> Self {
        Self {
            counter: Some(counter),
        }
    }

    fn unbounded() -> Self {
        Self { counter: None }
    }

    /// Construct a no-op guard for callers that bypass the registry
    /// (e.g. dispatch paths with no `LimitRegistry` configured at
    /// all). Drop is a no-op.
    pub(crate) fn unbounded_for_caller() -> Self {
        Self::unbounded()
    }
}

impl Drop for LimitGuard {
    fn drop(&mut self) {
        if let Some(counter) = self.counter.take() {
            counter.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Aggregated rejection stats. Adapters surface these via the
/// observability primitive (A.10); the in-memory snapshot is enough
/// for tests and a `/metrics` endpoint.
#[derive(Clone, Debug, Default)]
pub struct LimitStats {
    pub admitted: u64,
    pub rejected_route: u64,
    pub rejected_tenant: u64,
    pub rejected_scope: u64,
    pub rejected_backpressure: u64,
}

impl LimitStats {
    pub fn total_rejected(&self) -> u64 {
        self.rejected_route
            + self.rejected_tenant
            + self.rejected_scope
            + self.rejected_backpressure
    }
}

#[derive(Debug, Default)]
struct AtomicLimitStats {
    admitted: AtomicU64,
    rejected_route: AtomicU64,
    rejected_tenant: AtomicU64,
    rejected_scope: AtomicU64,
    rejected_backpressure: AtomicU64,
}

impl AtomicLimitStats {
    fn snapshot(&self) -> LimitStats {
        LimitStats {
            admitted: self.admitted.load(Ordering::Relaxed),
            rejected_route: self.rejected_route.load(Ordering::Relaxed),
            rejected_tenant: self.rejected_tenant.load(Ordering::Relaxed),
            rejected_scope: self.rejected_scope.load(Ordering::Relaxed),
            rejected_backpressure: self.rejected_backpressure.load(Ordering::Relaxed),
        }
    }
}

/// Per-tenant override applied to every declared quota for that tenant.
/// `1.0` is a no-op; `2.0` doubles every bucket; `0.5` halves them.
/// Capacity is rounded up so a bucket never collapses to 0.
#[derive(Clone, Copy, Debug)]
pub struct TenantOverride {
    pub multiplier: f64,
}

impl Default for TenantOverride {
    fn default() -> Self {
        Self { multiplier: 1.0 }
    }
}

/// Central rate-limit + backpressure orchestrator. Owned by the
/// dispatch core; one instance per `harn-serve` host process.
pub struct LimitRegistry {
    store: Arc<dyn LimitStore>,
    in_flight: Mutex<BTreeMap<String, Arc<AtomicUsize>>>,
    tenant_overrides: Mutex<BTreeMap<TenantId, TenantOverride>>,
    stats: AtomicLimitStats,
}

impl std::fmt::Debug for LimitRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LimitRegistry")
            .field("stats", &self.stats.snapshot())
            .finish()
    }
}

impl LimitRegistry {
    pub fn new(store: Arc<dyn LimitStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            in_flight: Mutex::new(BTreeMap::new()),
            tenant_overrides: Mutex::new(BTreeMap::new()),
            stats: AtomicLimitStats::default(),
        })
    }

    /// In-memory registry — single-node default. Pair with a
    /// production [`Clock`] (typically `harn_clock::RealClock::arc()`).
    pub fn in_memory(clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::new(Arc::new(InMemoryLimitStore::new(clock)))
    }

    pub fn set_tenant_override(&self, tenant: TenantId, value: TenantOverride) {
        self.tenant_overrides
            .lock()
            .expect("tenant overrides poisoned")
            .insert(tenant, value);
    }

    /// Snapshot of admitted vs. rejected counts. Cheap (atomic loads).
    pub fn stats(&self) -> LimitStats {
        self.stats.snapshot()
    }

    fn multiplier_for(&self, tenant: Option<&TenantId>) -> f64 {
        let Some(tenant) = tenant else {
            return 1.0;
        };
        self.tenant_overrides
            .lock()
            .expect("tenant overrides poisoned")
            .get(tenant)
            .map(|o| o.multiplier)
            .unwrap_or(1.0)
    }

    fn route_in_flight_handle(&self, route: &str) -> Arc<AtomicUsize> {
        // Fast path: lookup with the borrowed `&str` to skip the
        // allocation when the route is already known. Falls through to
        // an owned-key insertion when it isn't, which only happens
        // once per route over the process lifetime.
        let mut in_flight = self.in_flight.lock().expect("in-flight map poisoned");
        if let Some(handle) = in_flight.get(route) {
            return handle.clone();
        }
        let counter = Arc::new(AtomicUsize::new(0));
        in_flight.insert(route.to_string(), counter.clone());
        counter
    }

    /// Bump a stats counter by scope, used by both successful-admit
    /// and rejection paths so the per-scope rejection counters stay
    /// in lockstep with the [`LimitScope`] enum (adding a new scope
    /// only requires extending this match + the struct).
    fn record_rejection(&self, scope: LimitScope) {
        let counter = match scope {
            LimitScope::Route => &self.stats.rejected_route,
            LimitScope::Tenant => &self.stats.rejected_tenant,
            LimitScope::Scope => &self.stats.rejected_scope,
            LimitScope::Backpressure => &self.stats.rejected_backpressure,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Try one rate dimension against the underlying store. Returns
    /// `Some(retry_after_ms)` on rejection (registry has already
    /// counted the rejection and emitted telemetry); `None` on
    /// admission. Centralises the "build spec, try_admit, emit
    /// rejection" plumbing the three rate dimensions all share.
    fn admit_rate_dimension(
        &self,
        ctx: &LimitContext<'_>,
        scope: LimitScope,
        quota: Quota,
        limits: &RouteLimits,
        multiplier: f64,
        key: &str,
    ) -> Option<u64> {
        let spec = scaled_bucket(limits.bucket_for(quota), multiplier);
        let CellDecision::Rejected { retry_after_ms } = self.store.try_admit(key, &spec, 1) else {
            return None;
        };
        self.record_rejection(scope);
        emit_rejection(ctx, scope, retry_after_ms);
        Some(retry_after_ms)
    }

    /// Evaluate every declared bucket and the backpressure watermark
    /// for one dispatch. Returns admittedness; on rejection, callers
    /// surface the smallest `retry_after_ms` over `Retry-After`.
    pub fn check(&self, ctx: &LimitContext<'_>, limits: &RouteLimits) -> LimitDecision {
        if limits.is_unlimited() {
            self.stats.admitted.fetch_add(1, Ordering::Relaxed);
            return LimitDecision::Allowed(LimitGuard::unbounded());
        }

        let multiplier = self.multiplier_for(ctx.tenant_id);

        // Evaluate each rate dimension. We must charge every dimension
        // a token (so a later 429 in a wider bucket doesn't leave a
        // narrower bucket starved), but we early-return on the first
        // rejection to keep noise rejections cheap.

        if let Some(quota) = limits.per_route {
            let key = format!("route:{}", ctx.route);
            if let Some(retry_after_ms) =
                self.admit_rate_dimension(ctx, LimitScope::Route, quota, limits, multiplier, &key)
            {
                return LimitDecision::Rejected {
                    scope: LimitScope::Route,
                    retry_after_ms,
                };
            }
        }

        if let Some(quota) = limits.per_tenant {
            // Without a bound tenant the per_tenant bucket is meaningless,
            // but rejecting "no tenant" outright would surprise dev/test
            // callers who haven't wired authentication yet — instead fold
            // anonymous traffic into a shared `__anon__` bucket so the
            // ceiling still bounds runaway clients.
            let tenant_key = ctx.tenant_id.map(|t| t.0.as_str()).unwrap_or("__anon__");
            let key = format!("tenant:{tenant_key}:{}", ctx.route);
            if let Some(retry_after_ms) =
                self.admit_rate_dimension(ctx, LimitScope::Tenant, quota, limits, multiplier, &key)
            {
                return LimitDecision::Rejected {
                    scope: LimitScope::Tenant,
                    retry_after_ms,
                };
            }
        }

        if let Some(quota) = limits.per_scope {
            // Joined-scope keying matches a cloud gateway's semantics:
            // callers that present the same scope set share quota.
            let key = format!("scope:{}:{}", scope_key(ctx.scopes), ctx.route);
            if let Some(retry_after_ms) =
                self.admit_rate_dimension(ctx, LimitScope::Scope, quota, limits, multiplier, &key)
            {
                return LimitDecision::Rejected {
                    scope: LimitScope::Scope,
                    retry_after_ms,
                };
            }
        }

        // Backpressure: atomic increment-then-test against the
        // watermark. We can't do a CAS loop because the counter has no
        // upper bound at the type level, so we fetch_add unconditionally
        // and roll back when the prior value already met or exceeded
        // the cap. This closes the check-then-act race that a separate
        // load + increment would leave open under concurrent dispatch.
        if let Some(max) = limits.in_flight_max {
            let counter = self.route_in_flight_handle(ctx.route);
            let prev = counter.fetch_add(1, Ordering::AcqRel);
            if prev >= max as usize {
                counter.fetch_sub(1, Ordering::AcqRel);
                let retry_after_ms = BACKPRESSURE_RETRY_AFTER_MS;
                self.record_rejection(LimitScope::Backpressure);
                emit_rejection(ctx, LimitScope::Backpressure, retry_after_ms);
                return LimitDecision::Rejected {
                    scope: LimitScope::Backpressure,
                    retry_after_ms,
                };
            }
            self.stats.admitted.fetch_add(1, Ordering::Relaxed);
            return LimitDecision::Allowed(LimitGuard::for_incremented(counter));
        }

        self.stats.admitted.fetch_add(1, Ordering::Relaxed);
        LimitDecision::Allowed(LimitGuard::unbounded())
    }
}

/// Backpressure rejections drain by *completions*, not by clock — the
/// caller's hint is a short poll interval, not a quota window.
const BACKPRESSURE_RETRY_AFTER_MS: u64 = 250;

fn scope_key(scopes: &std::collections::BTreeSet<String>) -> String {
    if scopes.is_empty() {
        return "__none__".to_string();
    }
    let mut out = String::new();
    for scope in scopes {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(scope);
    }
    out
}

fn scaled_bucket(mut spec: BucketSpec, multiplier: f64) -> BucketSpec {
    if (multiplier - 1.0).abs() < f64::EPSILON {
        return spec;
    }
    let multiplier = multiplier.max(0.0);
    spec.rate_per_sec = (spec.rate_per_sec * multiplier).max(0.0);
    let scaled_capacity = (spec.capacity as f64 * multiplier).ceil().max(1.0);
    spec.capacity = scaled_capacity.min(u32::MAX as f64) as u32;
    spec
}

fn emit_rejection(ctx: &LimitContext<'_>, scope: LimitScope, retry_after_ms: u64) {
    tracing::warn!(
        target: "harn.serve.limits",
        route = ctx.route,
        tenant = ctx.tenant_id.map(|t| t.0.as_str()).unwrap_or(""),
        scope = scope.as_str(),
        retry_after_ms,
        "rate limit rejection"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_clock::PausedClock;
    use std::collections::BTreeSet;
    use std::time::Duration;
    use time::OffsetDateTime;

    fn paused() -> Arc<PausedClock> {
        PausedClock::new(OffsetDateTime::UNIX_EPOCH)
    }

    fn ctx<'a>(
        route: &'a str,
        tenant: Option<&'a TenantId>,
        scopes: &'a BTreeSet<String>,
    ) -> LimitContext<'a> {
        LimitContext {
            route,
            tenant_id: tenant,
            scopes,
        }
    }

    #[test]
    fn quota_parse_handles_common_shapes() {
        assert_eq!(
            Quota::parse("100/min"),
            Some(Quota::new(100, QuotaWindow::Minute))
        );
        assert_eq!(
            Quota::parse("1_000/hour"),
            Some(Quota::new(1_000, QuotaWindow::Hour))
        );
        assert_eq!(
            Quota::parse(" 50 / s "),
            Some(Quota::new(50, QuotaWindow::Second))
        );
        assert_eq!(Quota::parse("0/min"), None);
        assert_eq!(Quota::parse("nope"), None);
    }

    #[test]
    fn unlimited_route_is_admitted_cheaply() {
        let registry = LimitRegistry::in_memory(paused());
        let scopes = BTreeSet::new();
        let limits = RouteLimits::default();
        let decision = registry.check(&ctx("/x", None, &scopes), &limits);
        assert!(matches!(decision, LimitDecision::Allowed(_)));
        assert_eq!(registry.stats().admitted, 1);
    }

    #[test]
    fn per_tenant_quota_isolates_tenants() {
        let registry = LimitRegistry::in_memory(paused());
        let scopes = BTreeSet::new();
        let tenant_a = TenantId::new("a");
        let tenant_b = TenantId::new("b");
        let limits = RouteLimits {
            per_tenant: Some(Quota::new(1, QuotaWindow::Second)),
            burst: Some(2),
            ..RouteLimits::default()
        };

        // Tenant A burns its burst of 2.
        assert!(matches!(
            registry.check(&ctx("/r", Some(&tenant_a), &scopes), &limits),
            LimitDecision::Allowed(_)
        ));
        assert!(matches!(
            registry.check(&ctx("/r", Some(&tenant_a), &scopes), &limits),
            LimitDecision::Allowed(_)
        ));
        // 3rd hits the tenant quota.
        let rejected = registry.check(&ctx("/r", Some(&tenant_a), &scopes), &limits);
        let LimitDecision::Rejected { scope, .. } = rejected else {
            panic!("expected rejection, got {rejected:?}");
        };
        assert_eq!(scope, LimitScope::Tenant);

        // Tenant B is unaffected — distinct bucket.
        assert!(matches!(
            registry.check(&ctx("/r", Some(&tenant_b), &scopes), &limits),
            LimitDecision::Allowed(_)
        ));
    }

    #[test]
    fn backpressure_rejects_when_watermark_reached() {
        let registry = LimitRegistry::in_memory(paused());
        let scopes = BTreeSet::new();
        let limits = RouteLimits {
            in_flight_max: Some(2),
            ..RouteLimits::default()
        };

        let _g1 = match registry.check(&ctx("/r", None, &scopes), &limits) {
            LimitDecision::Allowed(g) => g,
            other => panic!("{other:?}"),
        };
        let _g2 = match registry.check(&ctx("/r", None, &scopes), &limits) {
            LimitDecision::Allowed(g) => g,
            other => panic!("{other:?}"),
        };
        let rejected = registry.check(&ctx("/r", None, &scopes), &limits);
        match rejected {
            LimitDecision::Rejected {
                scope,
                retry_after_ms,
            } => {
                assert_eq!(scope, LimitScope::Backpressure);
                assert!(retry_after_ms > 0);
            }
            other => panic!("expected rejection, got {other:?}"),
        }

        // Drop g2 → slot frees up.
        drop(_g2);
        assert!(matches!(
            registry.check(&ctx("/r", None, &scopes), &limits),
            LimitDecision::Allowed(_)
        ));
    }

    #[test]
    fn tenant_override_widens_bucket() {
        let registry = LimitRegistry::in_memory(paused());
        let scopes = BTreeSet::new();
        let tenant = TenantId::new("vip");
        registry.set_tenant_override(tenant.clone(), TenantOverride { multiplier: 4.0 });

        let limits = RouteLimits {
            per_tenant: Some(Quota::new(1, QuotaWindow::Second)),
            burst: Some(1),
            ..RouteLimits::default()
        };

        // Multiplier 4 → effective burst 4. Burn through 4 admissions.
        for _ in 0..4 {
            assert!(matches!(
                registry.check(&ctx("/r", Some(&tenant), &scopes), &limits),
                LimitDecision::Allowed(_)
            ));
        }
        // 5th rejects.
        assert!(matches!(
            registry.check(&ctx("/r", Some(&tenant), &scopes), &limits),
            LimitDecision::Rejected { .. }
        ));
    }

    #[test]
    fn ten_x_burst_settles_to_steady_rate_after_window() {
        // Acceptance scenario distilled: a burst of 10× the steady rate
        // should drain to exactly the rate by the time the window
        // rolls. We model it as 1/sec rate, burst 1 — fire 10 → 1
        // admitted, 9 rejected; advance 10s → 10 more admitted (one
        // per second of refill); next reject again.
        let clock = paused();
        let registry = LimitRegistry::in_memory(clock.clone());
        let scopes = BTreeSet::new();
        let limits = RouteLimits {
            per_route: Some(Quota::new(1, QuotaWindow::Second)),
            burst: Some(1),
            ..RouteLimits::default()
        };

        let mut admitted_burst = 0;
        for _ in 0..10 {
            if let LimitDecision::Allowed(_) = registry.check(&ctx("/r", None, &scopes), &limits) {
                admitted_burst += 1;
            }
        }
        assert_eq!(admitted_burst, 1, "burst should exhaust capacity");

        // Advance 10 seconds → 10 refilled tokens become 1 (capacity 1).
        // Actually each second yields 1 admit; loop one admit per advance.
        let mut admitted_steady = 0;
        for _ in 0..10 {
            clock.advance(Duration::from_secs(1));
            if let LimitDecision::Allowed(_) = registry.check(&ctx("/r", None, &scopes), &limits) {
                admitted_steady += 1;
            }
        }
        assert_eq!(admitted_steady, 10);

        // Stats: 1 burst + 10 steady = 11 admitted, 9 rejected.
        let stats = registry.stats();
        assert_eq!(stats.admitted, 11);
        assert_eq!(stats.total_rejected(), 9);
    }
}
