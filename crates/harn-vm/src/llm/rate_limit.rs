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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const DURABLE_RATE_LIMIT_ENABLED_ENV: &str = "HARN_LLM_RATE_LIMIT_DURABLE";
const DURABLE_RATE_LIMIT_STATE_PATH_ENV: &str = "HARN_LLM_RATE_LIMIT_STATE_PATH";
const WINDOW_SECS: u64 = 60;
const RATE_LIMIT_ENV_FIELD_SUFFIXES: [&str; 5] =
    ["_RPM", "_TPM", "_INPUT_TPM", "_OUTPUT_TPM", "_CONCURRENCY"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RateLimitRequest {
    input_tokens: u64,
    output_tokens: u64,
}

impl RateLimitRequest {
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

struct RouteLimiter {
    request_window: Option<SlidingWindow>,
    total_token_window: Option<SlidingWindow>,
    input_token_window: Option<SlidingWindow>,
    output_token_window: Option<SlidingWindow>,
    concurrency: Option<Arc<Semaphore>>,
    cooldown_until_ms: Option<u128>,
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

/// Load rate limits from provider/model config and environment variables.
/// Safe to call multiple times (replaces existing config-derived entries).
pub(crate) fn init_from_config() {
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
    let outcome =
        crate::durable_rate_limit::acquire_durable_rate_limit(state_path, buckets, None, || false)
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        reset_rate_limit_state();
        std::env::set_var("HARN_RATE_LIMIT_TESTPROVIDER", "42");
        init_from_config();
        assert_eq!(get_rate_limit("testprovider"), Some(42));
        std::env::remove_var("HARN_RATE_LIMIT_TESTPROVIDER");
        reset_test_rate_limit_state();
    }

    #[test]
    fn concurrency_queue_does_not_consume_request_quota_until_started() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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

            let second = tokio::spawn(async { acquire_permit("queue").await });
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            assert!(!second.is_finished());
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        reset_test_rate_limit_state();
        install_concurrency_overlay();
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("llm-rate-limits.sqlite");
        let _env = EnvVarGuard::set_path(DURABLE_RATE_LIMIT_STATE_PATH_ENV, &state_path);
        init_from_config();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let first = acquire_permit("queue").await.expect("first permit");
            assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 1);

            let second = tokio::spawn(async { acquire_permit("queue").await });
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            assert!(!second.is_finished());
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        reset_test_rate_limit_state();
        install_durable_overlay();
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set_path(
            DURABLE_RATE_LIMIT_STATE_PATH_ENV,
            &temp.path().join("llm-rate-limits.sqlite"),
        );
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
}
