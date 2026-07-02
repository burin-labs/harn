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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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
const RATE_LIMIT_ENV_FIELD_SUFFIXES: [&str; 5] =
    ["_RPM", "_TPM", "_INPUT_TPM", "_OUTPUT_TPM", "_CONCURRENCY"];

/// Consecutive NetworkError/Timeout (or provider-overload 529/503) failures on
/// one route that trip the circuit breaker open. Distinct from 429 handling,
/// which uses `cooldown_until_ms` + provider Retry-After and never feeds the
/// breaker.
const NETWORK_BREAKER_FAILURE_THRESHOLD: u32 = 4;
/// How long the breaker stays open (fail-fast) before allowing a half-open probe.
/// Short on purpose: a laptop reconnect or DNS recovery should be retried soon,
/// we only want to stop burning the per-call retry budget while the link is down.
const NETWORK_BREAKER_OPEN_MS: u64 = 5_000;
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

/// Weighted sliding-window counter.
///
/// Request buckets use one unit per request; token buckets use projected token
/// counts. A single request larger than the published per-minute quota is
/// charged as one full window so it can run, but the next request waits until
/// the window clears.
struct SlidingWindow {
    max_units: u64,
    window_ms: u128,
    entries: VecDeque<(u128, u64)>,
}

impl SlidingWindow {
    fn new(max_units: u64) -> Self {
        Self {
            max_units: max_units.max(1),
            window_ms: u128::from(WINDOW_SECS) * 1000,
            entries: VecDeque::with_capacity(max_units.min(1024) as usize),
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

    fn usage(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |acc, (_, units)| acc.saturating_add(*units))
    }

    fn charge(&self, units: u64) -> u64 {
        if units == 0 {
            0
        } else {
            units.min(self.max_units)
        }
    }

    /// Drain expired entries and check capacity.
    /// Returns `Some(wait_duration)` if the window is full, `None` if OK.
    fn check(&mut self, now_ms: u128, units: u64) -> Option<Duration> {
        let charge = self.charge(units);
        if charge == 0 {
            return None;
        }
        self.prune(now_ms);
        let usage = self.usage();
        if usage.saturating_add(charge) <= self.max_units {
            return None;
        }
        let needed = usage.saturating_add(charge).saturating_sub(self.max_units);
        let mut freed = 0u64;
        for (entry_ms, units) in &self.entries {
            freed = freed.saturating_add(*units);
            if freed >= needed {
                let wait_ms = entry_ms
                    .saturating_add(self.window_ms)
                    .saturating_sub(now_ms);
                return Some(Duration::from_millis(
                    wait_ms.min(u128::from(u64::MAX)) as u64
                ));
            }
        }
        Some(Duration::from_millis(
            self.window_ms.min(u128::from(u64::MAX)) as u64,
        ))
    }

    /// Record a request or token charge timestamp.
    fn record(&mut self, now_ms: u128, units: u64) {
        let charge = self.charge(units);
        if charge > 0 {
            self.entries.push_back((now_ms, charge));
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
/// Network reachability is a property of THIS process, so the breaker is
/// per-process state (not shared via the durable rate-limit DB). It is distinct
/// from the opt-in routing-policy breaker; this one is always-on and only ever
/// reacts to transport-level network failures.
#[derive(Debug, Default, PartialEq, Eq)]
enum BreakerState {
    #[default]
    Closed,
    /// Failing fast until `until_ms`, after which one half-open probe is admitted.
    Open { until_ms: u128 },
    /// A single probe is in flight; further calls fail fast until it resolves.
    HalfOpen,
}

#[derive(Debug, Default)]
struct NetworkBreaker {
    state: BreakerState,
    consecutive_network_failures: u32,
}

impl NetworkBreaker {
    /// Whether a call should be admitted now, transitioning Open→HalfOpen when
    /// the open window has elapsed. Returns `None` to admit (Closed/HalfOpen
    /// probe), or `Some(remaining)` to fail fast while open.
    fn admit(&mut self, now_ms: u128) -> Option<Duration> {
        match self.state {
            BreakerState::Closed => None,
            BreakerState::HalfOpen => {
                // A probe is already in flight; do not admit a second.
                Some(Duration::from_millis(0))
            }
            BreakerState::Open { until_ms } => {
                if now_ms >= until_ms {
                    self.state = BreakerState::HalfOpen;
                    None
                } else {
                    Some(Duration::from_millis(
                        until_ms.saturating_sub(now_ms).min(u128::from(u64::MAX)) as u64,
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
            self.state = BreakerState::Open {
                until_ms: now_ms.saturating_add(u128::from(NETWORK_BREAKER_OPEN_MS)),
            };
        }
    }

    fn record_success(&mut self) {
        self.consecutive_network_failures = 0;
        self.state = BreakerState::Closed;
    }
}

struct RouteLimiter {
    request_window: Option<SlidingWindow>,
    total_token_window: Option<SlidingWindow>,
    input_token_window: Option<SlidingWindow>,
    output_token_window: Option<SlidingWindow>,
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
    }

    fn observe_retry_after(&mut self, now_ms: u128, retry_after_ms: u64) {
        if retry_after_ms == 0 {
            return;
        }
        let until_ms = now_ms.saturating_add(u128::from(retry_after_ms));
        self.cooldown_until_ms = Some(self.cooldown_until_ms.unwrap_or(0).max(until_ms));
    }

    /// Fail-fast wait if the network breaker is open; `None` admits the call.
    fn breaker_block(&mut self, now_ms: u128) -> Option<Duration> {
        self.breaker.admit(now_ms)
    }

    fn observe_network_failure(&mut self, now_ms: u128) {
        self.breaker.record_network_failure(now_ms);
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

/// Load rate limits from provider/model config and environment variables.
/// Safe to call multiple times (replaces existing config-derived entries).
pub(crate) fn init_from_config() {
    let mut limiters = config_limiters_from_effective_config();
    for (provider, limits) in runtime_overrides()
        .lock()
        .expect("rate limiter runtime override mutex poisoned")
        .iter()
    {
        insert_limiter(&mut limiters, provider_key(provider), limits.clone());
    }
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    registry.limiters = limiters;
    registry.initialized_from_config = true;
}

fn ensure_initialized_from_config() {
    let initialized = registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .initialized_from_config;
    if !initialized {
        init_from_config();
    }
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

async fn acquire_durable_for_keys(
    state_path: PathBuf,
    provider: &str,
    model: &str,
    keys: &[String],
    request: RateLimitRequest,
) -> Result<(), crate::value::VmError> {
    let buckets = {
        let registry = registry().lock().expect("rate limiter mutex poisoned");
        durable_buckets_for_keys(&registry, keys, request)
    };
    if buckets.is_empty() {
        return Ok(());
    }
    let outcome = crate::durable_rate_limit::acquire_durable_rate_limit(
        state_path,
        buckets,
        durable_max_wait_ms(),
        || false,
    )
    .await?;
    if outcome.waited_ms > 0 {
        let route = if model.trim().is_empty() {
            provider.to_string()
        } else {
            format!(
                "{provider}/{}",
                crate::llm_config::normalize_model_id(model)
            )
        };
        crate::events::log_debug(
            "llm.rate_limit",
            &format!(
                "Durable rate limit for '{}': waited {}ms",
                route, outcome.waited_ms
            ),
        );
        // The wait was clamped before the quota cleared: proceed to attempt the
        // provider anyway rather than block longer. A genuine over-quota route
        // returns a 429 here, which feeds the Retry-After / retry / escalation
        // path. This is what stops one rate-limited provider from eating the
        // whole trial wall.
        if outcome.timed_out {
            crate::events::log_debug(
                "llm.rate_limit",
                &format!(
                    "Durable rate limit for '{}': wait clamped at {}ms (cap reached); \
                     proceeding to attempt the provider",
                    route, outcome.waited_ms
                ),
            );
        }
    }
    Ok(())
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
    crate::events::log_debug(
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
            acquire_durable_for_keys(state_path, provider, model, &keys, request).await?;
            return Ok(RateLimitPermit { _permits: permits });
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
fn breaker_open_error(provider: &str, model: &str, remaining: Duration) -> crate::value::VmError {
    let route = if model.trim().is_empty() {
        provider.to_string()
    } else {
        format!(
            "{provider}/{}",
            crate::llm_config::normalize_model_id(model)
        )
    };
    crate::value::VmError::CategorizedError {
        message: format!(
            "network circuit breaker open for '{route}': sustained network failures or \
             provider overload; failing fast for {}ms (a half-open probe will follow)",
            remaining.as_millis()
        ),
        category: crate::value::ErrorCategory::TransientNetwork,
    }
}

/// Fail-fast if the per-route network breaker is open. Returns `Ok(())` to admit
/// the call (Closed, or an admitted half-open probe), or a typed transient error
/// to short-circuit the retry loop while the link is down.
///
/// Separate from `acquire_permit_for_llm_call` so the breaker decision is taken
/// once per call attempt at the same seam that observes the outcome, rather than
/// being entangled with the (durable) rate-limit wait loop.
pub(crate) fn check_network_breaker_for_llm_call(
    opts: &super::api::LlmCallOptions,
) -> Result<(), crate::value::VmError> {
    ensure_initialized_from_config();
    let keys = limiter_keys(&opts.provider, &opts.model);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    // Use the max remaining-open across the route's keys: if any key is open, the
    // call fails fast. `breaker_block` also performs the Open→HalfOpen
    // transition, so probe admission is consistent across sibling callers.
    let mut blocked: Option<Duration> = None;
    for key in &keys {
        let limiter = limiter_for_key(&mut registry.limiters, key);
        if let Some(remaining) = limiter.breaker_block(now_ms) {
            blocked = Some(match blocked {
                Some(prev) => prev.max(remaining),
                None => remaining,
            });
        }
    }
    drop(registry);
    match blocked {
        Some(remaining) => Err(breaker_open_error(&opts.provider, &opts.model, remaining)),
        None => Ok(()),
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
    acquire_permit_for(provider, "", RateLimitRequest::default()).await
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
mod tests {
    use super::*;

    fn install_quota_overlay() {
        let overlay = crate::llm_config::parse_config_toml(
            "[providers.quota]\n\
             base_url = \"https://quota.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 9, tpm = 900, concurrency = 2 }\n\
             \n\
             [models.\"quota-model\"]\n\
             name = \"Quota Model\"\n\
             provider = \"quota\"\n\
             context_window = 32768\n\
             rate_limits = { rpm = 7, tpm = 700, input_tpm = 300, output_tpm = 400, concurrency = 1 }\n",
        )
        .expect("quota overlay parses");
        crate::llm_config::set_user_overrides(Some(overlay));
    }

    fn install_concurrency_overlay() {
        let overlay = crate::llm_config::parse_config_toml(
            "[providers.queue]\n\
             base_url = \"https://queue.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 2, concurrency = 1 }\n",
        )
        .expect("queue overlay parses");
        crate::llm_config::set_user_overrides(Some(overlay));
    }

    fn install_durable_overlay() {
        let overlay = crate::llm_config::parse_config_toml(
            "[providers.durable]\n\
             base_url = \"https://durable.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 1 }\n",
        )
        .expect("durable overlay parses");
        crate::llm_config::set_user_overrides(Some(overlay));
    }

    fn reset_test_rate_limit_state() {
        reset_rate_limit_state();
        crate::llm_config::clear_user_overrides();
    }

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn set_value(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            Self::set_value(key, value)
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.old.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn durable_usage(path: &std::path::Path, key: &str) -> u64 {
        if !path.exists() {
            return 0;
        }
        let conn = rusqlite::Connection::open(path).expect("open durable rate limit db");
        conn.query_row(
            "SELECT COALESCE(SUM(units), 0)
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, i64>(0),
        )
        .expect("query durable usage")
        .max(0) as u64
    }

    #[test]
    fn sliding_window_allows_weighted_tokens_within_limit() {
        let mut window = SlidingWindow::new(10);
        assert!(window.check(0, 4).is_none());
        window.record(0, 4);
        assert!(window.check(0, 6).is_none());
        window.record(0, 6);
        assert!(window.check(0, 1).is_some());
    }

    #[test]
    fn sliding_window_waits_until_enough_weight_expires() {
        let mut window = SlidingWindow::new(10);
        window.record(0, 4);
        window.record(10_000, 6);
        let wait = window.check(10_000, 4).expect("window should be full");
        assert_eq!(wait.as_secs(), 50);
    }

    #[test]
    fn sliding_window_expires_entries_at_window_boundary() {
        let mut window = SlidingWindow::new(10);
        window.record(0, 10);
        assert!(window.check(59_999, 1).is_some());
        assert!(window.check(60_000, 1).is_none());
    }

    #[test]
    fn oversized_token_reservation_charges_one_full_window() {
        let mut window = SlidingWindow::new(10);
        assert!(window.check(0, 25).is_none());
        window.record(0, 25);
        assert_eq!(window.usage(), 10);
        assert!(window.check(0, 1).is_some());
    }

    #[test]
    fn retry_after_cooldown_blocks_route_without_catalog_limit() {
        let mut limiter = RouteLimiter::new(EffectiveRateLimits::default());
        limiter.observe_retry_after(1_000, 2_500);

        let wait = limiter
            .check(1_000, RateLimitRequest::default())
            .expect("cooldown should block route");
        assert_eq!(wait.as_millis(), 2_500);
        assert!(
            limiter.check(3_500, RateLimitRequest::default()).is_none(),
            "cooldown should expire exactly at the provider-supplied deadline"
        );
    }

    #[test]
    fn retry_after_cooldown_extends_existing_route_cooldown() {
        let mut limiter = RouteLimiter::new(EffectiveRateLimits::default());
        limiter.observe_retry_after(1_000, 1_000);
        limiter.observe_retry_after(1_500, 3_000);

        let wait = limiter
            .check(2_000, RateLimitRequest::default())
            .expect("extended cooldown should block route");
        assert_eq!(wait.as_millis(), 2_500);
    }

    #[test]
    fn init_from_config_loads_model_rate_limits_from_catalog_overlay() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_quota_overlay();
        init_from_config();
        let provider_limits = get_rate_limits("quota").expect("provider limits");
        assert_eq!(provider_limits.rpm, Some(9));
        assert_eq!(provider_limits.tpm, Some(900));
        assert_eq!(provider_limits.concurrency, Some(2));
        let model_limits = get_model_rate_limits("quota", "quota-model").expect("model limits");
        assert_eq!(model_limits.rpm, Some(7));
        assert_eq!(model_limits.tpm, Some(700));
        assert_eq!(model_limits.input_tpm, Some(300));
        assert_eq!(model_limits.output_tpm, Some(400));
        assert_eq!(model_limits.concurrency, Some(1));
        reset_test_rate_limit_state();
    }

    #[test]
    fn provider_env_override_sets_tpm() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_quota_overlay();
        std::env::set_var("HARN_RATE_LIMIT_QUOTA_TPM", "1000000");
        init_from_config();
        let limits = get_rate_limits("quota").expect("provider limits");
        assert_eq!(limits.rpm, Some(9));
        assert_eq!(limits.tpm, Some(1_000_000));
        std::env::remove_var("HARN_RATE_LIMIT_QUOTA_TPM");
        reset_test_rate_limit_state();
    }

    #[test]
    fn legacy_provider_rpm_env_still_sets_provider_bucket() {
        let _guard = crate::llm::env_guard();
        reset_rate_limit_state();
        std::env::set_var("HARN_RATE_LIMIT_TESTPROVIDER", "42");
        init_from_config();
        assert_eq!(get_rate_limit("testprovider"), Some(42));
        std::env::remove_var("HARN_RATE_LIMIT_TESTPROVIDER");
        reset_test_rate_limit_state();
    }

    #[test]
    fn concurrency_queue_does_not_consume_request_quota_until_started() {
        let _guard = crate::llm::env_guard();
        let _durable_disabled = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_ENABLED_ENV, "0");
        reset_test_rate_limit_state();
        install_concurrency_overlay();
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let first = acquire_permit("queue").await.expect("first permit");
            assert_eq!(provider_request_usage("queue"), 1);

            let mut second = tokio::spawn(async { acquire_permit("queue").await });
            // The second acquire must stay parked behind the first permit.
            // Poll under a real-time timeout instead of counting yields —
            // yield counting is scheduler-sensitive, and when this fires it
            // also surfaces *what* completed instead of a bare is_finished.
            if let Ok(join) =
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut second).await
            {
                let outcome = match join {
                    Ok(Ok(_permit)) => "a second permit was granted".to_string(),
                    Ok(Err(error)) => format!("acquire failed: {error:?}"),
                    Err(join_error) => format!("task panicked: {join_error}"),
                };
                panic!("second acquire completed while the first permit was held ({outcome})");
            }
            assert_eq!(provider_request_usage("queue"), 1);

            drop(first);
            let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
                .await
                .expect("second task should acquire after first permit drops")
                .expect("second task completed")
                .expect("second permit");
            assert_eq!(provider_request_usage("queue"), 2);
            drop(second);
        });

        reset_test_rate_limit_state();
    }

    #[test]
    fn durable_concurrency_queue_does_not_consume_request_quota_until_started() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_concurrency_overlay();
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("llm-rate-limits.sqlite");
        let _env = EnvVarGuard::set_path(DURABLE_RATE_LIMIT_STATE_PATH_ENV, &state_path);
        let _clock =
            crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let first = acquire_permit("queue").await.expect("first permit");
            assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 1);

            let mut second = tokio::spawn(async { acquire_permit("queue").await });
            // See concurrency_queue_does_not_consume_request_quota_until_started
            // for why this polls under a timeout instead of counting yields.
            if let Ok(join) =
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut second).await
            {
                let outcome = match join {
                    Ok(Ok(_permit)) => "a second permit was granted".to_string(),
                    Ok(Err(error)) => format!("acquire failed: {error:?}"),
                    Err(join_error) => format!("task panicked: {join_error}"),
                };
                panic!("second acquire completed while the first permit was held ({outcome})");
            }
            assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 1);

            drop(first);
            let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
                .await
                .expect("second task should acquire after first permit drops")
                .expect("second task completed")
                .expect("second permit");
            assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 2);
            drop(second);
        });

        reset_test_rate_limit_state();
    }

    #[test]
    fn durable_state_path_coordinates_after_process_local_reset() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_durable_overlay();
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set_path(
            DURABLE_RATE_LIMIT_STATE_PATH_ENV,
            &temp.path().join("llm-rate-limits.sqlite"),
        );
        // This test asserts the UNCAPPED full-window coordination wait, so it
        // explicitly disables the per-acquire backoff clamp (default-ON, see
        // durable_backoff_is_clamped_per_acquire_so_one_route_cannot_eat_the_wall).
        let _cap = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV, "0");
        let _clock =
            crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let first = acquire_permit("durable").await.expect("first permit");
            drop(first);

            reset_rate_limit_state();
            init_from_config();

            let before = crate::clock_mock::now_ms();
            let second = acquire_permit("durable").await.expect("second permit");
            let after = crate::clock_mock::now_ms();
            drop(second);

            assert!(
                after.saturating_sub(before) >= 60_000,
                "second process-local registry should wait on durable SQLite state"
            );
        });

        reset_test_rate_limit_state();
    }

    // ---------------------------------------------------------------------
    // Mechanism-fitness: durable rate-limit backoff is BOUNDED per acquire.
    //
    // A sustained-quota provider (the field case: cerebras:gpt-oss-120b
    // returning a full per-minute window wait on nearly every call) must NOT be
    // able to make one LLM call block out the whole sliding window. Without the
    // clamp the durable acquire waits the full ~60s; with the default-ON cap it
    // returns after the ceiling so the call can attempt the provider and let a
    // real 429 drive the retry/escalation path. This pins that the wait cannot
    // dominate: a saturated bucket waits ~cap, never the full window.
    //
    // Sibling `durable_state_path_coordinates_after_process_local_reset` pins
    // the UNCAPPED >= 60_000 wait (it sets the cap env to 0); the two together
    // bracket the mechanism so a refactor cannot silently drop the clamp.
    // ---------------------------------------------------------------------
    #[test]
    fn durable_backoff_is_clamped_per_acquire_so_one_route_cannot_eat_the_wall() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_durable_overlay();
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set_path(
            DURABLE_RATE_LIMIT_STATE_PATH_ENV,
            &temp.path().join("llm-rate-limits.sqlite"),
        );
        // Bound any one acquire's durable backoff to 5s, well under the 60s
        // sliding window the rpm=1 overlay would otherwise force a second call
        // to wait out.
        let cap_ms: u64 = 5_000;
        let _cap = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV, cap_ms.to_string());
        let _clock =
            crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            // First acquire consumes the single rpm slot for the window.
            let first = acquire_permit("durable").await.expect("first permit");
            drop(first);

            // The second acquire saturates the durable rpm bucket and would, with
            // an unbounded wait, sleep the full 60s window. The clamp must cap it.
            let before = crate::clock_mock::now_ms();
            let second = acquire_permit("durable").await.expect("second permit");
            let after = crate::clock_mock::now_ms();
            drop(second);

            let waited = after.saturating_sub(before).max(0) as u64;
            assert!(
                waited <= cap_ms.saturating_add(1_000),
                "durable backoff must be clamped to ~{cap_ms}ms, but one acquire waited {waited}ms \
                 — a sustained-quota route would otherwise dominate the trial wall"
            );
            assert!(
                waited < 60_000,
                "durable backoff must NOT wait the full {WINDOW_SECS}s window (waited {waited}ms)"
            );
        });

        reset_test_rate_limit_state();
    }

    // ---------------------------------------------------------------------
    // Reset-scope contract.
    //
    // The rate-limiter registry is process-global. Two reset levels exist and
    // MUST stay decoupled:
    //
    //   * `reset_llm_state` (runs from parallel in-process unit tests) only
    //     scrubs leaked *runtime overrides* — it must NOT wipe a config-derived
    //     registry, or it would corrupt a concurrently asserting sibling test.
    //   * `reset_rate_limit_state` (the full wipe, reached in sequential /
    //     separate-process contexts via `reset_thread_local_state`) clears
    //     everything, including retry-after cooldowns.
    //
    // These two tests pin each half of that contract so a future refactor can't
    // silently re-merge them — the failure mode was a leaked cooldown stalling a
    // later conformance test's mocked LLM call under a paused clock for the full
    // per-test timeout.
    // ---------------------------------------------------------------------

    #[test]
    fn full_registry_reset_clears_retry_after_cooldown() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_quota_overlay();
        init_from_config();

        let keys = limiter_keys("quota", "quota-model");
        let request = RateLimitRequest::default();
        let now_ms = 0;

        // A provider 429 installs a long retry-after cooldown on the route.
        {
            let mut registry = registry().lock().expect("registry");
            for key in &keys {
                limiter_for_key(&mut registry.limiters, key).observe_retry_after(now_ms, 60_000);
            }
            assert!(
                check_wait_for_keys(&mut registry, &keys, request, now_ms).is_some(),
                "cooldown should force a wait before reset"
            );
        }

        // The full wipe (what `reset_thread_local_state` performs between
        // sequential tests) must clear the cooldown so the next test's call
        // doesn't stall on a paused clock.
        reset_rate_limit_state();
        init_from_config();
        {
            let mut registry = registry().lock().expect("registry");
            assert!(
                check_wait_for_keys(&mut registry, &keys, request, now_ms).is_none(),
                "cooldown must not survive a full registry reset"
            );
        }

        reset_test_rate_limit_state();
    }

    #[test]
    fn runtime_override_reset_preserves_config_registry() {
        let _guard = crate::llm::env_guard();
        reset_test_rate_limit_state();
        install_quota_overlay();
        init_from_config();

        let provider_bucket = provider_key("quota");
        assert!(
            registry()
                .lock()
                .expect("registry")
                .limiters
                .contains_key(&provider_bucket),
            "config init should populate the provider limiter"
        );

        // The override-scoped reset (what `reset_llm_state` runs from parallel
        // unit tests, with no runtime override installed) must leave the
        // config-derived registry untouched — otherwise it would wipe usage
        // counters a concurrent rate-limit test is asserting on.
        reset_runtime_rate_limit_overrides();

        let registry = registry().lock().expect("registry");
        assert!(
            registry.initialized_from_config,
            "config-init flag must survive an override-only reset"
        );
        assert!(
            registry.limiters.contains_key(&provider_bucket),
            "config limiters must survive an override-only reset"
        );
        drop(registry);

        reset_test_rate_limit_state();
    }

    #[test]
    fn runtime_override_reset_preserves_unrelated_provider_usage() {
        let _guard = crate::llm::env_guard();
        let _durable_disabled = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_ENABLED_ENV, "0");
        reset_test_rate_limit_state();
        install_concurrency_overlay();
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let first = acquire_permit("queue").await.expect("first permit");
            assert_eq!(provider_request_usage("queue"), 1);

            let limits = crate::llm_config::RateLimitsDef {
                rpm: Some(1),
                ..Default::default()
            };
            set_rate_limits("runtime-only-provider", limits);
            reset_runtime_rate_limit_overrides();

            assert_eq!(
                provider_request_usage("queue"),
                1,
                "clearing an unrelated runtime override must not wipe queued provider usage"
            );

            let mut second = tokio::spawn(async { acquire_permit("queue").await });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                    .await
                    .is_err(),
                "second acquire should remain queued behind the held concurrency permit"
            );

            drop(first);
            let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
                .await
                .expect("second task should acquire after first permit drops")
                .expect("second task completed")
                .expect("second permit");
            assert_eq!(provider_request_usage("queue"), 2);
            drop(second);
        });

        reset_test_rate_limit_state();
    }

    // ---- network circuit breaker (pure state-machine tests) ----------------

    #[test]
    fn breaker_opens_after_threshold_consecutive_network_failures() {
        let mut b = NetworkBreaker::default();
        // Below threshold: still closed, calls admitted.
        for i in 1..NETWORK_BREAKER_FAILURE_THRESHOLD {
            b.record_network_failure(u128::from(i));
            assert!(
                b.admit(u128::from(i)).is_none(),
                "must stay closed below threshold ({i} failures)"
            );
        }
        // Crossing the threshold opens it.
        b.record_network_failure(100);
        let blocked = b.admit(100).expect("breaker must be open at threshold");
        assert!(
            blocked.as_millis() > 0 && blocked.as_millis() <= u128::from(NETWORK_BREAKER_OPEN_MS),
            "open window remaining {}ms out of (0, {NETWORK_BREAKER_OPEN_MS}]",
            blocked.as_millis()
        );
    }

    #[test]
    fn breaker_fails_fast_while_open_then_half_opens_then_closes_on_probe_success() {
        let mut b = NetworkBreaker::default();
        let open_at = 1_000u128;
        for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
            b.record_network_failure(open_at);
        }
        // Fail fast inside the open window.
        assert!(
            b.admit(open_at + 1).is_some(),
            "must fail fast while open window is active"
        );
        // After the window elapses, exactly one half-open probe is admitted...
        let after = open_at + u128::from(NETWORK_BREAKER_OPEN_MS) + 1;
        assert!(
            b.admit(after).is_none(),
            "half-open probe must be admitted once the window elapses"
        );
        // ...and a concurrent caller while the probe is in flight is blocked.
        assert!(
            b.admit(after).is_some(),
            "second concurrent call must not get a second half-open probe"
        );
        // A successful probe closes the breaker and clears the failure count.
        b.record_success();
        assert!(
            b.admit(after).is_none(),
            "breaker must close after probe success"
        );
        assert_eq!(b.consecutive_network_failures, 0);
    }

    #[test]
    fn breaker_reopens_when_half_open_probe_fails() {
        let mut b = NetworkBreaker::default();
        let open_at = 0u128;
        for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
            b.record_network_failure(open_at);
        }
        let after = u128::from(NETWORK_BREAKER_OPEN_MS) + 1;
        assert!(b.admit(after).is_none(), "half-open probe admitted");
        // The probe fails (still no network): the breaker re-opens immediately,
        // even though the *count* logic alone is irrelevant in half-open state.
        b.record_network_failure(after);
        assert!(
            b.admit(after + 1).is_some(),
            "a failed half-open probe must re-open the breaker"
        );
    }

    #[test]
    fn breaker_success_resets_failure_streak() {
        let mut b = NetworkBreaker::default();
        b.record_network_failure(0);
        b.record_network_failure(0);
        b.record_success();
        assert_eq!(b.consecutive_network_failures, 0);
        // One post-success failure must not be enough to re-open (streak reset).
        b.record_network_failure(0);
        assert!(
            b.admit(0).is_none(),
            "single failure after reset must stay closed"
        );
    }

    #[test]
    fn breaker_does_not_open_on_rate_limit_or_server_errors() {
        // 429 / 5xx must NOT feed the breaker. Drive the same number of NON-network
        // outcomes well past the threshold and assert it never opens. (The wiring
        // in agent_observe only calls `observe_network_failure` for true network
        // failures; here we assert the classifier that gates that call.)
        use super::super::agent_observe::is_network_failure_llm_error;
        use crate::value::{ErrorCategory, VmError, VmValue};

        let rate_limited = VmError::CategorizedError {
            message: "429 too many requests".to_string(),
            category: ErrorCategory::RateLimit,
        };
        let server_error = VmError::CategorizedError {
            message: "503 service unavailable".to_string(),
            category: ErrorCategory::ServerError,
        };
        let thrown_429 = VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "[rate_limited] too many requests",
        )));
        assert!(
            !is_network_failure_llm_error(&rate_limited),
            "429 is not a network failure"
        );
        assert!(
            !is_network_failure_llm_error(&server_error),
            "5xx is not a network failure"
        );
        assert!(
            !is_network_failure_llm_error(&thrown_429),
            "thrown 429 is not a network failure"
        );

        // A genuine network/timeout failure IS one.
        let connect = VmError::CategorizedError {
            message: "openai request error (connect): connection refused".to_string(),
            category: ErrorCategory::TransientNetwork,
        };
        let timeout = VmError::CategorizedError {
            message: "openai request error (timeout): operation timed out".to_string(),
            category: ErrorCategory::Timeout,
        };
        assert!(
            is_network_failure_llm_error(&connect),
            "connect failure is a network failure"
        );
        assert!(
            is_network_failure_llm_error(&timeout),
            "timeout is a network failure"
        );
    }
}
