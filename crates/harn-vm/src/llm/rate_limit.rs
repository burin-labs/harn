//! Route-aware sliding-window rate limiter for outbound LLM requests.
//!
//! Proactively throttles requests to stay within configured per-minute request
//! and token limits. When a bucket is full, `acquire_permit_for_llm_call`
//! yields to the Harn mockable clock, allowing other spawn_local tasks and
//! parallel pipelines to run.
//!
//! Configuration sources (later overrides earlier):
//! 1. provider/model catalog `rate_limits` fields and legacy provider `rpm`
//! 2. environment variables such as `HARN_RATE_LIMIT_<PROVIDER>_TPM=1000000`
//! 3. runtime `llm_rate_limit("provider", {rpm: N, tpm: M})`
//!
//! Request/token buckets are durable across processes by default when a route
//! has rate limits. `HARN_LLM_RATE_LIMIT_STATE_PATH` overrides the shared DB
//! path and `HARN_LLM_RATE_LIMIT_DURABLE=0` disables the durable layer for
//! debugging or constrained embeddings.
//!
//! Each durable acquire's backoff is bounded (default-ON) by
//! `HARN_LLM_RATE_LIMIT_MAX_WAIT_MS` (default 30000) so a single sustained-quota
//! provider cannot make every call sleep out a full window and dominate the
//! wall clock; on hitting the cap the call proceeds and a real 429 drives the
//! Retry-After / retry / escalation path. Set it to `0` to restore the old
//! unbounded wait.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod durable_admission;
mod provider_quota;
mod sliding_window;

pub(crate) use provider_quota::observe_for_llm_call as observe_provider_quota_for_llm_call;
use provider_quota::{ObservedTokenQuota, ProviderTokenQuotaReceipt};
use sliding_window::SlidingWindow;

const DURABLE_RATE_LIMIT_ENABLED_ENV: &str = "HARN_LLM_RATE_LIMIT_DURABLE";
const DURABLE_RATE_LIMIT_STATE_PATH_ENV: &str = "HARN_LLM_RATE_LIMIT_STATE_PATH";
/// Per-acquire ceiling on durable rate-limit backoff. Without it, a single
/// rate-limited provider (e.g. Cerebras returning a sustained per-minute quota
/// wait) can make EVERY call in a trial sleep out a full ~60s window, so the
/// durable backoff balloons to dominate the wall clock. Measured 2026-06-21 on a
/// `cerebras:gpt-oss-120b` zig-feat trial: 75 durable waits summing to 1622s =
/// ~89% of the 1812s trial wall. Capping each acquire lets the call attempt the
/// provider anyway after the cap; a genuine 429 then feeds the normal
/// Retry-After / retry / escalation path instead of being absorbed as a silent
/// unbounded sleep. The clamp is on the WAIT, not the limit: we never exceed the
/// provider's quota, we just refuse to block forever on one route.
const DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV: &str = "HARN_LLM_RATE_LIMIT_MAX_WAIT_MS";
/// Default per-acquire durable backoff ceiling (ms). Default-ON correctness fix.
/// 30s is below a single 60s sliding window: long enough to ride out an ordinary
/// in-window burst, short enough that a sustained-quota route cannot serialize
/// the whole trial behind it. Set the env to `0` to restore the old unbounded
/// behavior (debugging only).
const DURABLE_RATE_LIMIT_MAX_WAIT_MS_DEFAULT: u64 = 30_000;
const WINDOW_SECS: u64 = 60;
const FAIR_QUEUE_STARVATION_MS: u64 = 60_000;
const RATE_LIMIT_ENV_FIELD_SUFFIXES: [&str; 5] =
    ["_RPM", "_TPM", "_INPUT_TPM", "_OUTPUT_TPM", "_CONCURRENCY"];

/// Consecutive NetworkError/Timeout (or provider-overload 529/503) failures on
/// one route that trip the circuit breaker open. Distinct from 429 handling,
/// which uses `cooldown_until_ms` + provider Retry-After and never feeds the
/// breaker.
const NETWORK_BREAKER_FAILURE_THRESHOLD: u32 = 4;
/// Base network breaker open window before allowing a half-open probe.
/// The first trip stays short so a laptop reconnect or DNS recovery is probed
/// soon; repeated trips grow through [`NETWORK_BREAKER_OPEN_MS_MAX`] so a
/// sustained provider/network storm does not burn one serialized run per 5s.
const NETWORK_BREAKER_OPEN_MS: u64 = 5_000;
const NETWORK_BREAKER_OPEN_MS_SECOND: u64 = 30_000;
const NETWORK_BREAKER_OPEN_MS_MAX: u64 = 120_000;
/// Consecutive terminal *unproductive completions* (a zero-token empty
/// completion, or a billed-noncommittal turn — the provider served but
/// committed no content, reasoning, or tool call) on one route that trip the
/// breaker OPEN. Small on purpose: a route that empties this many times in a
/// row is not usefully serving, and continuing to dispatch just burns the
/// escalation/judge budget one dead turn at a time. This is the always-on,
/// provider-general backstop for the empty-completion storm that harn#4023's
/// failover-to-`circuit_open` cannot stop for a single-provider model with no
/// cross-provider alternate (its conversion is also gated behind the
/// default-OFF `llm.rate_governor` flag). Distinct from the network threshold:
/// an empty completion is a served-but-useless turn, not an unreachable link.
pub(super) const UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD: u32 = 3;
/// Base open window after the breaker trips on unproductive completions. It
/// grows per consecutive open (doubling, capped) so a genuinely dead escalation
/// lane is probed only a bounded handful of times over a long trial instead of
/// storming 18-43x, while a provider that recovers is retried within a window.
/// Longer than [`NETWORK_BREAKER_OPEN_MS`]: an empty-completing model rarely
/// self-heals in seconds, and each probe costs a full billed provider call.
const UNPRODUCTIVE_BREAKER_OPEN_MS: u64 = 30_000;
/// Ceiling for the (doubling) unproductive open window.
const UNPRODUCTIVE_BREAKER_OPEN_MS_MAX: u64 = 240_000;
/// Default shared-cooldown window recorded on a provider-overload failure
/// (HTTP 529/503, Anthropic `overloaded_error`) when the response carried no
/// Retry-After header — overload responses rarely do. Recording it in the
/// route limiter makes N parallel agents back off together instead of
/// stampeding a provider that is already shedding load. Kept as short as the
/// breaker window: overload recovers on the provider's schedule and we only
/// need to break the herd, not idle the route.
pub(crate) const OVERLOAD_COOLDOWN_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RateLimitRequest {
    input_tokens: u64,
    output_tokens: u64,
}

impl RateLimitRequest {
    /// Charge GROSS projected tokens against TPM — this is intentional, not a
    /// bug, and must not be "optimized" to net out prompt-cached tokens.
    ///
    /// `projected_input_tokens` is the whole prompt (system + every message +
    /// tools) with no subtraction for prompt-cached prefixes. A reasonable
    /// instinct is that re-sending a cached transcript prefix shouldn't count
    /// against the per-minute token budget. It does: provider TPM enforcement
    /// is on GROSS prompt tokens regardless of cache hits. Verified live
    /// (2026-06-12) against Cerebras gpt-oss-120b — with 6400/6482 prompt
    /// tokens served from cache (usage.prompt_tokens_details.cached_tokens),
    /// the x-ratelimit-remaining-tokens-minute header still decremented by the
    /// full ~6480 gross prompt tokens. Caching reduces BILLED cost (see
    /// cost.rs, which does net out cache_read_tokens for dollars), not the rate
    /// limit. Netting cached tokens out here would make the proactive limiter
    /// UNDER-throttle and provoke provider 429s. The lever for cache-heavy,
    /// growing-transcript workloads is footprint reduction (compaction / fewer
    /// turns), not limiter accounting.
    fn for_llm_call(opts: &super::api::LlmCallOptions) -> Self {
        let projection = super::cost::project_llm_call_cost(opts, 0.0);
        Self {
            input_tokens: projection.projected_input_tokens.max(0) as u64,
            output_tokens: projection.projected_output_tokens.max(0) as u64,
        }
    }

    fn total_tokens(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EffectiveRateLimits {
    rpm: Option<u32>,
    tpm: Option<u64>,
    input_tpm: Option<u64>,
    output_tpm: Option<u64>,
    concurrency: Option<u32>,
}

impl EffectiveRateLimits {
    fn from_catalog(mut limits: crate::llm_config::RateLimitsDef) -> Option<Self> {
        if limits.is_empty() {
            return None;
        }
        let out = Self {
            rpm: limits.rpm.take(),
            tpm: limits.tpm.take(),
            input_tpm: limits.input_tpm.take(),
            output_tpm: limits.output_tpm.take(),
            concurrency: limits.concurrency.take(),
        };
        (!out.is_empty()).then_some(out)
    }

    fn is_empty(&self) -> bool {
        self.rpm.is_none()
            && self.tpm.is_none()
            && self.input_tpm.is_none()
            && self.output_tpm.is_none()
            && self.concurrency.is_none()
    }

    fn to_catalog(&self) -> crate::llm_config::RateLimitsDef {
        crate::llm_config::RateLimitsDef {
            rpm: self.rpm,
            tpm: self.tpm,
            input_tpm: self.input_tpm,
            output_tpm: self.output_tpm,
            concurrency: self.concurrency,
            ..Default::default()
        }
    }
}

/// Per-process circuit breaker for one route.
///
/// Opens on sustained `NetworkError`/`Timeout` (laptop disconnect, DNS
/// failure, dropped link) and on sustained provider overload (HTTP 529/503 /
/// `overloaded_error` — the provider is shedding load, so continuing to call
/// it only deepens the overload) — never on 429, which the rate limiter
/// already handles via `cooldown_until_ms` + provider Retry-After, and never
/// on generic 500/502 (a single-request fault on a healthy link). While open
/// it fails fast so a call does not burn its whole retry budget against a
/// dead link or an overloaded provider; after a short window it half-opens to
/// admit a single probe, then closes on success or re-opens on another
/// qualifying failure.
///
/// Also opens on sustained terminal *unproductive completions* (a served turn
/// that delivered no content, reasoning, or tool call). A single-provider model
/// with no cross-provider failover alternate cannot be rescued by routing, so
/// once it empties [`UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD`] times in a row
/// on a route the breaker fails fast (surfaced as `circuit_open`, so the agent
/// loop degrades onto the primary/cheap result and the judge defers) instead of
/// re-dispatching the dead lane every turn.
///
/// Network reachability is a property of THIS process, so the breaker is
/// per-process state (not shared via the durable rate-limit DB). It is distinct
/// from the opt-in routing-policy breaker; this one is always-on.
#[derive(Debug, Default, PartialEq, Eq)]
enum BreakerState {
    #[default]
    Closed,
    /// Failing fast until `until_ms`, after which one half-open probe is admitted.
    Open { until_ms: u128 },
    /// A single probe is in flight; further calls fail fast until it resolves.
    HalfOpen,
}

/// Why the breaker is currently OPEN, so the fail-fast error carries the right
/// category: an unreachable link is `transient_network`, whereas a route that
/// keeps serving empty completions is `circuit_open` (failover-eligible where a
/// chain exists; degraded-onto-primary by the agent loop where it is not).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum BreakerOpenReason {
    #[default]
    Network,
    UnproductiveCompletion,
}

#[derive(Debug, Default)]
struct NetworkBreaker {
    state: BreakerState,
    consecutive_network_failures: u32,
    /// How many times the breaker has opened for network failures without an
    /// intervening successful serve. Drives storm-scale open windows.
    network_opens: u32,
    /// Consecutive terminal unproductive completions on this route. Reset by
    /// any productive serve; independent of the network-failure streak.
    consecutive_unproductive_completions: u32,
    /// How many times the breaker has opened for unproductive completions
    /// without an intervening productive serve. Drives the doubling open window
    /// so a persistently dead lane is probed a bounded number of times.
    unproductive_opens: u32,
    /// Reason the breaker last opened (drives the fail-fast error category).
    open_reason: BreakerOpenReason,
}

impl NetworkBreaker {
    /// Whether a call should be admitted now, transitioning Open→HalfOpen when
    /// the open window has elapsed. Returns `None` to admit (Closed/HalfOpen
    /// probe), or `Some((remaining, reason))` to fail fast while open.
    fn admit(&mut self, now_ms: u128) -> Option<(Duration, BreakerOpenReason)> {
        let blocked = self.blocked(now_ms);
        if blocked.is_none() && matches!(self.state, BreakerState::Open { .. }) {
            self.state = BreakerState::HalfOpen;
        }
        blocked
    }

    fn blocked(&self, now_ms: u128) -> Option<(Duration, BreakerOpenReason)> {
        match self.state {
            BreakerState::Closed => None,
            BreakerState::HalfOpen => {
                // A probe is already in flight; do not admit a second.
                Some((Duration::from_millis(0), self.open_reason))
            }
            BreakerState::Open { until_ms } => {
                if now_ms >= until_ms {
                    None
                } else {
                    Some((
                        Duration::from_millis(
                            until_ms.saturating_sub(now_ms).min(u128::from(u64::MAX)) as u64,
                        ),
                        self.open_reason,
                    ))
                }
            }
        }
    }

    fn record_network_failure(&mut self, now_ms: u128) {
        self.consecutive_network_failures = self.consecutive_network_failures.saturating_add(1);
        // A failed half-open probe (or crossing the threshold while closed)
        // (re)opens the breaker for a fresh window.
        if matches!(self.state, BreakerState::HalfOpen)
            || self.consecutive_network_failures >= NETWORK_BREAKER_FAILURE_THRESHOLD
        {
            self.network_opens = self.network_opens.saturating_add(1);
            let window = match self.network_opens {
                0 | 1 => NETWORK_BREAKER_OPEN_MS,
                2 => NETWORK_BREAKER_OPEN_MS_SECOND,
                _ => NETWORK_BREAKER_OPEN_MS_MAX,
            };
            self.open_reason = BreakerOpenReason::Network;
            self.state = BreakerState::Open {
                until_ms: now_ms.saturating_add(u128::from(window)),
            };
        }
    }

    /// Record a terminal unproductive completion (empty / billed-noncommittal).
    /// Trips the breaker OPEN on a failed half-open probe or once the streak
    /// crosses [`UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD`], with a doubling,
    /// capped open window so a dead lane is probed only a bounded few times.
    fn record_unproductive_completion(&mut self, now_ms: u128) -> Option<u64> {
        let was_open = matches!(self.state, BreakerState::Open { .. });
        self.consecutive_unproductive_completions =
            self.consecutive_unproductive_completions.saturating_add(1);
        if matches!(self.state, BreakerState::HalfOpen)
            || self.consecutive_unproductive_completions
                >= UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD
        {
            self.unproductive_opens = self.unproductive_opens.saturating_add(1);
            let shift = self.unproductive_opens.saturating_sub(1).min(3);
            let window = UNPRODUCTIVE_BREAKER_OPEN_MS
                .saturating_mul(1u64 << shift)
                .min(UNPRODUCTIVE_BREAKER_OPEN_MS_MAX);
            self.open_reason = BreakerOpenReason::UnproductiveCompletion;
            self.state = BreakerState::Open {
                until_ms: now_ms.saturating_add(u128::from(window)),
            };
            if !was_open {
                return Some(window);
            }
        }
        None
    }

    fn record_success(&mut self) {
        self.consecutive_network_failures = 0;
        self.network_opens = 0;
        self.consecutive_unproductive_completions = 0;
        self.unproductive_opens = 0;
        self.open_reason = BreakerOpenReason::Network;
        self.state = BreakerState::Closed;
    }
}

struct RouteLimiter {
    request_window: Option<SlidingWindow>,
    total_token_window: Option<SlidingWindow>,
    input_token_window: Option<SlidingWindow>,
    output_token_window: Option<SlidingWindow>,
    observed_token_quota: Option<ObservedTokenQuota>,
    concurrency: Option<Arc<Semaphore>>,
    cooldown_until_ms: Option<u128>,
    breaker: NetworkBreaker,
    limits: EffectiveRateLimits,
}

impl RouteLimiter {
    fn new(limits: EffectiveRateLimits) -> Self {
        Self {
            request_window: limits.rpm.map(|rpm| SlidingWindow::new(rpm as u64)),
            total_token_window: limits.tpm.map(SlidingWindow::new),
            input_token_window: limits.input_tpm.map(SlidingWindow::new),
            output_token_window: limits.output_tpm.map(SlidingWindow::new),
            observed_token_quota: None,
            concurrency: limits
                .concurrency
                .map(|limit| Arc::new(Semaphore::new(limit.max(1) as usize))),
            cooldown_until_ms: None,
            breaker: NetworkBreaker::default(),
            limits,
        }
    }

    fn check(&mut self, now_ms: u128, request: RateLimitRequest) -> Option<Duration> {
        let waits = [
            self.request_window
                .as_mut()
                .and_then(|window| window.check(now_ms, 1)),
            self.total_token_window
                .as_mut()
                .and_then(|window| window.check(now_ms, request.total_tokens())),
            self.input_token_window
                .as_mut()
                .and_then(|window| window.check(now_ms, request.input_tokens)),
            self.output_token_window
                .as_mut()
                .and_then(|window| window.check(now_ms, request.output_tokens)),
            self.observed_token_quota
                .and_then(|quota| quota.check(now_ms, request.total_tokens())),
            self.cooldown_until_ms
                .filter(|until_ms| *until_ms > now_ms)
                .map(|until_ms| {
                    Duration::from_millis(
                        until_ms.saturating_sub(now_ms).min(u128::from(u64::MAX)) as u64
                    )
                }),
        ];
        waits.into_iter().flatten().max()
    }

    fn record(&mut self, now_ms: u128, request: RateLimitRequest) {
        if let Some(window) = self.request_window.as_mut() {
            window.record(now_ms, 1);
        }
        if let Some(window) = self.total_token_window.as_mut() {
            window.record(now_ms, request.total_tokens());
        }
        if let Some(window) = self.input_token_window.as_mut() {
            window.record(now_ms, request.input_tokens);
        }
        if let Some(window) = self.output_token_window.as_mut() {
            window.record(now_ms, request.output_tokens);
        }
        if let Some(quota) = self.observed_token_quota.as_mut() {
            quota.record(now_ms, request.total_tokens());
        }
    }

    fn record_observed_token_quota(&mut self, now_ms: u128, request: RateLimitRequest) {
        if let Some(quota) = self.observed_token_quota.as_mut() {
            quota.record(now_ms, request.total_tokens());
        }
    }

    fn observe_token_quota(&mut self, now_ms: u128, receipt: ProviderTokenQuotaReceipt) {
        self.observed_token_quota = Some(ObservedTokenQuota::from_receipt(now_ms, receipt));
    }

    fn observe_retry_after(&mut self, now_ms: u128, retry_after_ms: u64) {
        if retry_after_ms == 0 {
            return;
        }
        let until_ms = now_ms.saturating_add(u128::from(retry_after_ms));
        self.cooldown_until_ms = Some(self.cooldown_until_ms.unwrap_or(0).max(until_ms));
    }

    /// Fail-fast wait + reason if the route breaker is open; `None` admits.
    fn breaker_block(&mut self, now_ms: u128) -> Option<(Duration, BreakerOpenReason)> {
        self.breaker.admit(now_ms)
    }

    fn observe_network_failure(&mut self, now_ms: u128) {
        self.breaker.record_network_failure(now_ms);
    }

    fn observe_unproductive_completion(&mut self, now_ms: u128) -> Option<u64> {
        self.breaker.record_unproductive_completion(now_ms)
    }

    fn observe_success(&mut self) {
        self.breaker.record_success();
    }
}

#[derive(Default)]
struct RateLimitRegistry {
    initialized_from_config: bool,
    limiters: HashMap<String, RouteLimiter>,
}

static LIMITERS: OnceLock<Mutex<RateLimitRegistry>> = OnceLock::new();
static RUNTIME_OVERRIDES: OnceLock<Mutex<HashMap<String, EffectiveRateLimits>>> = OnceLock::new();

/// Holds in-flight concurrency permits for the duration of one provider call.
pub(crate) struct RateLimitPermit {
    _permits: Vec<OwnedSemaphorePermit>,
}

fn registry() -> &'static Mutex<RateLimitRegistry> {
    LIMITERS.get_or_init(|| Mutex::new(RateLimitRegistry::default()))
}

fn runtime_overrides() -> &'static Mutex<HashMap<String, EffectiveRateLimits>> {
    RUNTIME_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn provider_key(provider: &str) -> String {
    format!("provider:{}", provider.trim().to_ascii_lowercase())
}

fn model_key(provider: &str, model: &str) -> String {
    format!(
        "model:{}:{}",
        provider.trim().to_ascii_lowercase(),
        crate::llm_config::normalize_model_id(model.trim())
    )
}

fn limiter_keys(provider: &str, model: &str) -> Vec<String> {
    let provider = provider.trim();
    if provider.is_empty() {
        return Vec::new();
    }
    let mut keys = vec![provider_key(provider)];
    let model = model.trim();
    if !model.is_empty() {
        keys.push(model_key(provider, model));
    }
    keys
}

fn env_key_fragment(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn set_from_env_u32(slot: &mut Option<u32>, key: &str) -> bool {
    let Ok(raw) = std::env::var(key) else {
        return false;
    };
    match raw.trim().parse::<i64>() {
        Ok(value) if value > 0 && value <= i64::from(u32::MAX) => *slot = Some(value as u32),
        Ok(value) if value <= 0 => *slot = None,
        Ok(_) => {}
        Err(_) => {}
    }
    true
}

fn set_from_env_u64(slot: &mut Option<u64>, key: &str) -> bool {
    let Ok(raw) = std::env::var(key) else {
        return false;
    };
    match raw.trim().parse::<i64>() {
        Ok(value) if value > 0 => *slot = Some(value as u64),
        Ok(_) => *slot = None,
        Err(_) => {}
    }
    true
}

fn apply_env_overrides(prefix: &str, limits: &mut EffectiveRateLimits) -> bool {
    let mut changed = false;
    changed |= set_from_env_u32(&mut limits.rpm, &format!("{prefix}_RPM"));
    changed |= set_from_env_u64(&mut limits.tpm, &format!("{prefix}_TPM"));
    changed |= set_from_env_u64(&mut limits.input_tpm, &format!("{prefix}_INPUT_TPM"));
    changed |= set_from_env_u64(&mut limits.output_tpm, &format!("{prefix}_OUTPUT_TPM"));
    changed |= set_from_env_u32(&mut limits.concurrency, &format!("{prefix}_CONCURRENCY"));
    changed
}

fn insert_limiter(
    limiters: &mut HashMap<String, RouteLimiter>,
    key: String,
    limits: EffectiveRateLimits,
) {
    if limits.is_empty() {
        limiters.remove(&key);
    } else {
        limiters.insert(key, RouteLimiter::new(limits));
    }
}

fn limiter_for_key<'a>(
    limiters: &'a mut HashMap<String, RouteLimiter>,
    key: &str,
) -> &'a mut RouteLimiter {
    limiters
        .entry(key.to_string())
        .or_insert_with(|| RouteLimiter::new(EffectiveRateLimits::default()))
}

fn provider_limits_from_config(
    provider: &crate::llm_config::ProviderDef,
) -> Option<EffectiveRateLimits> {
    let limits = provider
        .rate_limits
        .clone()
        .unwrap_or_default()
        .with_rpm_fallback(provider.rpm)?;
    EffectiveRateLimits::from_catalog(limits)
}

fn model_limits_from_config(model: &crate::llm_config::ModelDef) -> Option<EffectiveRateLimits> {
    model
        .rate_limits
        .clone()
        .and_then(EffectiveRateLimits::from_catalog)
}

fn install_legacy_env_provider_overrides(limiters: &mut HashMap<String, RouteLimiter>) {
    for (key, raw) in std::env::vars() {
        let Some(fragment) = key.strip_prefix("HARN_RATE_LIMIT_") else {
            continue;
        };
        if RATE_LIMIT_ENV_FIELD_SUFFIXES
            .iter()
            .any(|suffix| fragment.ends_with(suffix))
        {
            continue;
        }
        let Ok(rpm) = raw.trim().parse::<i64>() else {
            continue;
        };
        let provider = fragment.to_ascii_lowercase();
        let key = provider_key(&provider);
        if rpm <= 0 {
            limiters.remove(&key);
        } else {
            let mut limits = limiters
                .get(&key)
                .map(|limiter| limiter.limits.clone())
                .unwrap_or_default();
            limits.rpm = Some(rpm as u32);
            insert_limiter(limiters, key, limits);
        }
    }
}

fn config_limiters_from_effective_config() -> HashMap<String, RouteLimiter> {
    let config = crate::llm_config::effective_config();
    let mut limiters = HashMap::new();
    for (name, provider) in &config.providers {
        let mut limits = provider_limits_from_config(provider).unwrap_or_default();
        apply_env_overrides(
            &format!("HARN_RATE_LIMIT_{}", env_key_fragment(name)),
            &mut limits,
        );
        insert_limiter(&mut limiters, provider_key(name), limits);
    }
    for (model_id, model) in &config.models {
        let mut limits = model_limits_from_config(model).unwrap_or_default();
        apply_env_overrides(
            &format!(
                "HARN_RATE_LIMIT_{}_{}",
                env_key_fragment(&model.provider),
                env_key_fragment(model_id)
            ),
            &mut limits,
        );
        insert_limiter(&mut limiters, model_key(&model.provider, model_id), limits);
    }
    install_legacy_env_provider_overrides(&mut limiters);
    limiters
}

fn limiters_from_config_and_runtime_overrides() -> HashMap<String, RouteLimiter> {
    let mut limiters = config_limiters_from_effective_config();
    for (provider, limits) in runtime_overrides()
        .lock()
        .expect("rate limiter runtime override mutex poisoned")
        .iter()
    {
        insert_limiter(&mut limiters, provider_key(provider), limits.clone());
    }
    limiters
}

/// Load rate limits from provider/model config and environment variables.
/// Safe to call multiple times (replaces existing config-derived entries).
#[allow(
    dead_code,
    reason = "explicit reload entry point is distinct from non-destructive lazy initialization"
)]
pub(crate) fn init_from_config() {
    let limiters = limiters_from_config_and_runtime_overrides();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    registry.limiters = limiters;
    registry.initialized_from_config = true;
}

fn ensure_initialized_from_config() {
    ensure_initialized_from_config_with(limiters_from_config_and_runtime_overrides);
}

fn ensure_initialized_from_config_with(
    build_candidate: impl FnOnce() -> HashMap<String, RouteLimiter>,
) {
    if registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .initialized_from_config
    {
        return;
    }

    let limiters = build_candidate();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    if registry.initialized_from_config {
        return;
    }
    registry.limiters = limiters;
    registry.initialized_from_config = true;
}

/// Set or update the provider rate limits at runtime.
pub(crate) fn set_rate_limits(provider: &str, limits: crate::llm_config::RateLimitsDef) {
    ensure_initialized_from_config();
    let effective = EffectiveRateLimits::from_catalog(limits).unwrap_or_default();
    runtime_overrides()
        .lock()
        .expect("rate limiter runtime override mutex poisoned")
        .insert(provider.to_ascii_lowercase(), effective.clone());
    insert_limiter(
        &mut registry()
            .lock()
            .expect("rate limiter mutex poisoned")
            .limiters,
        provider_key(provider),
        effective,
    );
}

/// Remove the runtime provider rate limit override.
pub(crate) fn clear_rate_limit(provider: &str) {
    ensure_initialized_from_config();
    runtime_overrides()
        .lock()
        .expect("rate limiter runtime override mutex poisoned")
        .remove(&provider.to_ascii_lowercase());
    registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .limiters
        .remove(&provider_key(provider));
}

/// Query the current provider RPM limit. Returns `None` if unlimited.
pub(crate) fn get_rate_limit(provider: &str) -> Option<u32> {
    get_rate_limits(provider).and_then(|limits| limits.rpm)
}

/// Query the current rich provider rate limits. Returns `None` if unlimited.
pub(crate) fn get_rate_limits(provider: &str) -> Option<crate::llm_config::RateLimitsDef> {
    ensure_initialized_from_config();
    registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .limiters
        .get(&provider_key(provider))
        .map(|limiter| limiter.limits.to_catalog())
}

fn max_wait(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

async fn acquire_concurrency(keys: &[String]) -> Vec<OwnedSemaphorePermit> {
    let semaphores = {
        let registry = registry().lock().expect("rate limiter mutex poisoned");
        keys.iter()
            .filter_map(|key| {
                registry
                    .limiters
                    .get(key)
                    .and_then(|limiter| limiter.concurrency.clone())
            })
            .collect::<Vec<_>>()
    };
    let mut permits = Vec::with_capacity(semaphores.len());
    for semaphore in semaphores {
        if let Ok(permit) = semaphore.acquire_owned().await {
            permits.push(permit);
        }
    }
    permits
}

fn check_wait_for_keys(
    registry: &mut RateLimitRegistry,
    keys: &[String],
    request: RateLimitRequest,
    now_ms: u128,
) -> Option<Duration> {
    let mut wait = None;
    for key in keys {
        if let Some(limiter) = registry.limiters.get_mut(key) {
            wait = max_wait(wait, limiter.check(now_ms, request));
        }
    }
    wait
}

fn record_for_keys(
    registry: &mut RateLimitRegistry,
    keys: &[String],
    request: RateLimitRequest,
    now_ms: u128,
) {
    for key in keys {
        if let Some(limiter) = registry.limiters.get_mut(key) {
            limiter.record(now_ms, request);
        }
    }
}

fn record_observed_quota_for_keys(
    registry: &mut RateLimitRegistry,
    keys: &[String],
    request: RateLimitRequest,
    now_ms: u128,
) {
    for key in keys {
        if let Some(limiter) = registry.limiters.get_mut(key) {
            limiter.record_observed_token_quota(now_ms, request);
        }
    }
}

fn durable_rate_limit_disabled() -> bool {
    let Ok(raw) = std::env::var(DURABLE_RATE_LIMIT_ENABLED_ENV) else {
        return false;
    };
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "none" | "disabled"
    )
}

/// Per-acquire ceiling (ms) on how long durable backoff may block one LLM call.
/// `None` means unbounded (env explicitly set to `0`); `Some(ms)` clamps the
/// wait. Generalizes across every provider and model — it is keyed on the wait,
/// not on any specific route.
fn durable_max_wait_ms() -> Option<u64> {
    let configured = match std::env::var(DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(value) => value,
            Err(_) => DURABLE_RATE_LIMIT_MAX_WAIT_MS_DEFAULT,
        },
        Err(_) => DURABLE_RATE_LIMIT_MAX_WAIT_MS_DEFAULT,
    };
    if configured == 0 {
        None
    } else {
        Some(configured)
    }
}

fn durable_state_path() -> Option<PathBuf> {
    if durable_rate_limit_disabled() {
        return None;
    }

    if let Ok(raw) = std::env::var(DURABLE_RATE_LIMIT_STATE_PATH_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            return if path.is_absolute() {
                Some(path)
            } else {
                std::env::current_dir().ok().map(|cwd| cwd.join(path))
            };
        }
    }

    let base = crate::stdlib::process::runtime_root_base();
    Some(crate::runtime_paths::state_root(&base).join("llm-rate-limits.sqlite"))
}

fn durable_bucket(
    key: &str,
    suffix: &str,
    limit: u64,
    units: u64,
) -> crate::durable_rate_limit::RateBucket {
    crate::durable_rate_limit::RateBucket::new(
        format!("llm:{key}:{suffix}"),
        limit.max(1),
        units,
        WINDOW_SECS * 1000,
    )
}

fn durable_buckets_for_keys(
    registry: &RateLimitRegistry,
    keys: &[String],
    request: RateLimitRequest,
) -> Vec<crate::durable_rate_limit::RateBucket> {
    let mut buckets = Vec::new();
    for key in keys {
        let Some(limiter) = registry.limiters.get(key) else {
            continue;
        };
        if let Some(rpm) = limiter.limits.rpm {
            buckets.push(durable_bucket(key, "rpm", u64::from(rpm), 1));
        }
        if let Some(tpm) = limiter.limits.tpm {
            buckets.push(durable_bucket(key, "tpm", tpm, request.total_tokens()));
        }
        if let Some(input_tpm) = limiter.limits.input_tpm {
            buckets.push(durable_bucket(
                key,
                "input_tpm",
                input_tpm,
                request.input_tokens,
            ));
        }
        if let Some(output_tpm) = limiter.limits.output_tpm {
            buckets.push(durable_bucket(
                key,
                "output_tpm",
                output_tpm,
                request.output_tokens,
            ));
        }
    }
    buckets
}

async fn sleep_after_throttle(provider: &str, model: &str, duration: Duration) {
    let route = if model.trim().is_empty() {
        provider.to_string()
    } else {
        format!(
            "{provider}/{}",
            crate::llm_config::normalize_model_id(model)
        )
    };
    // This is user-visible wall time and part of the canonical eval receipt,
    // not debug trivia. Keeping the existing stable sentence also lets older
    // report projections account for the wait without a parallel metric path.
    crate::events::log_info(
        "llm.rate_limit",
        &format!(
            "Rate limit for '{}': throttling for {}ms",
            route,
            duration.as_millis()
        ),
    );
    crate::clock_mock::sleep(duration).await;
}

async fn acquire_permit_for(
    provider: &str,
    model: &str,
    request: RateLimitRequest,
    consumer_id: Option<&str>,
    session_id: Option<&str>,
    reroute_on_timeout: bool,
) -> Result<RateLimitPermit, crate::value::VmError> {
    ensure_initialized_from_config();
    let keys = limiter_keys(provider, model);
    if let Some(state_path) = durable_state_path() {
        loop {
            let permits = acquire_concurrency(&keys).await;
            if let Some(duration) = {
                let mut registry = registry().lock().expect("rate limiter mutex poisoned");
                let now_ms = crate::clock_mock::instant_now().as_millis();
                check_wait_for_keys(&mut registry, &keys, request, now_ms)
            } {
                drop(permits);
                sleep_after_throttle(provider, model, duration).await;
                continue;
            }
            durable_admission::acquire(
                state_path,
                provider,
                model,
                &keys,
                request,
                consumer_id,
                session_id,
                reroute_on_timeout,
            )
            .await?;
            return Ok(provider_quota::admit_after_durable(
                provider, model, &keys, request, permits,
            )
            .await);
        }
    }
    loop {
        if let Some(duration) = {
            let mut registry = registry().lock().expect("rate limiter mutex poisoned");
            let now_ms = crate::clock_mock::instant_now().as_millis();
            check_wait_for_keys(&mut registry, &keys, request, now_ms)
        } {
            sleep_after_throttle(provider, model, duration).await;
            continue;
        }

        let permits = acquire_concurrency(&keys).await;
        if let Some(duration) = {
            let mut registry = registry().lock().expect("rate limiter mutex poisoned");
            let now_ms = crate::clock_mock::instant_now().as_millis();
            let wait = check_wait_for_keys(&mut registry, &keys, request, now_ms);
            if wait.is_none() {
                record_for_keys(&mut registry, &keys, request, now_ms);
            }
            wait
        } {
            drop(permits);
            sleep_after_throttle(provider, model, duration).await;
            continue;
        }

        return Ok(RateLimitPermit { _permits: permits });
    }
}

/// Share a provider Retry-After signal with the route limiter.
///
/// Catalog limits prevent most known quota overruns. Provider 429 responses are
/// still useful live feedback: account tier, burst windows, or remote-side
/// throttles can differ from the catalog. Recording the cooldown here lets
/// sibling and subsequent calls wait on the same route instead of stampeding
/// the provider after the first failed call.
pub(crate) fn observe_retry_after_for_llm_call(
    opts: &super::api::LlmCallOptions,
    retry_after_ms: u64,
) {
    if retry_after_ms == 0 {
        return;
    }
    ensure_initialized_from_config();
    let keys = limiter_keys(&opts.provider, &opts.model);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    for key in keys {
        limiter_for_key(&mut registry.limiters, &key).observe_retry_after(now_ms, retry_after_ms);
    }
}

/// Fail-fast error returned when the network breaker is open for a route.
fn breaker_open_error(
    provider: &str,
    model: &str,
    remaining: Duration,
    reason: BreakerOpenReason,
) -> crate::value::VmError {
    let route = if model.trim().is_empty() {
        provider.to_string()
    } else {
        format!(
            "{provider}/{}",
            crate::llm_config::normalize_model_id(model)
        )
    };
    match reason {
        BreakerOpenReason::Network => crate::value::VmError::CategorizedError {
            message: format!(
                "network circuit breaker open for '{route}': sustained network failures or \
                 provider overload; failing fast for {}ms (a half-open probe will follow)",
                remaining.as_millis()
            ),
            category: crate::value::ErrorCategory::TransientNetwork,
        },
        // Preserve the zero-dispatch fact structurally. Routing uses
        // `attempt_count` to distinguish logical route attempts from physical
        // provider requests when it builds the terminal attempt ledger.
        BreakerOpenReason::UnproductiveCompletion => {
            let message = format!(
                "circuit breaker open for '{route}': sustained empty/unproductive completions \
                 (served but delivered no content, reasoning, or tool call); failing fast for \
                 {}ms (a half-open probe will follow)",
                remaining.as_millis()
            );
            crate::value::VmError::Thrown(crate::value::VmValue::dict(
                std::collections::BTreeMap::from([
                    (
                        "category".to_string(),
                        crate::value::VmValue::String(arcstr::ArcStr::from("circuit_open")),
                    ),
                    (
                        "code".to_string(),
                        crate::value::VmValue::String(arcstr::ArcStr::from("route_quarantined")),
                    ),
                    (
                        "reason".to_string(),
                        crate::value::VmValue::String(arcstr::ArcStr::from(
                            "unproductive_completion",
                        )),
                    ),
                    ("attempt_count".to_string(), crate::value::VmValue::Int(0)),
                    (
                        "remaining_ms".to_string(),
                        crate::value::VmValue::Int(
                            remaining.as_millis().try_into().unwrap_or(i64::MAX),
                        ),
                    ),
                    (
                        "message".to_string(),
                        crate::value::VmValue::String(arcstr::ArcStr::from(message)),
                    ),
                ]),
            ))
        }
    }
}

mod network_recovery;
pub(crate) use network_recovery::await_network_breaker_for_llm_call;
#[cfg(test)]
pub(crate) use network_recovery::check_network_breaker_for_llm_call;
#[cfg(test)]
use network_recovery::network_breaker_admission;

/// Feed a terminal unproductive completion (a served turn that delivered no
/// content, reasoning, or tool call — the zero-token empty completion or the
/// billed-noncommittal contract violation) to the route's circuit breaker.
///
/// Always-on and governor-independent: unlike harn#4023's
/// throttle-gated failover-to-`circuit_open`, this backstops the storm for a
/// single-provider model (e.g. an Anthropic-only escalation target or judge)
/// that has no cross-provider alternate. Once a route empties
/// [`UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD`] times in a row the breaker
/// opens and subsequent calls fail fast instead of re-dispatching the dead
/// lane. A productive serve resets the streak.
pub(crate) fn observe_unproductive_completion_for_llm_call(
    opts: &super::api::LlmCallOptions,
    reason: &str,
) {
    ensure_initialized_from_config();
    let key = if opts.model.trim().is_empty() {
        provider_key(&opts.provider)
    } else {
        model_key(&opts.provider, &opts.model)
    };
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    let cooldown_ms =
        limiter_for_key(&mut registry.limiters, &key).observe_unproductive_completion(now_ms);
    drop(registry);
    if let Some(cooldown_ms) = cooldown_ms {
        super::append_observability_sidecar_entry(
            "route_quarantined",
            serde_json::Map::from_iter([
                (
                    "schema".to_string(),
                    serde_json::json!("harn.llm.route_quarantine.v1"),
                ),
                (
                    "receipt_kind".to_string(),
                    serde_json::json!("route_quarantined"),
                ),
                ("provider".to_string(), serde_json::json!(opts.provider)),
                ("model".to_string(), serde_json::json!(opts.model)),
                ("reason".to_string(), serde_json::json!(reason)),
                ("cooldown_ms".to_string(), serde_json::json!(cooldown_ms)),
            ]),
        );
    }
}

/// Feed a completed LLM-call outcome to the route's circuit breaker.
///
/// `network_failure == true` ONLY for transport-level `NetworkError`/`Timeout`
/// or provider overload (529/503 — the provider is shedding load and must not
/// be hammered), never 429 (that is rate limiting, not unreachability) and
/// never generic 500/502. A success closes the breaker; a qualifying failure
/// increments toward / re-opens it.
pub(crate) fn observe_network_outcome_for_llm_call(
    opts: &super::api::LlmCallOptions,
    network_failure: bool,
) {
    ensure_initialized_from_config();
    let keys = limiter_keys(&opts.provider, &opts.model);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    for key in keys {
        let limiter = limiter_for_key(&mut registry.limiters, &key);
        if network_failure {
            limiter.observe_network_failure(now_ms);
        } else {
            limiter.observe_success();
        }
    }
}

/// Wait until the provider rate limit allows an opaque request, then record it.
/// Returns immediately if no limit is configured or the window has capacity.
pub(crate) async fn acquire_permit(
    provider: &str,
) -> Result<RateLimitPermit, crate::value::VmError> {
    acquire_permit_for(provider, "", RateLimitRequest::default(), None, None, false).await
}

/// Wait until all provider/model buckets allow this LLM request, then record it.
/// The returned permit must be held until the provider call finishes so
/// concurrency limits cover in-flight calls rather than just launch rate.
pub(crate) async fn acquire_permit_for_llm_call(
    opts: &super::api::LlmCallOptions,
) -> Result<RateLimitPermit, crate::value::VmError> {
    acquire_permit_for(
        &opts.provider,
        &opts.model,
        RateLimitRequest::for_llm_call(opts),
        opts.rate_limit_consumer_id
            .as_deref()
            .or(opts.session_id.as_deref()),
        opts.session_id.as_deref(),
        opts.rate_limit_reroute_on_timeout,
    )
    .await
}

/// Reset all rate limiter state. Used between test runs.
pub(crate) fn reset_rate_limit_state() {
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    registry.limiters.clear();
    registry.initialized_from_config = false;
    drop(registry);
    runtime_overrides()
        .lock()
        .expect("rate limiter runtime override mutex poisoned")
        .clear();
}

/// Reset runtime overrides (via `llm_rate_limit`) without wiping unrelated
/// process-global rate-limit state.
///
/// `reset_llm_state` used to call [`reset_rate_limit_state`] unconditionally,
/// but the limiter registry is *process-global*, and `reset_thread_local_state`
/// runs from ~150 test setups in parallel — each call wiped the usage counters
/// that concurrently running rate-limit tests were asserting on. Runtime
/// overrides are also process-global, so clearing them must surgically restore
/// only the providers they shadowed. Otherwise an unrelated `llm_rate_limit`
/// cleanup can erase a sibling route's request window while that sibling still
/// holds a concurrency permit.
pub(crate) fn reset_runtime_rate_limit_overrides() {
    let cleared_provider_keys = {
        let mut overrides = runtime_overrides()
            .lock()
            .expect("rate limiter runtime override mutex poisoned");
        if overrides.is_empty() {
            return;
        }
        let keys = overrides
            .keys()
            .map(|provider| provider_key(provider))
            .collect::<Vec<_>>();
        overrides.clear();
        keys
    };

    let mut config_limiters = config_limiters_from_effective_config();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    for key in cleared_provider_keys {
        if let Some(limiter) = config_limiters.remove(&key) {
            registry.limiters.insert(key, limiter);
        } else {
            registry.limiters.remove(&key);
        }
    }
}

#[cfg(test)]
fn get_model_rate_limits(provider: &str, model: &str) -> Option<crate::llm_config::RateLimitsDef> {
    ensure_initialized_from_config();
    registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .limiters
        .get(&model_key(provider, model))
        .map(|limiter| limiter.limits.to_catalog())
}

#[cfg(test)]
fn provider_request_usage(provider: &str) -> u64 {
    ensure_initialized_from_config();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    let Some(limiter) = registry.limiters.get_mut(&provider_key(provider)) else {
        return 0;
    };
    let Some(window) = limiter.request_window.as_mut() else {
        return 0;
    };
    window.prune(crate::clock_mock::instant_now().as_millis());
    window.usage()
}

#[cfg(test)]
mod tests;
