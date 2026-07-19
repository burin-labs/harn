//! First-class `routing_policy` primitive for `llm_call`.
//!
//! `routing_policy({...})` validates a config dict and returns a tagged
//! handle. Passing it through `llm_call(prompt, system, {routing:
//! policy, ...})` runs the chain with failover, latency-aware racing,
//! and per-call / session budget enforcement. Each decision emits a
//! structured tape event so transcripts and replay can attribute the
//! outcome to a specific chain link.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::value::{VmError, VmValue};

use super::api::LlmCallOptions;
use super::cost::LlmBudgetEnvelope;
use super::routing_verifier::{parse_escalate_on, verifiers_summary, Verifier};

mod auth;

/// Marker key set by `routing_policy(...)` to distinguish a validated
/// routing config dict from a stray dict the user happened to pass.
const ROUTING_POLICY_TAG: &str = "__routing_policy__";

const HANDLE_KEY: &str = "__handle__";

/// Default racing slack: if `latency.race_after_ms` is set without a
/// finite per-attempt budget, we still need an upper bound to cancel a
/// stuck primary. Two minutes matches `HARN_LLM_TIMEOUT`'s default.
const DEFAULT_RACE_PRIMARY_TIMEOUT_MS: u64 = 120_000;

const DEFAULT_FAILOVER_STATUSES: &[u16] = &[408, 429, 500, 502, 503, 504];

/// Build a first-class routing policy from catalog-declared same-logical-model
/// routes. This intentionally feeds the existing routing executor instead of
/// adding another transport fallback path, so receipts, budget checks, and
/// transcript metadata stay in one schema.
pub(crate) fn build_equivalent_failover_policy(
    provider: &str,
    model: &str,
    max_routes: usize,
    on_no_dispatch: bool,
    requirements: crate::llm_config::EquivalentModelRequirements,
) -> Option<Arc<RoutingPolicyConfig>> {
    if max_routes < 2 || super::providers::is_internal_simulator(provider) {
        return None;
    }

    let mut chain = vec![ChainLink {
        provider: provider.to_string(),
        model: model.to_string(),
        timeout_ms: None,
        label: Some("primary".to_string()),
        region: None,
        overrides: None,
    }];

    let candidates =
        crate::llm_config::equivalent_model_catalog_entries_for_requirements(model, requirements);
    // A same-provider equivalent keeps auth and transport locality while
    // rotating away from the sticky model route. Try one before crossing a
    // provider boundary, then retain any remaining same-provider candidates as
    // tail capacity. This keeps the default three-route ladder as primary ->
    // local rotation -> cross-provider recovery instead of letting aliases
    // consume every bounded slot.
    let (same_provider, cross_provider): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|(_, candidate)| candidate.provider == provider);
    let mut same_provider = same_provider.into_iter();
    let ordered_candidates = same_provider
        .next()
        .into_iter()
        .chain(cross_provider)
        .chain(same_provider);
    for (candidate_model, candidate) in ordered_candidates {
        if chain.len() >= max_routes {
            break;
        }
        if chain
            .iter()
            .any(|link| link.provider == candidate.provider && link.model == candidate_model)
        {
            continue;
        }
        if !provider_route_available(&candidate.provider) {
            continue;
        }
        chain.push(ChainLink {
            provider: candidate.provider.clone(),
            model: candidate_model,
            timeout_ms: None,
            label: Some(if candidate.provider == provider {
                format!("equivalent_same_provider:{}", candidate.provider)
            } else {
                format!("equivalent:{}", candidate.provider)
            }),
            region: None,
            overrides: None,
        });
    }

    if chain.len() < 2 {
        return None;
    }

    let label = format!("equivalent_failover({provider}:{model})");
    Some(linear_failover_policy(label, chain, on_no_dispatch))
}

/// Lower the legacy preference/fallback options onto the canonical routing
/// executor so transport errors and empty-generation failures share one
/// attempt ledger and one exhaustion contract.
pub(crate) fn build_transport_failover_policy(
    provider: &str,
    model: &str,
    route_fallbacks: &[super::api::LlmRouteFallback],
    fallback_chain: &[String],
) -> Option<Arc<RoutingPolicyConfig>> {
    let mut chain = vec![ChainLink {
        provider: provider.to_string(),
        model: model.to_string(),
        timeout_ms: None,
        label: Some("primary".to_string()),
        region: None,
        overrides: None,
    }];
    let mut push = |candidate_provider: &str, candidate_model: &str, label: String| {
        if candidate_provider == provider && candidate_model == model {
            return;
        }
        if chain
            .iter()
            .any(|link| link.provider == candidate_provider && link.model == candidate_model)
            || !provider_route_available(candidate_provider)
        {
            return;
        }
        chain.push(ChainLink {
            provider: candidate_provider.to_string(),
            model: candidate_model.to_string(),
            timeout_ms: None,
            label: Some(label),
            region: None,
            overrides: None,
        });
    };
    for route in route_fallbacks {
        push(
            &route.provider,
            &route.model,
            format!("preference:{}", route.provider),
        );
    }
    for fallback_provider in fallback_chain {
        push(
            fallback_provider,
            model,
            format!("fallback:{fallback_provider}"),
        );
    }
    if let Some(config_fallback) =
        crate::llm_config::provider_config(provider).and_then(|definition| definition.fallback)
    {
        push(
            &config_fallback,
            model,
            format!("provider_config:{config_fallback}"),
        );
    }
    if chain.len() < 2 {
        return None;
    }
    let label = format!("transport_failover({provider}:{model})");
    Some(linear_failover_policy(label, chain, false))
}

fn linear_failover_policy(
    label: String,
    chain: Vec<ChainLink>,
    on_no_dispatch: bool,
) -> Arc<RoutingPolicyConfig> {
    Arc::new(RoutingPolicyConfig {
        failover: FailoverRules {
            max_attempts: Some(chain.len()),
            on_no_dispatch,
            ..FailoverRules::default()
        },
        latency: LatencyRules::default(),
        budget: BudgetRules::default(),
        observe: ObserveRules::default(),
        escalate_on: Vec::new(),
        max_refines_per_link: 0,
        label,
        chain,
        is_ladder: false,
    })
}

fn provider_route_available(provider: &str) -> bool {
    super::provider_auth::provider_auth_status(provider).available
}

/// What to do when a budget cap is exceeded while the chain is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetExceedAction {
    /// Throw the standard budget-exceeded error and stop the chain.
    Abort,
    /// Skip this link and continue to the next one in the chain.
    Skip,
    /// Emit a warning event but allow the call to proceed.
    Warn,
}

impl BudgetExceedAction {
    fn parse(value: &str) -> Result<Self, VmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "abort" | "" => Ok(Self::Abort),
            "skip" | "downgrade" => Ok(Self::Skip),
            "warn" => Ok(Self::Warn),
            other => Err(runtime_error(format!(
                "routing_policy.budget.on_exceed: expected one of abort|skip|warn, got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Abort => "abort",
            Self::Skip => "skip",
            Self::Warn => "warn",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChainLink {
    pub provider: String,
    pub model: String,
    /// Optional per-link override that wins over the policy-level timeout.
    pub timeout_ms: Option<u64>,
    /// Optional human-readable label for telemetry; falls back to
    /// `provider:model` if unset.
    pub label: Option<String>,
    /// Optional region override (e.g. AWS Bedrock `us-east-1` vs
    /// `eu-west-1`). When set, the provider call uses this region instead
    /// of the env/profile-resolved default. `None` keeps the existing
    /// env-fallback behaviour, so omitting it is fully backward
    /// compatible. Only multi-region providers (currently Bedrock) act on
    /// it; other providers ignore it gracefully.
    pub region: Option<String>,
    /// Per-step generation-parameter overrides supplied by a `models:`
    /// ladder step's `options` dict, applied over the call's base options
    /// in [`link_options`]. Only the curated scalar keys in
    /// [`apply_ladder_step_overrides`] are honored; unknown keys are
    /// rejected at ladder-build time so there is no silent drop.
    pub overrides: Option<Arc<crate::value::DictMap>>,
}

impl ChainLink {
    pub(crate) fn display_label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| format!("{}:{}", self.provider, self.model))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FailoverRules {
    pub on_status: Vec<u16>,
    pub on_timeout_ms: Option<u64>,
    pub on_error_kinds: Vec<String>,
    /// Opt-in for billed no-dispatch upstream contract violations. Kept out
    /// of default failover because these errors are already retried once by
    /// `observed_llm_call` before routing sees the exhausted error.
    pub on_no_dispatch: bool,
    pub max_attempts: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LatencyRules {
    pub target_p95_ms: Option<u64>,
    pub race_after_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BudgetRules {
    pub per_call_usd: Option<f64>,
    pub session_usd: Option<f64>,
    pub on_exceed: Option<BudgetExceedAction>,
}

impl BudgetRules {
    pub(crate) fn on_exceed_or_abort(&self) -> BudgetExceedAction {
        self.on_exceed.unwrap_or(BudgetExceedAction::Abort)
    }

    pub(crate) fn envelope(&self) -> Option<LlmBudgetEnvelope> {
        let envelope = LlmBudgetEnvelope {
            max_cost_usd: self.per_call_usd,
            total_budget_usd: self.session_usd,
            max_input_tokens: None,
            max_output_tokens: None,
        };
        if envelope.max_cost_usd.is_none() && envelope.total_budget_usd.is_none() {
            None
        } else {
            Some(envelope)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ObserveRules {
    pub emit_event: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingPolicyConfig {
    pub chain: Vec<ChainLink>,
    pub failover: FailoverRules,
    pub latency: LatencyRules,
    pub budget: BudgetRules,
    pub observe: ObserveRules,
    /// Verifier chain that gates frontier-tier escalation. Each
    /// verifier inspects the candidate's text after a successful
    /// link; the first non-`accept` signal drives the next decision
    /// (refine = retry same link with a nudge; escalate = advance).
    pub escalate_on: Vec<Verifier>,
    /// Maximum number of refine retries permitted **per link**.
    /// Refines also count against `failover.max_attempts`, so the
    /// effective cap is `min(refines_per_link, remaining_attempts)`.
    /// Defaults to 1.
    pub max_refines_per_link: usize,
    /// Stable identifier — short label or the full
    /// `routing_policy(chain=N)` summary. Forwarded into tape events so
    /// transcripts can correlate attempts back to the policy that drove
    /// them.
    pub label: String,
    /// True when this policy was synthesized from a `models:`/`ladder:`
    /// option rather than an explicit `routing_policy(...)`. Ladders emit a
    /// dedicated `llm_models_advance` trace event on each transport-driven
    /// step advance; explicit routing policies keep only the existing
    /// `<dispatch>.attempt` telemetry so their behavior is unchanged.
    pub is_ladder: bool,
}

impl RoutingPolicyConfig {
    pub(crate) fn dispatch_label(&self) -> String {
        self.observe
            .emit_event
            .clone()
            .unwrap_or_else(|| "llm.routing".to_string())
    }
}

// ---------------------------------------------------------------------------
// Thread-local registry for parsed policies.
//
// `routing_policy(...)` returns a tagged dict that's safe to pass
// through the VM. The dict carries a `__handle__` integer; the parsed
// config itself lives here so the executor reaches it without
// re-parsing. The registry is keyed by an auto-incrementing handle
// number and rooted in a thread-local so concurrent agent loops on
// different worker threads don't share state. Handles outlive a single
// `llm_call` so the same policy value can drive multiple calls.
// ---------------------------------------------------------------------------

thread_local! {
    static POLICY_REGISTRY: RefCell<BTreeMap<u64, Arc<RoutingPolicyConfig>>> =
        const { RefCell::new(BTreeMap::new()) };
}

static POLICY_COUNTER: AtomicU64 = AtomicU64::new(1);

fn intern_policy(policy: RoutingPolicyConfig) -> u64 {
    let handle = POLICY_COUNTER.fetch_add(1, Ordering::SeqCst);
    POLICY_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(handle, Arc::new(policy));
    });
    handle
}

fn lookup_policy(handle: u64) -> Option<Arc<RoutingPolicyConfig>> {
    POLICY_REGISTRY.with(|registry| registry.borrow().get(&handle).cloned())
}

/// Drop every interned policy. Each `routing_policy(...)` call interns a
/// fresh entry keyed by a monotonic counter and never removes it, so a
/// reused worker thread accumulates one `RoutingPolicyConfig` per call
/// across a test suite. The per-test reset path (via `reset_llm_state`)
/// clears the table so a handle leak in one fixture doesn't bleed
/// pricing assumptions — or memory — into the next.
pub(crate) fn clear_policy_registry() {
    POLICY_REGISTRY.with(|registry| registry.borrow_mut().clear());
}

/// Number of interned routing policies on this thread. Test-only.
#[cfg(test)]
pub(crate) fn policy_registry_len() -> usize {
    POLICY_REGISTRY.with(|registry| registry.borrow().len())
}

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

fn runtime_error(message: String) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message)))
}

fn parse_label(dict: &crate::value::DictMap, key: &str) -> Result<String, VmError> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(VmValue::Nil) | None => Ok(String::new()),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.{key}: expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_pos_u64(dict: &crate::value::DictMap, key: &str) -> Result<Option<u64>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(Some(*n as u64)),
        Some(VmValue::Float(f)) if f.is_finite() && *f >= 0.0 => Ok(Some(*f as u64)),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.{key}: expected a non-negative integer (got {})",
            other.type_name()
        ))),
    }
}

fn parse_pos_usize(dict: &crate::value::DictMap, key: &str) -> Result<Option<usize>, VmError> {
    parse_pos_u64(dict, key).map(|opt| opt.map(|v| v as usize))
}

fn parse_pos_f64(dict: &crate::value::DictMap, key: &str) -> Result<Option<f64>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(Some(*n as f64)),
        Some(VmValue::Float(f)) if f.is_finite() && *f >= 0.0 => Ok(Some(*f)),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.{key}: expected a non-negative number (got {})",
            other.type_name()
        ))),
    }
}

fn parse_string_list(dict: &crate::value::DictMap, key: &str) -> Result<Vec<String>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                let text = item.display();
                let trimmed = text.trim();
                if !trimmed.is_empty() && !out.iter().any(|existing: &String| existing == trimmed) {
                    out.push(trimmed.to_string());
                }
            }
            Ok(out)
        }
        Some(VmValue::String(s)) => Ok(s
            .split(',')
            .map(str::trim)
            .filter(|chunk| !chunk.is_empty())
            .map(str::to_string)
            .collect()),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.{key}: expected a list of strings (got {})",
            other.type_name()
        ))),
    }
}

fn parse_status_list(dict: &crate::value::DictMap, key: &str) -> Result<Vec<u16>, VmError> {
    let Some(value) = dict.get(key) else {
        return Ok(Vec::new());
    };
    let items = match value {
        VmValue::Nil => return Ok(Vec::new()),
        VmValue::List(items) => items.clone(),
        _ => {
            return Err(runtime_error(format!(
                "routing_policy.failover.{key}: expected a list of HTTP status codes"
            )));
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        let code = item.as_int().ok_or_else(|| {
            runtime_error(format!(
                "routing_policy.failover.{key}: expected integer status codes (got {})",
                item.type_name()
            ))
        })?;
        if !(100..=599).contains(&code) {
            return Err(runtime_error(format!(
                "routing_policy.failover.{key}: {code} is not a valid HTTP status (100..=599)"
            )));
        }
        out.push(code as u16);
    }
    Ok(out)
}

fn split_target(target: &str) -> Option<(String, String)> {
    let target = target.trim();
    let (provider, model) = target.split_once(':')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        None
    } else {
        Some((provider.to_string(), model.to_string()))
    }
}

fn parse_chain_link(value: &VmValue, idx: usize) -> Result<ChainLink, VmError> {
    let dict = match value {
        VmValue::Dict(dict) => dict.clone(),
        VmValue::String(target) => {
            let (provider, model) = split_target(target).ok_or_else(|| {
                runtime_error(format!(
                    "routing_policy.chain[{idx}]: expected {{provider, model}} dict or \"provider:model\" string, got {value:?}"
                ))
            })?;
            return Ok(ChainLink {
                provider,
                model,
                timeout_ms: None,
                label: None,
                region: None,
                overrides: None,
            });
        }
        other => {
            return Err(runtime_error(format!(
                "routing_policy.chain[{idx}]: expected dict or string, got {}",
                other.type_name()
            )));
        }
    };
    let provider = dict
        .get("provider")
        .map(|v| v.display())
        .unwrap_or_default()
        .trim()
        .to_string();
    let model = dict
        .get("model")
        .map(|v| v.display())
        .unwrap_or_default()
        .trim()
        .to_string();
    if provider.is_empty() || model.is_empty() {
        return Err(runtime_error(format!(
            "routing_policy.chain[{idx}]: both provider and model are required (got provider={provider:?}, model={model:?})"
        )));
    }
    let timeout_ms = parse_pos_u64(&dict, "timeout_ms")?;
    let label_text = parse_label(&dict, "label")?;
    let label = if label_text.is_empty() {
        None
    } else {
        Some(label_text)
    };
    let region_text = parse_label(&dict, "region")?;
    let region = if region_text.is_empty() {
        None
    } else {
        Some(region_text)
    };
    Ok(ChainLink {
        provider,
        model,
        timeout_ms,
        label,
        region,
        overrides: None,
    })
}

fn parse_failover(value: Option<&VmValue>) -> Result<FailoverRules, VmError> {
    let Some(value) = value else {
        return Ok(FailoverRules::default());
    };
    let dict = match value {
        VmValue::Nil => return Ok(FailoverRules::default()),
        VmValue::Dict(dict) => dict.clone(),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.failover: expected dict, got {}",
                other.type_name()
            )));
        }
    };
    Ok(FailoverRules {
        on_status: parse_status_list(&dict, "on_status")?,
        on_timeout_ms: parse_pos_u64(&dict, "on_timeout_ms")?,
        on_error_kinds: parse_string_list(&dict, "on_error_kinds")?,
        on_no_dispatch: false,
        max_attempts: parse_pos_usize(&dict, "max_attempts")?,
    })
}

fn parse_latency(value: Option<&VmValue>) -> Result<LatencyRules, VmError> {
    let Some(value) = value else {
        return Ok(LatencyRules::default());
    };
    let dict = match value {
        VmValue::Nil => return Ok(LatencyRules::default()),
        VmValue::Dict(dict) => dict.clone(),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.latency: expected dict, got {}",
                other.type_name()
            )));
        }
    };
    Ok(LatencyRules {
        target_p95_ms: parse_pos_u64(&dict, "target_p95_ms")?,
        race_after_ms: parse_pos_u64(&dict, "race_after_ms")?,
    })
}

fn parse_budget(value: Option<&VmValue>) -> Result<BudgetRules, VmError> {
    let Some(value) = value else {
        return Ok(BudgetRules::default());
    };
    let dict = match value {
        VmValue::Nil => return Ok(BudgetRules::default()),
        VmValue::Dict(dict) => dict.clone(),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.budget: expected dict, got {}",
                other.type_name()
            )));
        }
    };
    let on_exceed = match dict.get("on_exceed") {
        Some(VmValue::Nil) | None => None,
        Some(VmValue::String(s)) => Some(BudgetExceedAction::parse(s)?),
        Some(other) => {
            return Err(runtime_error(format!(
                "routing_policy.budget.on_exceed: expected a string, got {}",
                other.type_name()
            )));
        }
    };
    Ok(BudgetRules {
        per_call_usd: parse_pos_f64(&dict, "per_call_usd")?,
        session_usd: parse_pos_f64(&dict, "session_usd")?,
        on_exceed,
    })
}

fn parse_observe(value: Option<&VmValue>) -> Result<ObserveRules, VmError> {
    let Some(value) = value else {
        return Ok(ObserveRules::default());
    };
    let dict = match value {
        VmValue::Nil => return Ok(ObserveRules::default()),
        VmValue::Dict(dict) => dict.clone(),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.observe: expected dict, got {}",
                other.type_name()
            )));
        }
    };
    let emit_event = match dict.get("emit_event") {
        Some(VmValue::String(s)) => {
            let text = s.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(runtime_error(format!(
                "routing_policy.observe.emit_event: expected a string, got {}",
                other.type_name()
            )));
        }
    };
    Ok(ObserveRules { emit_event })
}

/// Validate a user-facing config dict and return a tagged copy. The
/// tag (`__routing_policy__: true`) is what `llm_call` looks for when
/// it decides whether to dispatch through the routing executor.
/// One resolved rung of a `models:`/`ladder:` ladder, before it is lowered
/// to a [`ChainLink`]. `provider` may be unset here and inferred later.
struct LadderStep {
    model: String,
    provider: Option<String>,
    label: Option<String>,
    overrides: Option<Arc<crate::value::DictMap>>,
}

/// Build a routing policy from a `models:` (inline steps) or `ladder:`
/// (named catalog ladder) option. A model ladder lowers onto the SAME
/// first-class routing chain that `routing_policy(...)` drives, so it
/// inherits — with zero duplication — the exact transport-failover
/// classification (`matches_failover`: 429/5xx/timeout/circuit_open, never
/// schema/auth/4xx), the per-attempt `routing` envelope trace, and the
/// schema-retry composition in `call.rs` (schema failures re-ask the same
/// rung; they do not advance the ladder).
///
/// Returns `Ok(None)` when neither option is present, so callers can fall
/// through to their existing single-model / explicit-routing path.
pub(crate) fn build_model_ladder_policy(
    options: &crate::value::DictMap,
    base_provider: &str,
    base_model: &str,
) -> Result<Option<Arc<RoutingPolicyConfig>>, VmError> {
    let has_models = matches!(options.get("models"), Some(v) if !matches!(v, VmValue::Nil));
    let has_ladder = matches!(options.get("ladder"), Some(v) if !matches!(v, VmValue::Nil));
    if !has_models && !has_ladder {
        return Ok(None);
    }
    if has_models && has_ladder {
        return Err(runtime_error(
            "llm_call: `models:` and `ladder:` are mutually exclusive — pass an \
             inline step list OR a named catalog ladder, not both"
                .to_string(),
        ));
    }

    let (steps, label) = if has_ladder {
        resolve_named_ladder(options.get("ladder"))?
    } else {
        parse_inline_ladder_steps(options.get("models"))?
    };
    if steps.is_empty() {
        return Err(runtime_error(
            "llm_call: model ladder must list at least one step".to_string(),
        ));
    }

    let mut chain = Vec::with_capacity(steps.len());
    for step in &steps {
        chain.push(ladder_step_to_link(step, base_provider, base_model));
    }

    Ok(Some(Arc::new(RoutingPolicyConfig {
        failover: FailoverRules {
            // One transport attempt per rung: try every step once, advancing
            // only on transport-class failures. Empty explicit rules let
            // `matches_failover` use the default 429/5xx/timeout/circuit set.
            max_attempts: Some(chain.len()),
            ..FailoverRules::default()
        },
        latency: LatencyRules::default(),
        budget: BudgetRules::default(),
        observe: ObserveRules::default(),
        escalate_on: Vec::new(),
        max_refines_per_link: 0,
        label,
        chain,
        is_ladder: true,
    })))
}

/// Parse the inline `models:` list into ladder steps. Accepts either
/// `"model"` / `"provider:model"` strings (sugar) or
/// `{model, provider?, options?, label?}` dicts.
fn parse_inline_ladder_steps(
    value: Option<&VmValue>,
) -> Result<(Vec<LadderStep>, String), VmError> {
    let items = match value {
        Some(VmValue::List(items)) => items.clone(),
        Some(other) => {
            return Err(runtime_error(format!(
                "llm_call: `models:` expects a list of steps, got {}",
                other.type_name()
            )));
        }
        None => return Ok((Vec::new(), String::new())),
    };
    let mut steps = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        steps.push(parse_ladder_step_value(item, idx)?);
    }
    let label = format!("model_ladder(steps={})", steps.len());
    Ok((steps, label))
}

/// Parse a single inline ladder step (string sugar or dict).
fn parse_ladder_step_value(value: &VmValue, idx: usize) -> Result<LadderStep, VmError> {
    match value {
        VmValue::String(raw) => {
            let (provider, model) = split_ladder_target(raw);
            if model.is_empty() {
                return Err(runtime_error(format!(
                    "llm_call: `models:`[{idx}] is an empty model id"
                )));
            }
            Ok(LadderStep {
                model,
                provider,
                label: None,
                overrides: None,
            })
        }
        VmValue::Dict(dict) => {
            // Treat a nil field as absent (avoids the literal string "nil").
            let non_nil = |key: &str| -> Option<String> {
                dict.get(key)
                    .filter(|v| !matches!(v, VmValue::Nil))
                    .map(|v| v.display().trim().to_string())
                    .filter(|s| !s.is_empty())
            };
            let model = non_nil("model").unwrap_or_default();
            if model.is_empty() {
                return Err(runtime_error(format!(
                    "llm_call: `models:`[{idx}] requires a non-empty `model` field"
                )));
            }
            let provider = non_nil("provider");
            let label = non_nil("label");
            let overrides = parse_ladder_step_overrides(dict.get("options"), idx)?;
            Ok(LadderStep {
                model,
                provider,
                label,
                overrides,
            })
        }
        other => Err(runtime_error(format!(
            "llm_call: `models:`[{idx}] must be a model string or {{model, provider?, options?}} dict, got {}",
            other.type_name()
        ))),
    }
}

/// Validate a ladder step's `options` override dict. Every key must be in
/// [`LADDER_STEP_OVERRIDE_KEYS`]; an unknown key errors here rather than
/// being silently dropped at dispatch time.
fn parse_ladder_step_overrides(
    value: Option<&VmValue>,
    idx: usize,
) -> Result<Option<Arc<crate::value::DictMap>>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Dict(dict)) => {
            for key in dict.keys() {
                if !LADDER_STEP_OVERRIDE_KEYS.contains(&key.as_str()) {
                    return Err(runtime_error(format!(
                        "llm_call: `models:`[{idx}].options key {key:?} is not a supported \
                         per-step override; supported keys are {LADDER_STEP_OVERRIDE_KEYS:?}. \
                         Put structural options (tools, schema, thinking) on the base call."
                    )));
                }
            }
            Ok(Some(Arc::new(dict.as_ref().clone())))
        }
        Some(other) => Err(runtime_error(format!(
            "llm_call: `models:`[{idx}].options must be a dict, got {}",
            other.type_name()
        ))),
    }
}

/// Convert a catalog `[model_ladders.*]` step's `options` table into the same
/// per-step override [`DictMap`] that inline `models:` steps produce, running
/// the identical [`LADDER_STEP_OVERRIDE_KEYS`] allowlist. An unknown key errors
/// loudly here rather than being silently dropped, so catalog and inline
/// ladders behave the same way.
fn catalog_step_overrides(
    options: Option<&std::collections::BTreeMap<String, toml::Value>>,
    ladder_name: &str,
    idx: usize,
) -> Result<Option<Arc<crate::value::DictMap>>, VmError> {
    let Some(options) = options.filter(|o| !o.is_empty()) else {
        return Ok(None);
    };
    let mut map = crate::value::DictMap::new();
    for (key, value) in options {
        if !LADDER_STEP_OVERRIDE_KEYS.contains(&key.as_str()) {
            return Err(runtime_error(format!(
                "model ladder {ladder_name:?} step {idx}: options key {key:?} is not a \
                 supported per-step override; supported keys are {LADDER_STEP_OVERRIDE_KEYS:?}. \
                 Put structural options (tools, schema, thinking) on the base call."
            )));
        }
        // toml::Value -> serde_json::Value -> VmValue reuses the canonical
        // JSON bridge (numbers/bools/strings map cleanly for the scalar
        // override allowlist).
        let json = serde_json::to_value(value).map_err(|e| {
            runtime_error(format!(
                "model ladder {ladder_name:?} step {idx}: options key {key:?} is not \
                 representable ({e})"
            ))
        })?;
        map.insert(
            crate::value::intern_key(key),
            crate::schema::json_to_vm_value(&json),
        );
    }
    Ok(Some(Arc::new(map)))
}

/// Resolve a named `[model_ladders.<name>]` catalog ladder into steps.
fn resolve_named_ladder(value: Option<&VmValue>) -> Result<(Vec<LadderStep>, String), VmError> {
    let name = match value {
        Some(VmValue::String(s)) => s.trim().to_string(),
        Some(other) => {
            return Err(runtime_error(format!(
                "llm_call: `ladder:` expects the name of a catalog ladder (string), got {}",
                other.type_name()
            )));
        }
        None => return Ok((Vec::new(), String::new())),
    };
    let Some(def) = crate::llm_config::model_ladder(&name) else {
        let known = crate::llm_config::model_ladder_names();
        let hint = if known.is_empty() {
            " (no ladders are declared in the catalog)".to_string()
        } else {
            format!(" (known ladders: {})", known.join(", "))
        };
        return Err(runtime_error(format!(
            "llm_call: no model ladder named {name:?} in the catalog{hint}"
        )));
    };
    let mut steps = Vec::with_capacity(def.steps.len());
    for (idx, s) in def.steps.into_iter().enumerate() {
        // Catalog steps honor per-step `options` overrides identically to
        // inline `models:` steps (previously they were silently dropped).
        let overrides = catalog_step_overrides(s.options.as_ref(), &name, idx)?;
        steps.push(LadderStep {
            model: s.model,
            provider: s.provider,
            label: s.label,
            overrides,
        });
    }
    let label = def.label.unwrap_or_else(|| format!("model_ladder:{name}"));
    Ok((steps, label))
}

/// Lower a resolved [`LadderStep`] to a [`ChainLink`], resolving model
/// aliases and inferring the provider when the step left it unset.
fn ladder_step_to_link(step: &LadderStep, base_provider: &str, _base_model: &str) -> ChainLink {
    let (resolved_model, alias_provider) = crate::llm_config::resolve_model(&step.model);
    let provider = step.provider.clone().or(alias_provider).unwrap_or_else(|| {
        crate::llm::provider::infer_provider_from_model_id(&resolved_model, base_provider).provider
    });
    ChainLink {
        provider,
        model: resolved_model,
        timeout_ms: None,
        label: step.label.clone(),
        region: None,
        overrides: step.overrides.clone(),
    }
}

/// Split a `models:` string step into an optional provider + model. Only
/// treats a `prefix:rest` split as `provider:model` when `prefix` is a
/// registered provider name — otherwise the whole string is a model id
/// (avoids mis-splitting colon-bearing ids such as Ollama image tags).
fn split_ladder_target(raw: &str) -> (Option<String>, String) {
    let raw = raw.trim();
    if let Some((prefix, rest)) = raw.split_once(':') {
        let prefix = prefix.trim();
        let rest = rest.trim();
        if !prefix.is_empty()
            && !rest.is_empty()
            && crate::llm::provider::is_provider_registered(prefix)
        {
            return (Some(prefix.to_string()), rest.to_string());
        }
    }
    (None, raw.to_string())
}

pub(crate) fn build_routing_policy(config: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let chain_value = config.get("chain").ok_or_else(|| {
        runtime_error("routing_policy: `chain` is required (list of {provider, model})".to_string())
    })?;
    let chain_items = match chain_value {
        VmValue::List(items) => items.clone(),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.chain: expected a list, got {}",
                other.type_name()
            )));
        }
    };
    if chain_items.is_empty() {
        return Err(runtime_error(
            "routing_policy.chain: at least one {provider, model} entry is required".to_string(),
        ));
    }
    let mut chain = Vec::with_capacity(chain_items.len());
    for (idx, item) in chain_items.iter().enumerate() {
        chain.push(parse_chain_link(item, idx)?);
    }
    let failover = parse_failover(config.get("failover"))?;
    let latency = parse_latency(config.get("latency"))?;
    let budget = parse_budget(config.get("budget"))?;
    let observe = parse_observe(config.get("observe"))?;
    let escalate_on = parse_escalate_on(config.get("escalate_on"))?;
    let max_refines_per_link = match config.get("max_refines_per_link") {
        None | Some(VmValue::Nil) => 1usize,
        Some(VmValue::Int(n)) if *n >= 0 => *n as usize,
        Some(other) => {
            return Err(runtime_error(format!(
                "routing_policy.max_refines_per_link: expected a non-negative integer, got {}",
                other.type_name()
            )));
        }
    };
    let label_text = parse_label(config, "label")?;
    let label = if label_text.is_empty() {
        format!("routing_policy(chain={})", chain.len())
    } else {
        label_text
    };

    let mut summary = BTreeMap::new();
    summary.insert(ROUTING_POLICY_TAG.to_string(), VmValue::Bool(true));
    summary.put_str("label", label.clone());
    summary.insert("chain".to_string(), chain_summary_value(&chain));
    if budget.envelope().is_some() {
        summary.insert("budget".to_string(), budget_value(&budget));
    }
    summary.insert("failover".to_string(), failover_value(&failover));
    summary.insert("latency".to_string(), latency_value(&latency));
    summary.insert("observe".to_string(), observe_value(&observe));
    if !escalate_on.is_empty() {
        summary.insert("escalate_on".to_string(), verifiers_summary(&escalate_on));
        summary.insert(
            "max_refines_per_link".to_string(),
            VmValue::Int(max_refines_per_link as i64),
        );
    }

    let parsed = RoutingPolicyConfig {
        chain,
        failover,
        latency,
        budget,
        observe,
        escalate_on,
        max_refines_per_link,
        label,
        is_ladder: false,
    };
    let handle = intern_policy(parsed);
    summary.insert(HANDLE_KEY.to_string(), VmValue::Int(handle as i64));
    Ok(VmValue::dict(summary))
}

fn chain_summary_value(chain: &[ChainLink]) -> VmValue {
    let items: Vec<VmValue> = chain
        .iter()
        .map(|link| {
            let mut dict = BTreeMap::new();
            dict.put_str("provider", link.provider.clone());
            dict.put_str("model", link.model.clone());
            if let Some(timeout) = link.timeout_ms {
                dict.insert("timeout_ms".to_string(), VmValue::Int(timeout as i64));
            }
            if let Some(label) = &link.label {
                dict.put_str("label", label.clone());
            }
            if let Some(region) = &link.region {
                dict.put_str("region", region.clone());
            }
            VmValue::dict(dict)
        })
        .collect();
    VmValue::List(std::sync::Arc::new(items))
}

fn failover_value(failover: &FailoverRules) -> VmValue {
    let mut dict = BTreeMap::new();
    let statuses: Vec<VmValue> = failover
        .on_status
        .iter()
        .map(|s| VmValue::Int(*s as i64))
        .collect();
    dict.insert(
        "on_status".to_string(),
        VmValue::List(std::sync::Arc::new(statuses)),
    );
    let kinds: Vec<VmValue> = failover
        .on_error_kinds
        .iter()
        .map(|s| VmValue::String(arcstr::ArcStr::from(s.clone())))
        .collect();
    dict.insert(
        "on_error_kinds".to_string(),
        VmValue::List(std::sync::Arc::new(kinds)),
    );
    if let Some(ms) = failover.on_timeout_ms {
        dict.insert("on_timeout_ms".to_string(), VmValue::Int(ms as i64));
    }
    if failover.on_no_dispatch {
        dict.insert("on_no_dispatch".to_string(), VmValue::Bool(true));
    }
    if let Some(max) = failover.max_attempts {
        dict.insert("max_attempts".to_string(), VmValue::Int(max as i64));
    }
    VmValue::dict(dict)
}

fn latency_value(latency: &LatencyRules) -> VmValue {
    let mut dict = BTreeMap::new();
    if let Some(ms) = latency.target_p95_ms {
        dict.insert("target_p95_ms".to_string(), VmValue::Int(ms as i64));
    }
    if let Some(ms) = latency.race_after_ms {
        dict.insert("race_after_ms".to_string(), VmValue::Int(ms as i64));
    }
    VmValue::dict(dict)
}

fn budget_value(budget: &BudgetRules) -> VmValue {
    let mut dict = BTreeMap::new();
    if let Some(v) = budget.per_call_usd {
        dict.insert("per_call_usd".to_string(), VmValue::Float(v));
    }
    if let Some(v) = budget.session_usd {
        dict.insert("session_usd".to_string(), VmValue::Float(v));
    }
    dict.put_str("on_exceed", budget.on_exceed_or_abort().as_str());
    VmValue::dict(dict)
}

fn observe_value(observe: &ObserveRules) -> VmValue {
    let mut dict = BTreeMap::new();
    if let Some(event) = &observe.emit_event {
        dict.put_str("emit_event", event.clone());
    }
    VmValue::dict(dict)
}

/// Pull the parsed config off a dict produced by `routing_policy(...)`.
/// Returns `None` when the dict isn't tagged so callers can fall back to
/// the historical resolution path; returns an error when the tag is
/// present but the handle is missing (corruption indicator).
pub(crate) fn extract_routing_policy(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<Arc<RoutingPolicyConfig>>, VmError> {
    let Some(opts) = options else {
        return Ok(None);
    };
    let Some(value) = opts.get("routing") else {
        return Ok(None);
    };
    let dict = match value {
        VmValue::Nil | VmValue::Bool(false) => return Ok(None),
        VmValue::Dict(dict) => dict,
        other => {
            return Err(runtime_error(format!(
                "llm_call(... routing: ...): expected a routing_policy(...) value, got {}",
                other.type_name()
            )));
        }
    };
    match dict.get(ROUTING_POLICY_TAG) {
        Some(VmValue::Bool(true)) => {}
        _ => {
            return Err(runtime_error(
                "llm_call(... routing: ...): pass the result of routing_policy({...}); the routing key does not accept a bare dict".to_string(),
            ));
        }
    }
    let handle = dict
        .get(HANDLE_KEY)
        .and_then(|v| v.as_int())
        .ok_or_else(|| {
            runtime_error(
                "llm_call(... routing: ...): routing policy handle missing — re-create it with routing_policy({...})".to_string(),
            )
        })?;
    let policy = lookup_policy(handle as u64).ok_or_else(|| {
        runtime_error(
            "llm_call(... routing: ...): routing policy handle expired — re-create it with routing_policy({...})".to_string(),
        )
    })?;
    Ok(Some(policy))
}

/// Per-step generation-parameter overrides honored by a `models:` ladder
/// step's `options` dict. Kept deliberately small: these are the scalar
/// knobs a per-rung override sensibly tweaks (a stronger rung may want more
/// tokens / lower temperature). Structural options (tools, schema, vision,
/// thinking mode) belong on the base call and are validated once there; the
/// ladder builder rejects any override key outside this set so nothing is
/// silently dropped.
const LADDER_STEP_OVERRIDE_KEYS: &[&str] = &[
    "temperature",
    "max_tokens",
    "top_p",
    "top_k",
    "seed",
    "frequency_penalty",
    "presence_penalty",
    "timeout_ms",
    "fast",
];

mod execution;
#[cfg(test)]
mod tests;

pub(crate) use execution::{
    execute_with_routing, provider_exhausted_error, trace_to_decision, trace_to_vm_attempts,
    AttemptStatus, RoutingAttempt, RoutingErrorSnapshot, RoutingTrace, VerifierOutcome,
};

#[cfg(test)]
use execution::{
    matches_failover, physical_request_attempt_count, provider_exhausted_routing_error,
    TerminalRoute,
};
