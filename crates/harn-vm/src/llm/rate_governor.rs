//! LLM provider rate/concurrency governor — Layer 0 (detection) + Layer 1
//! (per-process adaptive governor).
//!
//! This module governs Harn's concurrency/rate against provider throttling,
//! keyed by `(provider, org_key)`, so overloads become structured runtime
//! state instead of provider-specific branches at call sites:
//!   - **L0 detection** ([`ThrottleSignal::classify`]): turns a provider outcome
//!     into a throttle signal from STRUCTURED inputs (HTTP status, Retry-After,
//!     Anthropic overloaded/rate_limit body, and the empty-completion-under-load
//!     heuristic), emitted as a `provider_throttle` transcript record.
//!   - **L1 governor** ([`ProviderGovernor`]): an AIMD concurrency limiter
//!     (additive-increase on sustained success toward the configured max,
//!     multiplicative-decrease on a throttle signal), RPM+TPM token buckets, and
//!     a circuit breaker (CLOSED → OPEN with exponential backoff + full jitter,
//!     honoring Retry-After → HALF-OPEN single probe → CLOSED). Retries WAIT
//!     behind the governor instead of blind-firing.
//!
//! SCOPE / SEAMS. This is Layer 0 + Layer 1 only. It is a PER-PROCESS governor.
//! Layer 2 (shared-local Harn state leases so the aggregate across processes
//! respects an org-wide quota) and Layer 3 (Harn Cloud lease authority) are
//! follow-ups. The registry key already carries `org_key`, and
//! [`ProviderGovernor`] state is intentionally serializable-shaped, so a shared
//! store can back the same interface without a call-site change. The existing
//! route limiter ([`super::rate_limit`]) keeps its own token
//! buckets/durable layer/network breaker; this governor ADDS adaptive
//! concurrency + a throttle-signal (429/overload/empty-under-load) circuit
//! breaker + the structured throttle/state records, and only when the
//! `llm.rate_governor` flag is on.
//!
//! FLAG. Everything here is behind [`enabled`] (`HARN_LLM_RATE_GOVERNOR=1`,
//! default OFF). When off, [`gate`]/[`record_outcome`] short-circuit before
//! touching any state, so a healthy no-throttle run is byte-for-byte
//! unaffected in behavior.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::capabilities::{GovernorBackoff, ProviderLimits};

/// Env flag that arms the governor. Default OFF. Mirrors the env-boolean gate
/// style the rest of the LLM layer uses (`HARN_LLM_RATE_LIMIT_DURABLE`, ...).
/// The user-facing flag name is `llm.rate_governor`.
const RATE_GOVERNOR_ENABLED_ENV: &str = "HARN_LLM_RATE_GOVERNOR";

/// Conservative built-in defaults, used for any provider without a
/// `[provider_limits.<provider>]` catalog row. Deliberately gentle: the goal is
/// to break a stampede, not to bottleneck a healthy fleet.
const DEFAULT_MAX_CONCURRENCY: u32 = 8;
const DEFAULT_MIN_CONCURRENCY: u32 = 1;
const DEFAULT_BACKOFF_BASE_MS: u64 = 1_000;
const DEFAULT_BACKOFF_MAX_MS: u64 = 60_000;
const DEFAULT_BACKOFF_MULTIPLIER: f64 = 2.0;

/// Consecutive throttle signals that trip the circuit OPEN. One blip is noise
/// (the AIMD decrease already reacts); a short streak is the org-throttle
/// signature that must stop the hammering.
const THROTTLE_STREAK_TO_OPEN: u32 = 3;

/// Successful serves required at the current limit before AIMD additively
/// increases concurrency by one. Slower-than-decrease on purpose (AIMD): climb
/// gently, retreat fast.
const SUCCESSES_PER_INCREASE: u32 = 4;

/// Is the governor armed? `HARN_LLM_RATE_GOVERNOR` in {`1`,`true`,`yes`,`on`}
/// (case-insensitive) turns it on. Anything else — including unset — is OFF.
pub fn enabled() -> bool {
    std::env::var(RATE_GOVERNOR_ENABLED_ENV)
        .map(|raw| {
            let v = raw.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// The structured throttle signal L0 detection produces. Ordering matters for
/// [`classify`]: the strongest, most specific signal wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleSignal {
    /// HTTP 429 (rate limit) — org-wide or per-key.
    RateLimit429,
    /// HTTP 529 / 503, or an Anthropic `overloaded_error` body.
    Overloaded,
    /// A completion that billed output tokens but committed nothing, arriving
    /// while the provider is already known-throttled (its circuit is not
    /// CLOSED). Treated as a soft throttle, not a capability failure.
    EmptyUnderLoad,
}

impl ThrottleSignal {
    /// Stable machine label for the `provider_throttle` record and CLI/MCP.
    pub fn label(self) -> &'static str {
        match self {
            ThrottleSignal::RateLimit429 => "rate_limit_429",
            ThrottleSignal::Overloaded => "overloaded",
            ThrottleSignal::EmptyUnderLoad => "empty_under_load",
        }
    }

    /// Classify a provider outcome into a throttle signal from STRUCTURED
    /// inputs — never a log string. Returns `None` when the outcome carries no
    /// throttle evidence.
    ///
    /// - `http_status`: the response status, when the outcome was an HTTP error.
    /// - `body_lower`: the (already-lowercased) error body / message, checked
    ///   for the Anthropic `overloaded_error` / `rate_limit_error` /
    ///   "temporarily limiting requests" markers and a bare `429`/`529`.
    /// - `empty_completion`: the completion billed output tokens but committed
    ///   nothing (content, thinking, tool calls all empty).
    /// - `provider_already_throttled`: this provider's circuit is not CLOSED, so
    ///   an empty completion is attributable to load, not the model.
    pub fn classify(
        http_status: Option<u16>,
        body_lower: &str,
        empty_completion: bool,
        provider_already_throttled: bool,
    ) -> Option<ThrottleSignal> {
        // 429 is the canonical rate-limit signal, from status or body.
        if http_status == Some(429)
            || body_lower.contains("rate_limit_error")
            || body_lower.contains("temporarily limiting requests")
            || body_lower.contains(" 429 ")
            || body_lower.starts_with("429 ")
        {
            return Some(ThrottleSignal::RateLimit429);
        }
        // Overload: 529/503 or the Anthropic overloaded body.
        if matches!(http_status, Some(529) | Some(503))
            || body_lower.contains("overloaded_error")
            || body_lower.contains("overloaded")
        {
            return Some(ThrottleSignal::Overloaded);
        }
        // Empty-completion-under-load: only a throttle signal when the provider
        // is already known-throttled. A lone empty on a healthy provider is a
        // capability or retry question, not a throttle.
        if empty_completion && provider_already_throttled {
            return Some(ThrottleSignal::EmptyUnderLoad);
        }
        None
    }
}

/// The circuit-breaker state for one `(provider, org_key)`. The high-signal
/// value for the auto-response seam is `is_open`: a run that failed while the
/// circuit was OPEN is infra-throttled, not a capability FAIL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Dispatching normally.
    Closed,
    /// Paused. `until_ms` is the wall-clock ms at which a single half-open probe
    /// is admitted.
    Open { until_ms: u128 },
    /// One probe is in flight; further calls fail fast until it resolves.
    HalfOpen,
}

impl CircuitState {
    fn label(self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open { .. } => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

/// Resolved, defaulted governor limits for one provider. Built from the catalog
/// [`ProviderLimits`] row overlaid on the conservative built-in defaults, so
/// every field is concrete.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedLimits {
    pub max_concurrency: u32,
    pub min_concurrency: u32,
    pub rpm: Option<u32>,
    pub tpm: Option<u64>,
    pub adaptive: bool,
    pub backoff_base_ms: u64,
    pub backoff_max_ms: u64,
    pub backoff_multiplier: f64,
    pub backoff_jitter: bool,
}

impl Default for ResolvedLimits {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            min_concurrency: DEFAULT_MIN_CONCURRENCY,
            rpm: None,
            tpm: None,
            adaptive: true,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
            backoff_multiplier: DEFAULT_BACKOFF_MULTIPLIER,
            backoff_jitter: true,
        }
    }
}

impl ResolvedLimits {
    /// Overlay a catalog row on the defaults. `min_concurrency` is clamped to
    /// `<= max_concurrency` and `>= 1` so the AIMD floor can never invert the
    /// ceiling or stall the provider to zero.
    pub fn from_catalog(row: Option<&ProviderLimits>) -> Self {
        let mut resolved = ResolvedLimits::default();
        let Some(row) = row else { return resolved };
        if let Some(v) = row.max_concurrency {
            resolved.max_concurrency = v.max(1);
        }
        if let Some(v) = row.min_concurrency {
            resolved.min_concurrency = v.max(1);
        }
        resolved.min_concurrency = resolved.min_concurrency.min(resolved.max_concurrency);
        resolved.rpm = row.rpm;
        resolved.tpm = row.tpm;
        if let Some(v) = row.adaptive {
            resolved.adaptive = v;
        }
        if let Some(b) = row.backoff.as_ref() {
            apply_backoff(&mut resolved, b);
        }
        resolved
    }
}

fn apply_backoff(resolved: &mut ResolvedLimits, b: &GovernorBackoff) {
    if let Some(v) = b.base_ms {
        resolved.backoff_base_ms = v.max(1);
    }
    if let Some(v) = b.max_ms {
        resolved.backoff_max_ms = v.max(resolved.backoff_base_ms);
    }
    if let Some(v) = b.multiplier {
        // A multiplier < 1 would SHRINK the window each cycle (nonsense); floor
        // at 1.0 so backoff is monotonic non-decreasing.
        resolved.backoff_multiplier = if v.is_finite() && v >= 1.0 { v } else { 1.0 };
    }
    if let Some(v) = b.jitter {
        resolved.backoff_jitter = v;
    }
}

/// A minute-window token bucket (RPM or TPM). Sliding window over the last
/// 60_000 ms. `charge` clamps an oversized single request to `max` so one giant
/// request runs (then the next waits) rather than deadlocking the bucket.
#[derive(Debug)]
struct TokenBucket {
    max: u64,
    window_ms: u128,
    entries: std::collections::VecDeque<(u128, u64)>,
}

impl TokenBucket {
    fn new(max: u64) -> Self {
        Self {
            max: max.max(1),
            window_ms: 60_000,
            entries: std::collections::VecDeque::new(),
        }
    }

    fn prune(&mut self, now_ms: u128) {
        while self
            .entries
            .front()
            .is_some_and(|(t, _)| now_ms.saturating_sub(*t) >= self.window_ms)
        {
            self.entries.pop_front();
        }
    }

    fn used(&self) -> u64 {
        self.entries.iter().map(|(_, u)| *u).sum()
    }

    /// If the bucket cannot fit `units` now, return how long to wait for the
    /// oldest entry to age out. Otherwise `None`.
    fn wait_for(&mut self, now_ms: u128, units: u64) -> Option<Duration> {
        self.prune(now_ms);
        let charge = units.min(self.max);
        if self.used().saturating_add(charge) <= self.max {
            return None;
        }
        // Wait for the oldest entry to exit the window.
        self.entries.front().map(|(t, _)| {
            let elapsed = now_ms.saturating_sub(*t);
            let remaining = self.window_ms.saturating_sub(elapsed);
            Duration::from_millis(remaining.min(u128::from(u64::MAX)) as u64)
        })
    }

    fn record(&mut self, now_ms: u128, units: u64) {
        let charge = units.min(self.max);
        if charge > 0 {
            self.entries.push_back((now_ms, charge));
        }
    }
}

/// The per-(provider, org_key) governor: AIMD concurrency limit, circuit
/// breaker, RPM/TPM buckets. This is a plain data object guarded by the registry
/// mutex — deliberately NOT holding any tokio primitive, so its shape can back a
/// Layer-2 shared store unchanged.
#[derive(Debug)]
pub struct ProviderGovernor {
    limits: ResolvedLimits,
    /// Current AIMD concurrency ceiling (between min and max).
    concurrency_limit: u32,
    /// In-flight requests currently holding a slot.
    in_flight: u32,
    /// Consecutive serves at the current limit (drives additive increase).
    consecutive_successes: u32,
    /// Consecutive throttle signals (drives the OPEN transition).
    consecutive_throttles: u32,
    /// How many times the circuit has opened without an intervening CLOSED
    /// (drives exponential backoff growth). Reset when a probe closes it.
    open_cycles: u32,
    circuit: CircuitState,
    rpm_bucket: Option<TokenBucket>,
    tpm_bucket: Option<TokenBucket>,
    /// Last observed throttle signal, for the status surface.
    last_signal: Option<ThrottleSignal>,
    /// Deterministic PRNG state for full jitter (mockable clock keeps tests
    /// reproducible; this keeps the jitter reproducible too).
    jitter_state: u64,
}

impl ProviderGovernor {
    fn new(limits: ResolvedLimits) -> Self {
        let rpm_bucket = limits.rpm.map(|r| TokenBucket::new(u64::from(r)));
        let tpm_bucket = limits.tpm.map(TokenBucket::new);
        Self {
            concurrency_limit: limits.max_concurrency,
            in_flight: 0,
            consecutive_successes: 0,
            consecutive_throttles: 0,
            open_cycles: 0,
            circuit: CircuitState::Closed,
            rpm_bucket,
            tpm_bucket,
            last_signal: None,
            jitter_state: 0x9E37_79B9_7F4A_7C15,
            limits,
        }
    }

    /// A provider is "already throttled" (for the empty-under-load heuristic)
    /// whenever its circuit is not CLOSED.
    fn is_throttled(&self) -> bool {
        !matches!(self.circuit, CircuitState::Closed)
    }

    /// Advance a possibly-elapsed OPEN window to HALF-OPEN. Returns the current
    /// (post-transition) state.
    fn tick_circuit(&mut self, now_ms: u128) -> CircuitState {
        if let CircuitState::Open { until_ms } = self.circuit {
            if now_ms >= until_ms {
                self.circuit = CircuitState::HalfOpen;
            }
        }
        self.circuit
    }

    /// Decide whether a call may proceed now. Returns:
    /// - `Proceed` — a slot was reserved (`in_flight` incremented); the caller
    ///   MUST later call [`ProviderGovernor::release`] via [`record_outcome`].
    /// - `Wait(d)` — back off `d` then re-gate (bucket full, or OPEN window not
    ///   elapsed, or concurrency saturated).
    /// - `CircuitOpen(d)` — the circuit is OPEN and no probe is due; prefer a
    ///   fallback provider. `d` is the remaining OPEN window.
    fn decide(&mut self, now_ms: u128, est_tokens: u64) -> GateOutcome {
        match self.tick_circuit(now_ms) {
            CircuitState::Open { until_ms } => {
                let remaining = until_ms.saturating_sub(now_ms);
                return GateOutcome::CircuitOpen(Duration::from_millis(
                    remaining.min(u128::from(u64::MAX)) as u64,
                ));
            }
            CircuitState::HalfOpen => {
                // Admit exactly one probe: if none is in flight, let this one
                // through; otherwise wait for the probe to resolve.
                if self.in_flight > 0 {
                    return GateOutcome::Wait(Duration::from_millis(50));
                }
            }
            CircuitState::Closed => {}
        }

        // Token buckets (skip on the half-open probe: a single probe should not
        // be starved by a stale bucket — the point is to test liveness).
        if matches!(self.circuit, CircuitState::Closed) {
            if let Some(b) = self.rpm_bucket.as_mut() {
                if let Some(d) = b.wait_for(now_ms, 1) {
                    return GateOutcome::Wait(d);
                }
            }
            if let Some(b) = self.tpm_bucket.as_mut() {
                if let Some(d) = b.wait_for(now_ms, est_tokens) {
                    return GateOutcome::Wait(d);
                }
            }
        }

        // Concurrency limit (half-open forces effective limit 1).
        let effective_limit = if matches!(self.circuit, CircuitState::HalfOpen) {
            1
        } else {
            self.concurrency_limit
        };
        if self.in_flight >= effective_limit {
            return GateOutcome::Wait(Duration::from_millis(25));
        }

        // Reserve the slot and charge the buckets.
        self.in_flight += 1;
        if matches!(self.circuit, CircuitState::Closed) {
            if let Some(b) = self.rpm_bucket.as_mut() {
                b.record(now_ms, 1);
            }
            if let Some(b) = self.tpm_bucket.as_mut() {
                b.record(now_ms, est_tokens);
            }
        }
        GateOutcome::Proceed
    }

    /// Release a reserved slot and apply the outcome to the AIMD limit + circuit.
    fn release_success(&mut self, now_ms: u128) {
        self.in_flight = self.in_flight.saturating_sub(1);
        // A serve closes a half-open probe and fully resets the circuit.
        if matches!(self.circuit, CircuitState::HalfOpen) {
            self.circuit = CircuitState::Closed;
            self.open_cycles = 0;
        }
        self.consecutive_throttles = 0;
        // AIMD additive increase: climb one step per SUCCESSES_PER_INCREASE
        // serves, capped at max.
        if self.limits.adaptive {
            self.consecutive_successes = self.consecutive_successes.saturating_add(1);
            if self.consecutive_successes >= SUCCESSES_PER_INCREASE {
                self.consecutive_successes = 0;
                if self.concurrency_limit < self.limits.max_concurrency {
                    self.concurrency_limit += 1;
                }
            }
        }
        let _ = now_ms;
    }

    /// Release a reserved slot after a throttle signal: AIMD multiplicative
    /// decrease + circuit accounting. `retry_after_ms` (if present) pins the
    /// OPEN window floor.
    fn release_throttle(
        &mut self,
        now_ms: u128,
        signal: ThrottleSignal,
        retry_after_ms: Option<u64>,
    ) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.last_signal = Some(signal);
        self.consecutive_successes = 0;

        // AIMD multiplicative decrease: halve, floored at min.
        if self.limits.adaptive {
            let halved = (self.concurrency_limit / 2).max(self.limits.min_concurrency);
            self.concurrency_limit = halved;
        }

        // A throttle during a half-open probe re-opens immediately.
        if matches!(self.circuit, CircuitState::HalfOpen) {
            self.open_circuit(now_ms, retry_after_ms);
            return;
        }

        self.consecutive_throttles = self.consecutive_throttles.saturating_add(1);
        if self.consecutive_throttles >= THROTTLE_STREAK_TO_OPEN {
            self.open_circuit(now_ms, retry_after_ms);
        }
    }

    /// Release a reserved slot after a NON-throttle failure (e.g. a plain
    /// provider/network error). Neither AIMD nor the throttle circuit reacts —
    /// that is the existing route limiter's/network breaker's job — but the slot
    /// must be freed. A non-throttle error does NOT reset the throttle streak
    /// (the streak is about throttles specifically).
    fn release_neutral(&mut self, _now_ms: u128) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if matches!(self.circuit, CircuitState::HalfOpen) {
            // A probe that errored non-throttle is inconclusive; keep it
            // half-open so the next call re-probes rather than declaring health.
        }
    }

    fn open_circuit(&mut self, now_ms: u128, retry_after_ms: Option<u64>) {
        self.open_cycles = self.open_cycles.saturating_add(1);
        let window = self.backoff_window_ms(retry_after_ms);
        self.circuit = CircuitState::Open {
            until_ms: now_ms.saturating_add(u128::from(window)),
        };
        self.consecutive_throttles = 0;
    }

    /// Exponential backoff with full jitter, honoring Retry-After as a floor.
    /// Window = `min(base * mult^(open_cycles-1), max)`, then (if jitter) a
    /// uniform draw in `[window/2, window]` — we jitter the top half so backoff
    /// never collapses toward zero but still de-synchronizes N agents.
    fn backoff_window_ms(&mut self, retry_after_ms: Option<u64>) -> u64 {
        let exp = self.open_cycles.saturating_sub(1);
        let mut window =
            self.limits.backoff_base_ms as f64 * self.limits.backoff_multiplier.powi(exp as i32);
        if !window.is_finite() {
            window = self.limits.backoff_max_ms as f64;
        }
        let mut window = (window as u64).min(self.limits.backoff_max_ms);
        if self.limits.backoff_jitter && window > 0 {
            let half = window / 2;
            let span = window - half;
            let draw = if span > 0 {
                self.next_jitter() % (span + 1)
            } else {
                0
            };
            window = half + draw;
        }
        // A provider Retry-After is authoritative: never wake before it.
        window.max(retry_after_ms.unwrap_or(0))
    }

    /// xorshift64* — deterministic, no external dep, good enough to de-sync
    /// backoff wakeups. Reproducible so jitter is testable.
    fn next_jitter(&mut self) -> u64 {
        let mut x = self.jitter_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.jitter_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A snapshot for `governor_state` records and the status surface.
    pub fn snapshot(&self) -> GovernorSnapshot {
        GovernorSnapshot {
            concurrency_limit: self.concurrency_limit,
            max_concurrency: self.limits.max_concurrency,
            min_concurrency: self.limits.min_concurrency,
            in_flight: self.in_flight,
            circuit_state: self.circuit.label(),
            consecutive_throttles: self.consecutive_throttles,
            open_cycles: self.open_cycles,
            last_signal: self.last_signal.map(|s| s.label()),
            rpm: self.limits.rpm,
            tpm: self.limits.tpm,
            adaptive: self.limits.adaptive,
        }
    }
}

/// The decision returned by [`gate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// Proceed with the call; a slot was reserved and MUST be released via
    /// [`record_outcome`].
    Proceed,
    /// Back off for this long, then re-gate.
    Wait(Duration),
    /// The circuit is OPEN; prefer a fallback provider. Duration is the
    /// remaining OPEN window.
    CircuitOpen(Duration),
}

/// A plain, serializable-shaped snapshot of one governor's state for
/// `governor_state` records and the CLI/MCP status surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernorSnapshot {
    pub concurrency_limit: u32,
    pub max_concurrency: u32,
    pub min_concurrency: u32,
    pub in_flight: u32,
    pub circuit_state: &'static str,
    pub consecutive_throttles: u32,
    pub open_cycles: u32,
    pub last_signal: Option<&'static str>,
    pub rpm: Option<u32>,
    pub tpm: Option<u64>,
    pub adaptive: bool,
}

impl GovernorSnapshot {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "concurrency_limit": self.concurrency_limit,
            "max_concurrency": self.max_concurrency,
            "min_concurrency": self.min_concurrency,
            "in_flight": self.in_flight,
            "circuit_state": self.circuit_state,
            "consecutive_throttles": self.consecutive_throttles,
            "open_cycles": self.open_cycles,
            "last_signal": self.last_signal,
            "rpm": self.rpm,
            "tpm": self.tpm,
            "adaptive": self.adaptive,
        })
    }
}

// ---------------------------------------------------------------------------
// Process-global registry, keyed by (provider, org_key).
// ---------------------------------------------------------------------------

/// The org identity used to key the governor. Providers with distinct API keys
/// have distinct org quotas; a stable, non-secret fingerprint of the key keeps
/// them apart WITHOUT storing the secret. Empty key → `"default"`.
pub fn org_key_id(api_key: &str) -> String {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    // Non-reversible short fingerprint (FNV-1a). Never the raw key.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in trimmed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("k{hash:016x}")
}

fn route_key(provider: &str, org_key: &str) -> String {
    format!("{}::{}", provider.trim().to_ascii_lowercase(), org_key)
}

#[derive(Default)]
struct Registry {
    governors: HashMap<String, ProviderGovernor>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn with_governor<R>(
    provider: &str,
    org_key: &str,
    f: impl FnOnce(&mut ProviderGovernor) -> R,
) -> R {
    let key = route_key(provider, org_key);
    let mut reg = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let governor = reg.governors.entry(key).or_insert_with(|| {
        let limits = ResolvedLimits::from_catalog(
            super::capabilities::provider_limits_for(provider).as_ref(),
        );
        ProviderGovernor::new(limits)
    });
    f(governor)
}

/// Gate a call through the governor. No-op `Proceed` when the flag is OFF (the
/// caller then behaves exactly as it does today). When ON, reserves a slot /
/// asks the caller to wait / reports the circuit OPEN.
///
/// `est_tokens` is the request's estimated (input + output) token count for TPM
/// charging; pass 0 when unknown.
pub fn gate(provider: &str, org_key: &str, est_tokens: u64) -> GateOutcome {
    if !enabled() {
        return GateOutcome::Proceed;
    }
    let now_ms = crate::clock_mock::instant_now().as_millis();
    with_governor(provider, org_key, |g| g.decide(now_ms, est_tokens))
}

/// Whether the empty-completion-under-load heuristic should fire for this
/// provider right now — i.e. the provider is already known-throttled. Cheap
/// read for the detection point. `false` when the flag is off.
pub fn provider_already_throttled(provider: &str, org_key: &str) -> bool {
    if !enabled() {
        return false;
    }
    with_governor(provider, org_key, |g| g.is_throttled())
}

/// The normalized outcome the governor records for a gated call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorOutcome {
    /// The call served content/tool-calls. AIMD additive increase; closes a
    /// half-open probe.
    Served,
    /// The call hit a throttle signal. AIMD multiplicative decrease + circuit
    /// accounting. `retry_after_ms` pins the OPEN-window floor.
    Throttled {
        signal: ThrottleSignal,
        retry_after_ms: Option<u64>,
    },
    /// A non-throttle failure (plain provider/network error). Frees the slot;
    /// leaves AIMD and the throttle circuit untouched.
    Neutral,
}

/// Record the outcome of a call previously gated to [`GateOutcome::Proceed`],
/// releasing its reserved slot and driving AIMD + the circuit. No-op when the
/// flag is OFF. MUST be called exactly once per `Proceed` (the retry-loop seam
/// guarantees this).
pub fn record_outcome(provider: &str, org_key: &str, outcome: GovernorOutcome) {
    if !enabled() {
        return;
    }
    let now_ms = crate::clock_mock::instant_now().as_millis();
    with_governor(provider, org_key, |g| match outcome {
        GovernorOutcome::Served => g.release_success(now_ms),
        GovernorOutcome::Throttled {
            signal,
            retry_after_ms,
        } => g.release_throttle(now_ms, signal, retry_after_ms),
        GovernorOutcome::Neutral => g.release_neutral(now_ms),
    });
}

/// Snapshot one governor's state, or `None` if no call has ever touched this
/// (provider, org_key). Used by `governor_state` records and the status surface.
pub fn snapshot(provider: &str, org_key: &str) -> Option<GovernorSnapshot> {
    let key = route_key(provider, org_key);
    let reg = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reg.governors.get(&key).map(ProviderGovernor::snapshot)
}

/// Whether a (provider, org_key)'s circuit is OPEN right now — the auto-response
/// seam. An eval whose run failed while this returns `true` was infra-throttled,
/// not a capability FAIL. Deterministic, no LLM. `false` when the flag is off or
/// the route is unknown.
pub fn circuit_is_open(provider: &str, org_key: &str) -> bool {
    if !enabled() {
        return false;
    }
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let key = route_key(provider, org_key);
    let mut reg = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match reg.governors.get_mut(&key) {
        Some(g) => matches!(g.tick_circuit(now_ms), CircuitState::Open { .. }),
        None => false,
    }
}

/// Every (provider, org_key) the governor has seen, with its snapshot. Powers
/// `harn provider limits` and the `provider_rate_status` MCP tool. Sorted by
/// route key for deterministic output.
pub fn all_snapshots() -> Vec<(String, GovernorSnapshot)> {
    let reg = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut out: Vec<(String, GovernorSnapshot)> = reg
        .governors
        .iter()
        .map(|(k, g)| (k.clone(), g.snapshot()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The resolved (defaulted) limits a provider WOULD govern under, from the
/// catalog alone — no call required. Powers the static half of the CLI status
/// surface (a governor row need not exist yet). Deterministic, no LLM.
pub fn resolved_limits_for(provider: &str) -> ResolvedLimits {
    ResolvedLimits::from_catalog(super::capabilities::provider_limits_for(provider).as_ref())
}

/// Provider ids with explicit limit rows in the effective catalog.
pub fn configured_limit_providers() -> Vec<String> {
    super::capabilities::provider_limit_providers()
}

/// Build the structured `provider_throttle` transcript record (Layer 0). Follows
/// the `resolved_dispatch::build_record` template: a self-contained,
/// provenance-bearing, append-only event.
pub fn build_throttle_record(
    provider: &str,
    org_key: &str,
    signal: ThrottleSignal,
    http_status: Option<u16>,
    retry_after_ms: Option<u64>,
    timestamp: String,
) -> serde_json::Value {
    serde_json::json!({
        "type": "provider_throttle",
        "timestamp": timestamp,
        "provider": provider,
        "org_key_id": org_key,
        "signal_type": signal.label(),
        "http_status": http_status,
        "retry_after_ms": retry_after_ms,
    })
}

/// Build the structured `governor_state` transcript record (observability). Emit
/// alongside a throttle or a limit change so a consumer sees the governor's
/// reaction in the same stream.
pub fn build_state_record(
    provider: &str,
    org_key: &str,
    snapshot: &GovernorSnapshot,
    timestamp: String,
) -> serde_json::Value {
    let mut record = snapshot.to_json();
    if let Some(obj) = record.as_object_mut() {
        obj.insert("type".to_string(), serde_json::json!("governor_state"));
        obj.insert("timestamp".to_string(), serde_json::json!(timestamp));
        obj.insert("provider".to_string(), serde_json::json!(provider));
        obj.insert("org_key_id".to_string(), serde_json::json!(org_key));
    }
    record
}

/// Wipe all governor state. Test isolation between sequential runs.
pub fn reset_for_tests() {
    let mut reg = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reg.governors.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock_mock::{install_override, MockClock};

    /// Serializes the tests that toggle the process-global `HARN_LLM_RATE_GOVERNOR`
    /// env var and share the process-global registry, so cargo's parallel test
    /// threads cannot race the flag or clobber each other's governor state.
    static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Sets the env flag on for the guard's lifetime and restores it after,
    /// holding [`TEST_ENV_LOCK`] the whole time. Also wipes the registry so the
    /// test starts clean.
    struct GovernorEnabledGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl GovernorEnabledGuard {
        fn on() -> Self {
            let lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev = std::env::var(RATE_GOVERNOR_ENABLED_ENV).ok();
            std::env::set_var(RATE_GOVERNOR_ENABLED_ENV, "1");
            reset_for_tests();
            Self { _lock: lock, prev }
        }
    }

    impl Drop for GovernorEnabledGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(RATE_GOVERNOR_ENABLED_ENV, v),
                None => std::env::remove_var(RATE_GOVERNOR_ENABLED_ENV),
            }
            reset_for_tests();
        }
    }

    fn limits(max: u32, min: u32) -> ResolvedLimits {
        ResolvedLimits {
            max_concurrency: max,
            min_concurrency: min,
            ..Default::default()
        }
    }

    // --- L0 detection ---------------------------------------------------

    #[test]
    fn classify_429_from_status() {
        assert_eq!(
            ThrottleSignal::classify(Some(429), "", false, false),
            Some(ThrottleSignal::RateLimit429)
        );
    }

    #[test]
    fn classify_429_from_anthropic_body() {
        assert_eq!(
            ThrottleSignal::classify(None, "type: rate_limit_error", false, false),
            Some(ThrottleSignal::RateLimit429)
        );
        assert_eq!(
            ThrottleSignal::classify(None, "server temporarily limiting requests", false, false),
            Some(ThrottleSignal::RateLimit429)
        );
    }

    #[test]
    fn classify_overloaded_from_status_and_body() {
        assert_eq!(
            ThrottleSignal::classify(Some(529), "", false, false),
            Some(ThrottleSignal::Overloaded)
        );
        assert_eq!(
            ThrottleSignal::classify(Some(503), "", false, false),
            Some(ThrottleSignal::Overloaded)
        );
        assert_eq!(
            ThrottleSignal::classify(None, "overloaded_error", false, false),
            Some(ThrottleSignal::Overloaded)
        );
    }

    #[test]
    fn empty_completion_is_throttle_only_under_load() {
        // A lone empty on a healthy provider is not a throttle; retry and
        // capability handling own that case.
        assert_eq!(
            ThrottleSignal::classify(None, "", true, false),
            None,
            "empty on healthy provider must not be a throttle signal"
        );
        // The same empty while the provider is already throttled is a
        // soft-throttle signal.
        assert_eq!(
            ThrottleSignal::classify(None, "", true, true),
            Some(ThrottleSignal::EmptyUnderLoad)
        );
    }

    #[test]
    fn clean_success_has_no_signal() {
        assert_eq!(
            ThrottleSignal::classify(Some(200), "ok", false, false),
            None
        );
    }

    // --- Config resolution ---------------------------------------------

    #[test]
    fn default_limits_are_conservative() {
        let l = ResolvedLimits::default();
        assert_eq!(l.max_concurrency, DEFAULT_MAX_CONCURRENCY);
        assert_eq!(l.min_concurrency, DEFAULT_MIN_CONCURRENCY);
        assert!(l.adaptive);
    }

    #[test]
    fn catalog_overlay_clamps_min_below_max() {
        let row = ProviderLimits {
            max_concurrency: Some(4),
            min_concurrency: Some(10), // nonsense: min > max
            ..Default::default()
        };
        let l = ResolvedLimits::from_catalog(Some(&row));
        assert_eq!(l.max_concurrency, 4);
        assert_eq!(l.min_concurrency, 4, "min clamped to max");
    }

    // --- AIMD math ------------------------------------------------------

    #[test]
    fn aimd_additive_increase_toward_max() {
        let mut g = ProviderGovernor::new(limits(4, 1));
        // Force the starting limit down so we can watch it climb.
        g.concurrency_limit = 1;
        // SUCCESSES_PER_INCREASE serves → +1.
        for _ in 0..SUCCESSES_PER_INCREASE {
            g.in_flight = 1;
            g.release_success(0);
        }
        assert_eq!(g.concurrency_limit, 2);
        // Keep going; never exceeds max.
        for _ in 0..(SUCCESSES_PER_INCREASE * 20) {
            g.in_flight = 1;
            g.release_success(0);
        }
        assert_eq!(g.concurrency_limit, 4, "capped at max_concurrency");
    }

    #[test]
    fn aimd_multiplicative_decrease_halves_to_floor() {
        let mut g = ProviderGovernor::new(limits(16, 2));
        assert_eq!(g.concurrency_limit, 16);
        g.in_flight = 1;
        g.release_throttle(0, ThrottleSignal::RateLimit429, None);
        assert_eq!(g.concurrency_limit, 8);
        g.in_flight = 1;
        g.release_throttle(0, ThrottleSignal::RateLimit429, None);
        assert_eq!(g.concurrency_limit, 4);
        g.in_flight = 1;
        g.release_throttle(0, ThrottleSignal::RateLimit429, None);
        assert_eq!(g.concurrency_limit, 2, "floored at min");
        g.in_flight = 1;
        g.release_throttle(0, ThrottleSignal::RateLimit429, None);
        assert_eq!(g.concurrency_limit, 2, "never below min");
    }

    #[test]
    fn non_adaptive_pins_limit_at_max() {
        let mut l = limits(8, 1);
        l.adaptive = false;
        let mut g = ProviderGovernor::new(l);
        g.in_flight = 1;
        g.release_throttle(0, ThrottleSignal::RateLimit429, None);
        assert_eq!(g.concurrency_limit, 8, "fixed semaphore when non-adaptive");
    }

    // --- Circuit breaker state machine ---------------------------------

    #[test]
    fn circuit_opens_after_throttle_streak() {
        let mut g = ProviderGovernor::new(limits(8, 1));
        assert_eq!(g.circuit, CircuitState::Closed);
        for _ in 0..THROTTLE_STREAK_TO_OPEN {
            g.in_flight = 1;
            g.release_throttle(1_000, ThrottleSignal::RateLimit429, None);
        }
        assert!(matches!(g.circuit, CircuitState::Open { .. }));
    }

    #[test]
    fn circuit_honors_retry_after_floor() {
        let mut g = ProviderGovernor::new(limits(8, 1));
        for _ in 0..THROTTLE_STREAK_TO_OPEN {
            g.in_flight = 1;
            g.release_throttle(1_000, ThrottleSignal::RateLimit429, Some(30_000));
        }
        match g.circuit {
            CircuitState::Open { until_ms } => {
                assert!(
                    until_ms >= 1_000 + 30_000,
                    "OPEN window must not wake before Retry-After"
                );
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn half_open_probe_success_closes_circuit() {
        let mut g = ProviderGovernor::new(limits(8, 1));
        // Open the circuit.
        for _ in 0..THROTTLE_STREAK_TO_OPEN {
            g.in_flight = 1;
            g.release_throttle(1_000, ThrottleSignal::RateLimit429, Some(5_000));
        }
        // Before the window elapses: still OPEN, no probe admitted.
        assert!(matches!(g.decide(2_000, 0), GateOutcome::CircuitOpen(_)));
        // After the window: HALF-OPEN admits exactly one probe.
        let now = 1_000 + 5_000 + 1;
        assert_eq!(g.decide(now, 0), GateOutcome::Proceed);
        assert_eq!(g.circuit, CircuitState::HalfOpen);
        // A concurrent second call while the probe is in flight waits.
        assert!(matches!(g.decide(now, 0), GateOutcome::Wait(_)));
        // The probe serves → circuit CLOSES and backoff resets.
        g.release_success(now);
        assert_eq!(g.circuit, CircuitState::Closed);
        assert_eq!(g.open_cycles, 0);
    }

    #[test]
    fn half_open_probe_throttle_reopens_with_grown_backoff() {
        let mut g = ProviderGovernor::new(limits(8, 1));
        for _ in 0..THROTTLE_STREAK_TO_OPEN {
            g.in_flight = 1;
            g.release_throttle(1_000, ThrottleSignal::RateLimit429, None);
        }
        let first_open = match g.circuit {
            CircuitState::Open { until_ms } => until_ms,
            other => panic!("expected Open, got {other:?}"),
        };
        // Elapse and admit the probe.
        let now = first_open as u128 + 1;
        assert_eq!(g.decide(now, 0), GateOutcome::Proceed);
        // Probe throttles → re-open, and the window must have grown (exp backoff).
        g.release_throttle(now, ThrottleSignal::RateLimit429, None);
        match g.circuit {
            CircuitState::Open { until_ms } => {
                let first_window = first_open - 1_000;
                let second_window = until_ms - now;
                assert!(
                    second_window >= first_window,
                    "exp backoff must not shrink: {second_window} >= {first_window}"
                );
            }
            other => panic!("expected re-Open, got {other:?}"),
        }
    }

    #[test]
    fn backoff_grows_exponentially_up_to_max() {
        let mut l = limits(8, 1);
        l.backoff_base_ms = 1_000;
        l.backoff_max_ms = 8_000;
        l.backoff_multiplier = 2.0;
        l.backoff_jitter = false; // deterministic for the assertion
        let mut g = ProviderGovernor::new(l);
        g.open_cycles = 1;
        assert_eq!(g.backoff_window_ms(None), 1_000);
        g.open_cycles = 2;
        assert_eq!(g.backoff_window_ms(None), 2_000);
        g.open_cycles = 3;
        assert_eq!(g.backoff_window_ms(None), 4_000);
        g.open_cycles = 4;
        assert_eq!(g.backoff_window_ms(None), 8_000);
        g.open_cycles = 5;
        assert_eq!(g.backoff_window_ms(None), 8_000, "capped at max");
    }

    #[test]
    fn full_jitter_stays_within_window() {
        let mut l = limits(8, 1);
        l.backoff_base_ms = 10_000;
        l.backoff_max_ms = 10_000;
        l.backoff_jitter = true;
        let mut g = ProviderGovernor::new(l);
        g.open_cycles = 1;
        for _ in 0..1_000 {
            let w = g.backoff_window_ms(None);
            assert!((5_000..=10_000).contains(&w), "jitter out of band: {w}");
        }
    }

    // --- Token buckets --------------------------------------------------

    #[test]
    fn tpm_bucket_blocks_over_limit_then_recovers() {
        let mut b = TokenBucket::new(100);
        // First 60 fits (used 0 → 60).
        assert_eq!(b.wait_for(0, 60), None);
        b.record(0, 60);
        // A second 60 would be 120 > 100 → must wait.
        assert!(
            b.wait_for(0, 60).is_some(),
            "second charge exceeds the window and must wait"
        );
        // A smaller charge that still fits proceeds (60 + 40 = 100).
        assert_eq!(b.wait_for(0, 40), None);
        b.record(0, 40);
        // Now the window is full; anything more waits.
        assert!(b.wait_for(0, 1).is_some());
        // After the window slides past both entries, it recovers.
        assert_eq!(b.wait_for(60_001, 60), None);
    }

    #[test]
    fn rpm_bucket_limits_requests_per_window() {
        let mut b = TokenBucket::new(2);
        assert_eq!(b.wait_for(0, 1), None);
        b.record(0, 1);
        assert_eq!(b.wait_for(0, 1), None);
        b.record(0, 1);
        assert!(b.wait_for(0, 1).is_some(), "3rd request in window waits");
    }

    // --- Registry / gating integration ---------------------------------

    #[test]
    fn gate_no_op_when_flag_off() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var(RATE_GOVERNOR_ENABLED_ENV).ok();
        std::env::remove_var(RATE_GOVERNOR_ENABLED_ENV);
        reset_for_tests();
        // Flag off → gate is a bare Proceed and record_outcome a no-op; the
        // route's governor is never created, so snapshot stays None.
        assert_eq!(gate("anthropic", "default", 0), GateOutcome::Proceed);
        record_outcome("anthropic", "default", GovernorOutcome::Served);
        assert!(snapshot("anthropic", "default").is_none());
        if let Some(v) = prev {
            std::env::set_var(RATE_GOVERNOR_ENABLED_ENV, v);
        }
    }

    #[test]
    fn org_key_id_fingerprints_without_leaking_secret() {
        let id = org_key_id("sk-ant-super-secret-1234567890");
        assert!(!id.contains("secret"));
        assert!(!id.contains("sk-ant"));
        assert_eq!(org_key_id(""), "default");
        // Distinct keys → distinct ids; same key → stable id.
        assert_ne!(org_key_id("key-a"), org_key_id("key-b"));
        assert_eq!(org_key_id("key-a"), org_key_id("key-a"));
    }

    #[test]
    fn throttle_record_carries_structured_fields() {
        let rec = build_throttle_record(
            "anthropic",
            "kdeadbeef",
            ThrottleSignal::RateLimit429,
            Some(429),
            Some(30_000),
            "1.234".to_string(),
        );
        assert_eq!(rec["type"], "provider_throttle");
        assert_eq!(rec["provider"], "anthropic");
        assert_eq!(rec["org_key_id"], "kdeadbeef");
        assert_eq!(rec["signal_type"], "rate_limit_429");
        assert_eq!(rec["http_status"], 429);
        assert_eq!(rec["retry_after_ms"], 30_000);
    }

    // --- Mechanism-fitness: backs off, recovers, and does NOT starve a
    //     healthy provider. Runs under a mock clock so it is deterministic. ---

    #[test]
    fn mechanism_fitness_backoff_recover_without_starving_healthy() {
        let _env = GovernorEnabledGuard::on();
        let _clock = install_override(MockClock::at_wall_ms(0));

        let bad = ("anthropic", "orgA");
        let good = ("openai", "orgB");

        // 1) The healthy provider serves freely: repeated gate→served keeps its
        //    circuit CLOSED and lets concurrency climb.
        for _ in 0..20 {
            assert_eq!(gate(good.0, good.1, 0), GateOutcome::Proceed);
            record_outcome(good.0, good.1, GovernorOutcome::Served);
        }
        let good_snap = snapshot(good.0, good.1).expect("healthy governor exists");
        assert_eq!(good_snap.circuit_state, "closed");
        assert!(
            good_snap.concurrency_limit >= good_snap.min_concurrency,
            "healthy provider not starved"
        );
        assert!(!circuit_is_open(good.0, good.1));

        // 2) The bad provider gets a throttle streak → concurrency halves and
        //    the circuit OPENs. Further gates report CircuitOpen so callers can
        //    fail fast or route around the throttle.
        for _ in 0..THROTTLE_STREAK_TO_OPEN {
            assert_eq!(gate(bad.0, bad.1, 0), GateOutcome::Proceed);
            record_outcome(
                bad.0,
                bad.1,
                GovernorOutcome::Throttled {
                    signal: ThrottleSignal::RateLimit429,
                    retry_after_ms: Some(5_000),
                },
            );
        }
        let bad_snap = snapshot(bad.0, bad.1).expect("bad governor exists");
        assert_eq!(bad_snap.circuit_state, "open");
        assert!(
            bad_snap.concurrency_limit < bad_snap.max_concurrency,
            "AIMD decreased concurrency on the throttled provider"
        );
        assert!(
            circuit_is_open(bad.0, bad.1),
            "auto-response seam sees OPEN"
        );
        assert!(matches!(gate(bad.0, bad.1, 0), GateOutcome::CircuitOpen(_)));

        // 3) The healthy provider is UNAFFECTED while the bad one is open.
        assert_eq!(gate(good.0, good.1, 0), GateOutcome::Proceed);
        record_outcome(good.0, good.1, GovernorOutcome::Served);
        assert!(!circuit_is_open(good.0, good.1));

        // 4) Time passes the Retry-After window → HALF-OPEN probe → serve →
        //    CLOSED. The governor RECOVERS.
        crate::clock_mock::advance(Duration::from_secs(6));
        assert_eq!(
            gate(bad.0, bad.1, 0),
            GateOutcome::Proceed,
            "probe admitted"
        );
        record_outcome(bad.0, bad.1, GovernorOutcome::Served);
        let recovered = snapshot(bad.0, bad.1).expect("bad governor exists");
        assert_eq!(recovered.circuit_state, "closed", "recovered to CLOSED");
        assert!(!circuit_is_open(bad.0, bad.1));
    }

    #[test]
    fn mechanism_fitness_no_throttle_path_unthrottled() {
        // A healthy run never sees Wait/CircuitOpen: every gate is Proceed and
        // the circuit stays CLOSED, so throughput is unaffected.
        let _env = GovernorEnabledGuard::on();
        let _clock = install_override(MockClock::at_wall_ms(0));
        for _ in 0..100 {
            assert_eq!(gate("openai", "org", 0), GateOutcome::Proceed);
            record_outcome("openai", "org", GovernorOutcome::Served);
        }
        let snap = snapshot("openai", "org").unwrap();
        assert_eq!(snap.circuit_state, "closed");
        assert_eq!(snap.in_flight, 0);
    }
}
