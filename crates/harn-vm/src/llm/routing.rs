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
use std::time::Duration;

use serde_json::json;

use crate::value::{ErrorCategory, VmError, VmValue};

use super::api::{LlmCallOptions, LlmRoutePolicy};
use super::cost::{calculate_cost_for_provider, peek_total_cost, LlmBudgetEnvelope};
use super::routing_verifier::{
    build_refine_nudge, parse_escalate_on, run_verifier, verifiers_summary, Verifier,
    VerifierSignal,
};

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
) -> Option<Arc<RoutingPolicyConfig>> {
    if max_routes < 2 {
        return None;
    }

    let mut chain = vec![ChainLink {
        provider: provider.to_string(),
        model: model.to_string(),
        timeout_ms: None,
        label: Some("primary".to_string()),
        region: None,
    }];

    for (candidate_model, candidate) in crate::llm_config::equivalent_model_catalog_entries(model) {
        if chain.len() >= max_routes {
            break;
        }
        if candidate.provider == provider {
            continue;
        }
        if chain
            .iter()
            .any(|link| link.provider == candidate.provider && link.model == candidate_model)
        {
            continue;
        }
        if super::helpers::resolve_api_key(&candidate.provider).is_err() {
            continue;
        }
        chain.push(ChainLink {
            provider: candidate.provider.clone(),
            model: candidate_model,
            timeout_ms: None,
            label: Some(format!("equivalent:{}", candidate.provider)),
            region: None,
        });
    }

    if chain.len() < 2 {
        return None;
    }

    let label = format!("equivalent_failover({provider}:{model})");
    Some(Arc::new(RoutingPolicyConfig {
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
    }))
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

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// What `execute_with_routing` returns about each attempt so the
/// caller can re-emit it as a `routing_decision` block on the user-
/// facing result envelope.
#[derive(Clone, Debug)]
pub(crate) struct RoutingTrace {
    pub label: String,
    pub attempts: Vec<RoutingAttempt>,
    pub selected: Option<usize>,
    pub session_cost_usd: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingAttempt {
    pub index: usize,
    pub provider: String,
    pub model: String,
    pub label: String,
    pub status: AttemptStatus,
    pub duration_ms: u64,
    pub cost_usd: Option<f64>,
    /// Token counts attributed to this attempt's provider call. Mirrors
    /// the winning `LlmResult.input_tokens` / `output_tokens` so
    /// downstream graders can re-compute spend against alternate
    /// pricing tables (e.g. OpenRouter, which isn't in the catalog).
    /// `None` when the attempt was skipped, race-lost, or budget-aborted
    /// before the provider returned a payload.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub error: Option<RoutingErrorSnapshot>,
    /// Per-verifier signals emitted after this attempt's response.
    /// Empty when the policy has no `escalate_on` chain or the
    /// attempt failed before verifiers ran.
    pub verifier_signals: Vec<VerifierSignalRecord>,
    /// What the verifier chain told the router to do with this
    /// candidate. `Accept` means the answer was returned;
    /// `Refine`/`Escalate` mean the router moved on (within the same
    /// link or to the next link respectively).
    pub verifier_outcome: Option<VerifierOutcome>,
}

/// One signal entry in `RoutingAttempt.verifier_signals`. Kept simple
/// so it survives serialization through `VmValue` dicts without losing
/// the verifier's name and the human-readable reason.
#[derive(Clone, Debug)]
pub(crate) struct VerifierSignalRecord {
    pub name: String,
    pub kind: String,
    pub signal: String,
    pub reason: Option<String>,
}

/// Aggregated verdict across the verifier chain for one attempt. Drives
/// receipt rendering ("escalated because verifier=refine on attempt 1,
/// escalate on attempt 2").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerifierOutcome {
    Accept,
    Refine,
    Escalate,
}

impl VerifierOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Refine => "refine",
            Self::Escalate => "escalate",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AttemptStatus {
    Succeeded,
    Failed,
    Skipped,
    /// Cancelled because a concurrent racer won.
    RaceLost,
}

impl AttemptStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::RaceLost => "race_lost",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingErrorSnapshot {
    pub category: String,
    pub message: String,
    pub status: Option<u16>,
}

/// Match an error against the policy's failover rules. Returns true
/// when the error is eligible to advance the chain.
fn matches_failover(rules: &FailoverRules, error: &VmError) -> (bool, RoutingErrorSnapshot) {
    let category = crate::value::error_to_category(error);
    let message = match error {
        VmError::CategorizedError { message, .. } => message.clone(),
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| error.to_string()),
        _ => error.to_string(),
    };
    let status = extract_status_code(error);
    let snapshot = RoutingErrorSnapshot {
        category: category.as_str().to_string(),
        message,
        status,
    };

    if rules.on_no_dispatch && is_no_dispatch_contract_violation(&snapshot.message) {
        return (true, snapshot);
    }

    if let Some(code) = status {
        if rules.on_status.contains(&code) {
            return (true, snapshot);
        }
    }

    if matches!(category, ErrorCategory::Timeout) && rules.on_timeout_ms.is_some() {
        return (true, snapshot);
    }

    let category_label = category.as_str();
    let kind_match = rules.on_error_kinds.iter().any(|kind| {
        let normalized = kind.trim().to_ascii_lowercase();
        if normalized == category_label {
            return true;
        }
        matches!(
            (normalized.as_str(), category.clone()),
            ("rate_limit", ErrorCategory::RateLimit)
                | ("overloaded", ErrorCategory::Overloaded)
                | ("transient", ErrorCategory::TransientNetwork)
                | ("transient_network", ErrorCategory::TransientNetwork)
                | ("network", ErrorCategory::TransientNetwork)
                | ("timeout", ErrorCategory::Timeout)
                | ("schema_validation", ErrorCategory::SchemaValidation)
                | ("auth", ErrorCategory::Auth)
                | ("provider_error", ErrorCategory::ServerError)
                | ("server_error", ErrorCategory::ServerError)
                | ("provider_5xx", ErrorCategory::ServerError)
                | ("generic", ErrorCategory::Generic)
                | ("budget_exceeded", ErrorCategory::BudgetExceeded)
                | ("circuit_open", ErrorCategory::CircuitOpen)
                | ("egress_blocked", ErrorCategory::EgressBlocked)
                | ("cancelled", ErrorCategory::Cancelled)
                | ("tool_error", ErrorCategory::ToolError)
                | ("tool_rejected", ErrorCategory::ToolRejected)
                | ("not_found", ErrorCategory::NotFound)
        )
    });
    if kind_match {
        return (true, snapshot);
    }

    // Sensible defaults: 429 / 5xx and transient categories are always
    // failover-eligible when the script didn't write explicit rules.
    let defaults_active = rules.on_status.is_empty()
        && rules.on_error_kinds.is_empty()
        && rules.on_timeout_ms.is_none();
    if defaults_active {
        let by_status = status
            .map(|code| DEFAULT_FAILOVER_STATUSES.contains(&code))
            .unwrap_or(false);
        let by_category = matches!(
            category,
            ErrorCategory::RateLimit
                | ErrorCategory::Overloaded
                | ErrorCategory::TransientNetwork
                | ErrorCategory::Timeout
                | ErrorCategory::ServerError
        );
        if by_status || by_category {
            return (true, snapshot);
        }
    }

    (false, snapshot)
}

fn is_no_dispatch_contract_violation(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("returned billed output")
        && lower.contains("completion_tokens=")
        && lower.contains("no dispatchable tool call or answer")
        && lower.contains("upstream contract violation")
}

fn extract_status_code(error: &VmError) -> Option<u16> {
    let message = error.to_string();
    extract_status_from_text(&message)
}

fn extract_status_from_text(message: &str) -> Option<u16> {
    let lowered = message.to_ascii_lowercase();
    let needles = ["http ", "status_code: ", "status: ", "status "];
    for needle in needles.iter() {
        if let Some(idx) = lowered.find(needle) {
            let tail = &message[idx + needle.len()..];
            if let Some(code) = parse_leading_status(tail) {
                return Some(code);
            }
        }
    }
    parse_leading_status(message)
}

fn parse_leading_status(text: &str) -> Option<u16> {
    let text = text.trim_start();
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
}

fn emit_routing_event(
    dispatch: &str,
    event: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) {
    let category = format!("{dispatch}.{event}");
    let mut meta: BTreeMap<String, serde_json::Value> = metadata.into_iter().collect();
    meta.entry("event".to_string())
        .or_insert_with(|| serde_json::Value::String(event.to_string()));
    crate::events::log_info_meta(&category, "", meta);
}

fn budget_overrun_snapshot(
    cap: f64,
    projected: f64,
    session: f64,
    kind: &str,
) -> RoutingErrorSnapshot {
    RoutingErrorSnapshot {
        category: "budget_exceeded".to_string(),
        message: format!(
            "{kind} budget exceeded (cap=${cap:.6}, projected=${projected:.6}, session=${session:.6})"
        ),
        status: None,
    }
}

/// Effective per-link options: start from the base options, swap in
/// provider/model/timeout, and overlay the policy budget so per-call
/// preflight enforces it.
fn link_options(
    base: &LlmCallOptions,
    policy: &RoutingPolicyConfig,
    link: &ChainLink,
) -> LlmCallOptions {
    let mut opts = base.clone();
    opts.provider = link.provider.clone();
    opts.model = link.model.clone();
    opts.region = link.region.clone();
    opts.api_key = String::new();
    opts.route_policy = LlmRoutePolicy::Always(format!("{}:{}", link.provider, link.model));
    opts.fallback_chain = Vec::new();
    opts.route_fallbacks = Vec::new();
    opts.routing_decision = None;
    // The executor owns chain dispatch; per-link calls must not recurse
    // back through `execute_llm_call`'s routing dispatch.
    opts.routing_policy = None;
    if let Some(timeout_ms) = link.timeout_ms.or(policy.failover.on_timeout_ms) {
        let secs = (timeout_ms / 1000).max(1);
        opts.timeout = Some(secs);
    }
    if let Some(envelope) = policy.budget.envelope() {
        let mut merged = opts.budget.clone().unwrap_or_default();
        if envelope.max_cost_usd.is_some() {
            merged.max_cost_usd = envelope.max_cost_usd;
        }
        if envelope.total_budget_usd.is_some() {
            merged.total_budget_usd = envelope.total_budget_usd;
        }
        opts.budget = Some(merged);
    }
    if let Ok(key) = super::helpers::resolve_api_key(&link.provider) {
        opts.api_key = key;
    }
    opts
}

/// Pre-flight budget check that handles the policy's `on_exceed` knob.
/// Returns `Ok(true)` when the call may proceed, `Ok(false)` when the
/// caller should skip this link and try the next one, and `Err(_)` for
/// abort (caller surfaces the standard budget-exceeded error).
fn check_link_budget(
    policy: &RoutingPolicyConfig,
    opts: &LlmCallOptions,
    dispatch: &str,
    attempt_idx: usize,
    link_label: &str,
    trace_attempts: &mut Vec<RoutingAttempt>,
) -> Result<bool, (VmError, RoutingErrorSnapshot)> {
    let Some(rules_envelope) = policy.budget.envelope() else {
        return Ok(true);
    };
    let session_cost = peek_total_cost();
    let projection = super::cost::project_llm_call_cost(opts, session_cost);
    let action = policy.budget.on_exceed_or_abort();

    let mut breach = None::<(super::cost::BudgetLimitKind, f64, &'static str)>;
    if let Some(max) = rules_envelope.max_cost_usd {
        if projection.projected_cost_usd > max {
            breach = Some((super::cost::BudgetLimitKind::PerCallCost, max, "per_call"));
        }
    }
    if breach.is_none() {
        if let Some(max) = rules_envelope.total_budget_usd {
            if session_cost + projection.projected_cost_usd > max {
                breach = Some((super::cost::BudgetLimitKind::TotalCost, max, "session"));
            }
        }
    }

    let Some((limit_kind, limit_value, kind_label)) = breach else {
        return Ok(true);
    };

    let snapshot = budget_overrun_snapshot(
        limit_value,
        projection.projected_cost_usd,
        session_cost,
        kind_label,
    );

    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempt".to_string(), json!(attempt_idx));
    meta.insert("provider".to_string(), json!(opts.provider.clone()));
    meta.insert("model".to_string(), json!(opts.model.clone()));
    meta.insert("link_label".to_string(), json!(link_label));
    meta.insert("kind".to_string(), json!(kind_label));
    meta.insert("limit_usd".to_string(), json!(limit_value));
    meta.insert(
        "projected_cost_usd".to_string(),
        json!(projection.projected_cost_usd),
    );
    meta.insert("session_cost_usd".to_string(), json!(session_cost));
    meta.insert("on_exceed".to_string(), json!(action.as_str()));
    emit_routing_event(dispatch, "budget_exceeded", meta);

    match action {
        BudgetExceedAction::Abort => Err((
            super::cost::budget_exceeded_error(&projection, limit_kind, limit_value),
            snapshot,
        )),
        BudgetExceedAction::Skip => {
            trace_attempts.push(RoutingAttempt {
                index: attempt_idx,
                provider: opts.provider.clone(),
                model: opts.model.clone(),
                label: link_label.to_string(),
                status: AttemptStatus::Skipped,
                duration_ms: 0,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                error: Some(snapshot),
                verifier_signals: Vec::new(),
                verifier_outcome: None,
            });
            Ok(false)
        }
        BudgetExceedAction::Warn => Ok(true),
    }
}

fn project_link_cost_usd(result: &super::api::LlmResult) -> f64 {
    calculate_cost_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
    )
}

fn duration_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Lightweight wrapper that observes one provider call. We deliberately
/// bypass the standard transient-retry loop because the policy itself
/// owns retry semantics (max_attempts across the chain plus per-link
/// failover rules); doubling them up here would slow the chain past
/// the user-visible budget.
async fn execute_link(
    opts: &LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<super::api::LlmResult, VmError> {
    let retry_config = super::agent_observe::LlmRetryConfig {
        retries: 0,
        backoff_ms: 0,
    };
    super::agent_observe::observed_llm_call(
        opts,
        None,
        bridge,
        &retry_config,
        None,
        false,
        bridge.is_some(),
        None,
    )
    .await
}

fn pending_attempt_record(
    attempt_no: usize,
    link: &ChainLink,
    label: &str,
    elapsed: Duration,
) -> RoutingAttempt {
    RoutingAttempt {
        index: attempt_no,
        provider: link.provider.clone(),
        model: link.model.clone(),
        label: label.to_string(),
        // Caller patches this to Succeeded on success or attaches the
        // error snapshot for failure.
        status: AttemptStatus::Failed,
        duration_ms: duration_ms(elapsed),
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: None,
        verifier_signals: Vec::new(),
        verifier_outcome: None,
    }
}

/// Extract the text the verifier chain should inspect. Mirrors how
/// `agent_config::build_llm_call_result` derives the human-visible
/// answer so the verifier sees the same thing the script will.
fn candidate_text_for_verifier(result: &super::api::LlmResult) -> String {
    if !result.text.is_empty() {
        return result.text.clone();
    }
    // Tool-only responses still get a deterministic text payload
    // verifiers can match against (their JSON serialization), so
    // `lint`-style pattern rules work on tool-call shape too.
    if !result.tool_calls.is_empty() {
        return serde_json::to_string(&result.tool_calls).unwrap_or_default();
    }
    String::new()
}

/// Run the verifier chain over a candidate. Returns the per-verifier
/// records plus the aggregated outcome. The first non-`accept` signal
/// dominates: a single `escalate` outranks any later `refine`s, so
/// callers don't have to redo the precedence reasoning.
async fn run_verifier_chain(
    verifiers: &[Verifier],
    text: &str,
) -> (Vec<VerifierSignalRecord>, VerifierOutcome, Vec<String>) {
    if verifiers.is_empty() {
        return (Vec::new(), VerifierOutcome::Accept, Vec::new());
    }
    let mut records = Vec::with_capacity(verifiers.len());
    let mut outcome = VerifierOutcome::Accept;
    let mut refine_reasons: Vec<String> = Vec::new();
    for verifier in verifiers {
        let signal = run_verifier(verifier, text).await;
        let signal_label = signal.as_str().to_string();
        let reason = signal.reason().map(str::to_string);
        records.push(VerifierSignalRecord {
            name: verifier.name().to_string(),
            kind: verifier.kind_label().to_string(),
            signal: signal_label,
            reason: reason.clone(),
        });
        match signal {
            VerifierSignal::Accept => {}
            VerifierSignal::Refine { reason } => {
                if outcome == VerifierOutcome::Accept {
                    outcome = VerifierOutcome::Refine;
                }
                refine_reasons.push(reason);
            }
            VerifierSignal::Escalate { .. } => {
                outcome = VerifierOutcome::Escalate;
                // Don't `break` — we still want the full picture for
                // receipts, but no later refine can downgrade escalate.
            }
        }
    }
    (records, outcome, refine_reasons)
}

/// Default `max_attempts` budget when `escalate_on` is configured but
/// the script didn't set one explicitly. Without this nudge, the
/// default `chain.len()` would silently disable refine retries (each
/// link only gets one shot, leaving no slot for a tightened-prompt
/// retry). Adding `chain.len() * max_refines_per_link` keeps the
/// "happy path = once per link" semantics intact while making room
/// for the refine fan-out the verifier chain expects.
fn implied_max_attempts(policy: &RoutingPolicyConfig) -> usize {
    let base = policy.chain.len();
    if policy.escalate_on.is_empty() {
        base
    } else {
        base + base.saturating_mul(policy.max_refines_per_link)
    }
}

fn emit_verifier_signal_event(
    dispatch: &str,
    policy: &RoutingPolicyConfig,
    attempt_no: usize,
    link: &ChainLink,
    outcome: VerifierOutcome,
    signals: &[VerifierSignalRecord],
) {
    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempt".to_string(), json!(attempt_no));
    meta.insert("provider".to_string(), json!(link.provider.clone()));
    meta.insert("model".to_string(), json!(link.model.clone()));
    meta.insert("link_label".to_string(), json!(link.display_label()));
    meta.insert("outcome".to_string(), json!(outcome.as_str()));
    meta.insert(
        "signals".to_string(),
        serde_json::Value::Array(
            signals
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "kind": s.kind,
                        "signal": s.signal,
                        "reason": s.reason,
                    })
                })
                .collect(),
        ),
    );
    emit_routing_event(dispatch, "verifier_signal", meta);
}

/// Run the chain. Each link is tried in order; failover rules decide
/// whether to advance after an error. `latency.race_after_ms`, when
/// set, kicks off the next attempt in parallel and returns whichever
/// finishes first; the loser is cancelled and recorded as `race_lost`.
///
/// When `policy.escalate_on` is non-empty, each successful link's
/// candidate runs through the verifier chain before being returned:
/// `accept` returns, `refine` retries the same link with a nudge
/// (up to `policy.max_refines_per_link` per link), `escalate`
/// advances to the next link. If the verifier rejects the last link
/// and no frontier remains, the rejected candidate is returned anyway
/// — verifiers gate routing, not correctness.
pub(crate) async fn execute_with_routing(
    policy: &RoutingPolicyConfig,
    mut base_opts: LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<(super::api::LlmResult, RoutingTrace), VmError> {
    let dispatch = policy.dispatch_label();
    let mut trace = RoutingTrace {
        label: policy.label.clone(),
        attempts: Vec::new(),
        selected: None,
        session_cost_usd: peek_total_cost(),
    };
    let max_attempts = policy
        .failover
        .max_attempts
        .unwrap_or_else(|| implied_max_attempts(policy));
    if max_attempts == 0 {
        return Err(runtime_error(
            "routing_policy.failover.max_attempts: must be >= 1".to_string(),
        ));
    }
    let mut last_error: Option<VmError> = None;
    let mut last_snapshot: Option<RoutingErrorSnapshot> = None;
    let mut attempts_used: usize = 0;
    let original_messages = base_opts.messages.clone();
    // Per-link refine bookkeeping. Both reset on `idx` advance so a
    // refine streak from one link doesn't bleed into the next.
    let mut refines_for_current_link: usize = 0;
    let mut nudge_reasons_for_current_link: Vec<String> = Vec::new();
    // Last verifier-rejected candidate, returned as a fallback when
    // the chain exhausts without an `accept`.
    let mut last_rejected_candidate: Option<(super::api::LlmResult, usize)> = None;

    let mut decision_meta = serde_json::Map::new();
    decision_meta.insert("policy".to_string(), json!(policy.label.clone()));
    decision_meta.insert("chain_length".to_string(), json!(policy.chain.len()));
    decision_meta.insert("max_attempts".to_string(), json!(max_attempts));
    decision_meta.insert(
        "chain".to_string(),
        serde_json::Value::Array(
            policy
                .chain
                .iter()
                .map(|link| {
                    json!({
                        "provider": link.provider,
                        "model": link.model,
                        "label": link.display_label(),
                    })
                })
                .collect(),
        ),
    );
    emit_routing_event(&dispatch, "decision", decision_meta);

    let mut idx = 0usize;
    while idx < policy.chain.len() && attempts_used < max_attempts {
        let link = policy.chain[idx].clone();
        let opts = link_options(&base_opts, policy, &link);
        let link_label = link.display_label();

        let mut local_attempts: Vec<RoutingAttempt> = Vec::new();
        match check_link_budget(
            policy,
            &opts,
            &dispatch,
            attempts_used + 1,
            &link_label,
            &mut local_attempts,
        ) {
            Ok(true) => {}
            Ok(false) => {
                trace.attempts.extend(local_attempts);
                idx += 1;
                attempts_used += 1;
                continue;
            }
            Err((err, snapshot)) => {
                trace.attempts.extend(local_attempts);
                last_error = Some(err);
                last_snapshot = Some(snapshot);
                break;
            }
        }
        trace.attempts.extend(local_attempts);

        let attempt_no = attempts_used + 1;
        let start = std::time::Instant::now();
        let mut attempt_meta = serde_json::Map::new();
        attempt_meta.insert("policy".to_string(), json!(policy.label.clone()));
        attempt_meta.insert("attempt".to_string(), json!(attempt_no));
        attempt_meta.insert("provider".to_string(), json!(link.provider.clone()));
        attempt_meta.insert("model".to_string(), json!(link.model.clone()));
        attempt_meta.insert("link_label".to_string(), json!(link_label.clone()));
        emit_routing_event(&dispatch, "attempt", attempt_meta);

        let race_after_ms = policy.latency.race_after_ms;
        let primary_timeout_ms = link
            .timeout_ms
            .or(policy.failover.on_timeout_ms)
            .unwrap_or(DEFAULT_RACE_PRIMARY_TIMEOUT_MS);

        let race_outcome = if let Some(race_after) = race_after_ms {
            if idx + 1 < policy.chain.len() && attempts_used + 2 <= max_attempts {
                let backup_link = policy.chain[idx + 1].clone();
                let backup_opts = link_options(&base_opts, policy, &backup_link);
                let backup_label = backup_link.display_label();
                Some(
                    run_race(
                        &dispatch,
                        policy,
                        attempts_used,
                        &link,
                        &link_label,
                        &opts,
                        bridge,
                        race_after,
                        primary_timeout_ms,
                        backup_label,
                        backup_opts,
                    )
                    .await,
                )
            } else {
                None
            }
        } else {
            None
        };

        let (result, mut attempt_records) = if let Some(outcome) = race_outcome {
            outcome
        } else {
            let result = execute_link(&opts, bridge).await;
            (
                result,
                vec![pending_attempt_record(
                    attempt_no,
                    &link,
                    &link_label,
                    start.elapsed(),
                )],
            )
        };
        // Each record in `attempt_records` is one chain slot consumed
        // (1 for a normal call, 2 when racing actually started a backup).
        // Counting from the record vec keeps `max_attempts` honest and
        // prevents the chain from re-trying the same backup on the next
        // iteration.
        let consumed = attempt_records.len().max(1);

        match result {
            Ok(value) => {
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Failed) && rec.error.is_none())
                {
                    record.status = AttemptStatus::Succeeded;
                    record.cost_usd = Some(project_link_cost_usd(&value));
                    record.input_tokens = Some(value.input_tokens);
                    record.output_tokens = Some(value.output_tokens);
                }
                // Run the verifier chain over the winning candidate.
                let candidate_text = if policy.escalate_on.is_empty() {
                    String::new()
                } else {
                    candidate_text_for_verifier(&value)
                };
                let (signals, outcome, refine_reasons) =
                    run_verifier_chain(&policy.escalate_on, &candidate_text).await;
                let outcome_for_attempt = if policy.escalate_on.is_empty() {
                    None
                } else {
                    Some(outcome)
                };
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Succeeded))
                {
                    record.verifier_signals = signals.clone();
                    record.verifier_outcome = outcome_for_attempt;
                }
                if !policy.escalate_on.is_empty() {
                    emit_verifier_signal_event(
                        &dispatch, policy, attempt_no, &link, outcome, &signals,
                    );
                }
                let starting_len = trace.attempts.len();
                trace.attempts.extend(attempt_records);
                let success_idx = trace
                    .attempts
                    .iter()
                    .enumerate()
                    .skip(starting_len)
                    .find(|(_, a)| {
                        matches!(a.status, AttemptStatus::Succeeded)
                            && a.provider == value.provider
                            && a.model == value.model
                    })
                    .map(|(idx, _)| idx);

                match outcome {
                    VerifierOutcome::Accept => {
                        trace.selected = success_idx;
                        trace.session_cost_usd = peek_total_cost();
                        return Ok((value, trace));
                    }
                    VerifierOutcome::Refine
                        if refines_for_current_link < policy.max_refines_per_link
                            && attempts_used + consumed < max_attempts =>
                    {
                        // Same link, tightened prompt: append the
                        // cumulative refine nudge to the original
                        // messages snapshot (re-applying each iteration
                        // keeps prior bad responses out of context).
                        nudge_reasons_for_current_link.extend(refine_reasons);
                        let nudge = build_refine_nudge(&nudge_reasons_for_current_link);
                        base_opts.messages = original_messages.clone();
                        if !nudge.is_empty() {
                            base_opts.messages.push(serde_json::json!({
                                "role": "user",
                                "content": nudge,
                            }));
                        }
                        refines_for_current_link += 1;
                        attempts_used += consumed;
                        if let Some(idx_v) = success_idx {
                            last_rejected_candidate = Some((value, idx_v));
                        }
                        continue;
                    }
                    VerifierOutcome::Refine | VerifierOutcome::Escalate => {
                        // Either refine budget is exhausted (treat as
                        // escalate) or verifier escalated outright:
                        // advance to the next link, reset link-local
                        // refine state.
                        refines_for_current_link = 0;
                        nudge_reasons_for_current_link.clear();
                        base_opts.messages = original_messages.clone();
                        attempts_used += consumed;
                        idx += consumed;
                        if let Some(idx_v) = success_idx {
                            last_rejected_candidate = Some((value, idx_v));
                        }
                        continue;
                    }
                }
            }
            Err(err) => {
                let (eligible, snapshot) = matches_failover(&policy.failover, &err);
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Failed) && rec.error.is_none())
                {
                    record.error = Some(snapshot.clone());
                }
                trace.attempts.extend(attempt_records);
                last_snapshot = Some(snapshot);
                attempts_used += consumed;
                if !eligible {
                    last_error = Some(err);
                    break;
                }
                last_error = Some(err);
                idx += consumed;
                continue;
            }
        }
    }

    // Verifier-rejected fallback: if the chain exhausted because the
    // verifier kept escalating but we never got a transport error,
    // return the last successful candidate the chain produced rather
    // than failing the call. The verifier complaint is preserved on
    // `routing.attempts[*].verifier_outcome` so the caller can still
    // see why escalation ran out.
    if last_error.is_none() {
        if let Some((value, idx)) = last_rejected_candidate {
            trace.selected = Some(idx);
            trace.session_cost_usd = peek_total_cost();
            return Ok((value, trace));
        }
    }

    let err = last_error.unwrap_or_else(|| {
        runtime_error("routing_policy: chain exhausted with no attempts (empty chain?)".to_string())
    });
    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempts".to_string(), json!(trace.attempts.len()));
    if let Some(snapshot) = last_snapshot {
        meta.insert("last_error_category".to_string(), json!(snapshot.category));
        meta.insert("last_error_message".to_string(), json!(snapshot.message));
        if let Some(status) = snapshot.status {
            meta.insert("last_error_status".to_string(), json!(status));
        }
    }
    emit_routing_event(&dispatch, "exhausted", meta);
    Err(err)
}

#[allow(clippy::too_many_arguments)]
async fn run_race(
    dispatch: &str,
    policy: &RoutingPolicyConfig,
    attempts_used: usize,
    link: &ChainLink,
    link_label: &str,
    opts: &LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    race_after_ms: u64,
    primary_timeout_ms: u64,
    backup_label: String,
    backup_opts: LlmCallOptions,
) -> (Result<super::api::LlmResult, VmError>, Vec<RoutingAttempt>) {
    let primary_start = std::time::Instant::now();
    let primary_attempt_no = attempts_used + 1;
    let backup_attempt_no = attempts_used + 2;

    let primary_link = link.clone();
    let primary_label = link_label.to_string();
    let primary_opts = opts.clone();

    let mut primary_future = Box::pin(async move {
        let res = execute_link(&primary_opts, bridge).await;
        (res, primary_start.elapsed())
    });

    tokio::select! {
        biased;
        primary = &mut primary_future => {
            let (res, elapsed) = primary;
            let mut record = pending_attempt_record(
                primary_attempt_no,
                &primary_link,
                &primary_label,
                elapsed,
            );
            if let Ok(ref v) = res {
                record.status = AttemptStatus::Succeeded;
                record.cost_usd = Some(project_link_cost_usd(v));
                record.input_tokens = Some(v.input_tokens);
                record.output_tokens = Some(v.output_tokens);
            }
            (res, vec![record])
        }
        _ = crate::clock_mock::sleep(Duration::from_millis(race_after_ms)) => {
            let mut race_meta = serde_json::Map::new();
            race_meta.insert("policy".to_string(), json!(policy.label.clone()));
            race_meta.insert("race_after_ms".to_string(), json!(race_after_ms));
            race_meta.insert("primary_label".to_string(), json!(primary_label.clone()));
            race_meta.insert("backup_label".to_string(), json!(backup_label.clone()));
            emit_routing_event(dispatch, "race_started", race_meta);

            let backup_start = std::time::Instant::now();
            let backup_link_clone = ChainLink {
                provider: backup_opts.provider.clone(),
                model: backup_opts.model.clone(),
                timeout_ms: link.timeout_ms,
                label: Some(backup_label.clone()),
                region: backup_opts.region.clone(),
            };
            let mut backup_future = Box::pin({
                let backup_opts = backup_opts.clone();
                async move {
                    let res = execute_link(&backup_opts, bridge).await;
                    (res, backup_start.elapsed())
                }
            });

            let primary_deadline = primary_timeout_ms.saturating_add(race_after_ms);

            tokio::select! {
                biased;
                primary = &mut primary_future => {
                    let (res, elapsed) = primary;
                    let mut primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        elapsed,
                    );
                    if let Ok(ref v) = res {
                        primary_record.status = AttemptStatus::Succeeded;
                        primary_record.cost_usd = Some(project_link_cost_usd(v));
                        primary_record.input_tokens = Some(v.input_tokens);
                        primary_record.output_tokens = Some(v.output_tokens);
                    }
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        backup_start.elapsed(),
                    );
                    backup_record.status = AttemptStatus::RaceLost;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("winner".to_string(), json!(primary_label));
                    meta.insert("loser".to_string(), json!(backup_label));
                    emit_routing_event(dispatch, "race_won", meta.clone());
                    let mut lost_meta = meta;
                    lost_meta.insert("reason".to_string(), json!("primary_finished_first"));
                    emit_routing_event(dispatch, "race_lost", lost_meta);
                    (res, vec![primary_record, backup_record])
                }
                backup = &mut backup_future => {
                    let (res, elapsed) = backup;
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        elapsed,
                    );
                    if let Ok(ref v) = res {
                        backup_record.status = AttemptStatus::Succeeded;
                        backup_record.cost_usd = Some(project_link_cost_usd(v));
                        backup_record.input_tokens = Some(v.input_tokens);
                        backup_record.output_tokens = Some(v.output_tokens);
                    }
                    let mut primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        primary_start.elapsed(),
                    );
                    primary_record.status = AttemptStatus::RaceLost;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("winner".to_string(), json!(backup_label));
                    meta.insert("loser".to_string(), json!(primary_label));
                    emit_routing_event(dispatch, "race_won", meta.clone());
                    let mut lost_meta = meta;
                    lost_meta.insert("reason".to_string(), json!("backup_finished_first"));
                    emit_routing_event(dispatch, "race_lost", lost_meta);
                    (res, vec![primary_record, backup_record])
                }
                _ = crate::clock_mock::sleep(Duration::from_millis(primary_deadline)) => {
                    let primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        Duration::from_millis(primary_deadline),
                    );
                    let backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        Duration::from_millis(primary_deadline),
                    );
                    (
                        Err(runtime_error(
                            "routing_policy: race exhausted both primary and backup attempts".to_string(),
                        )),
                        vec![primary_record, backup_record],
                    )
                }
            }
        }
    }
}

/// Encode the routing trace into the standard `LlmRoutingDecision`
/// shape so the result envelope, transcript provider_payload, and
/// portal all see one consistent schema.
pub(crate) fn trace_to_decision(
    trace: &RoutingTrace,
    policy: &RoutingPolicyConfig,
) -> super::api::LlmRoutingDecision {
    use super::api::{LlmRouteAlternative, LlmRoutingDecision};

    let mut alternatives = Vec::with_capacity(trace.attempts.len());
    for (idx, attempt) in trace.attempts.iter().enumerate() {
        let selected = trace.selected == Some(idx);
        let reason = match attempt.status {
            AttemptStatus::Succeeded => match attempt.verifier_outcome {
                Some(VerifierOutcome::Refine) => {
                    if selected {
                        "selected:verifier_refine_fallback".to_string()
                    } else {
                        "verifier:refine".to_string()
                    }
                }
                Some(VerifierOutcome::Escalate) => {
                    if selected {
                        "selected:verifier_escalate_fallback".to_string()
                    } else {
                        "verifier:escalate".to_string()
                    }
                }
                Some(VerifierOutcome::Accept) | None => "selected".to_string(),
            },
            AttemptStatus::Failed => attempt
                .error
                .as_ref()
                .map(|e| format!("failed:{}", e.category))
                .unwrap_or_else(|| "failed".to_string()),
            AttemptStatus::Skipped => "skipped:budget".to_string(),
            AttemptStatus::RaceLost => "race_lost".to_string(),
        };
        let quality_tier = crate::llm_config::model_tier(&attempt.model);
        let pricing = super::cost::pricing_per_1k_for(&attempt.provider, &attempt.model);
        alternatives.push(LlmRouteAlternative {
            available: true,
            cost_per_1k_in: pricing.map(|p| p.0),
            cost_per_1k_out: pricing.map(|p| p.1),
            latency_p50_ms: super::cost::latency_p50_ms_for(&attempt.provider),
            provider: attempt.provider.clone(),
            model: attempt.model.clone(),
            quality_tier,
            selected,
            reason,
        });
    }
    let selected_idx = trace.selected.unwrap_or(0);
    let (selected_provider, selected_model) = trace
        .attempts
        .get(selected_idx)
        .map(|a| (a.provider.clone(), a.model.clone()))
        .unwrap_or_else(|| {
            policy
                .chain
                .first()
                .map(|link| (link.provider.clone(), link.model.clone()))
                .unwrap_or_default()
        });
    LlmRoutingDecision {
        policy: format!("routing_policy({})", policy.label),
        requested_quality: None,
        selected_provider,
        selected_model,
        alternatives,
    }
}

/// Convert routing trace attempts into a list of dicts suitable for
/// surfacing on the user-facing result envelope under
/// `routing.attempts`.
pub(crate) fn trace_to_vm_attempts(trace: &RoutingTrace) -> VmValue {
    let items: Vec<VmValue> = trace
        .attempts
        .iter()
        .map(|attempt| {
            let mut dict = BTreeMap::new();
            dict.insert("index".to_string(), VmValue::Int(attempt.index as i64));
            dict.put_str("provider", attempt.provider.clone());
            dict.put_str("model", attempt.model.clone());
            dict.put_str("label", attempt.label.clone());
            dict.put_str("status", attempt.status.as_str());
            dict.insert(
                "duration_ms".to_string(),
                VmValue::Int(attempt.duration_ms as i64),
            );
            if let Some(cost) = attempt.cost_usd {
                dict.insert("cost_usd".to_string(), VmValue::Float(cost));
            }
            if let Some(tokens) = attempt.input_tokens {
                dict.insert("input_tokens".to_string(), VmValue::Int(tokens));
            }
            if let Some(tokens) = attempt.output_tokens {
                dict.insert("output_tokens".to_string(), VmValue::Int(tokens));
            }
            if let Some(error) = &attempt.error {
                let mut err_dict = BTreeMap::new();
                err_dict.put_str("category", error.category.clone());
                err_dict.put_str("message", error.message.clone());
                if let Some(status) = error.status {
                    err_dict.insert("status".to_string(), VmValue::Int(status as i64));
                }
                dict.insert("error".to_string(), VmValue::dict(err_dict));
            }
            if let Some(outcome) = attempt.verifier_outcome {
                dict.put_str("verifier_outcome", outcome.as_str());
            }
            if !attempt.verifier_signals.is_empty() {
                let signals: Vec<VmValue> = attempt
                    .verifier_signals
                    .iter()
                    .map(|signal| {
                        let mut sig_dict = BTreeMap::new();
                        sig_dict.put_str("name", signal.name.clone());
                        sig_dict.put_str("kind", signal.kind.clone());
                        sig_dict.put_str("signal", signal.signal.clone());
                        if let Some(reason) = &signal.reason {
                            sig_dict.put_str("reason", reason.clone());
                        }
                        VmValue::dict(sig_dict)
                    })
                    .collect();
                dict.insert(
                    "verifier_signals".to_string(),
                    VmValue::List(std::sync::Arc::new(signals)),
                );
            }
            VmValue::dict(dict)
        })
        .collect();
    VmValue::List(std::sync::Arc::new(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(items: &[(&str, VmValue)]) -> crate::value::DictMap {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn build_routing_policy_validates_chain() {
        clear_policy_registry();
        let config = dict(&[
            (
                "chain",
                VmValue::List(std::sync::Arc::new(vec![
                    VmValue::String(arcstr::ArcStr::from("mock:mock")),
                    VmValue::dict(dict(&[
                        ("provider", VmValue::String(arcstr::ArcStr::from("mock"))),
                        ("model", VmValue::String(arcstr::ArcStr::from("mock-2"))),
                    ])),
                ])),
            ),
            (
                "failover",
                VmValue::dict(dict(&[
                    (
                        "on_status",
                        VmValue::List(std::sync::Arc::new(vec![
                            VmValue::Int(429),
                            VmValue::Int(500),
                        ])),
                    ),
                    ("max_attempts", VmValue::Int(2)),
                ])),
            ),
            (
                "budget",
                VmValue::dict(dict(&[
                    ("per_call_usd", VmValue::Float(0.5)),
                    ("on_exceed", VmValue::String(arcstr::ArcStr::from("abort"))),
                ])),
            ),
        ]);
        let tagged = build_routing_policy(&config).expect("validates");
        let inner = tagged.as_dict().expect("dict");
        assert!(matches!(
            inner.get(ROUTING_POLICY_TAG),
            Some(VmValue::Bool(true))
        ));
        assert!(inner.contains_key(HANDLE_KEY));
        let handle = inner.get(HANDLE_KEY).and_then(|v| v.as_int()).unwrap();
        let policy = lookup_policy(handle as u64).expect("policy registered");
        assert_eq!(policy.chain.len(), 2);
        assert_eq!(policy.failover.on_status, vec![429, 500]);
    }

    #[test]
    fn chain_link_region_parses_summarizes_and_threads_into_options() {
        clear_policy_registry();
        let config = dict(&[(
            "chain",
            VmValue::List(std::sync::Arc::new(vec![
                // Link 0: explicit region override.
                VmValue::dict(dict(&[
                    ("provider", VmValue::String(arcstr::ArcStr::from("bedrock"))),
                    (
                        "model",
                        VmValue::String(arcstr::ArcStr::from("anthropic.claude-3-5-sonnet-v2:0")),
                    ),
                    ("region", VmValue::String(arcstr::ArcStr::from("eu-west-1"))),
                ])),
                // Link 1: no region -> falls back to env at call time.
                VmValue::String(arcstr::ArcStr::from("mock:mock")),
            ])),
        )]);
        let tagged = build_routing_policy(&config).expect("validates");
        let inner = tagged.as_dict().expect("dict");

        // Parsed chain carries the region on link 0 and None on link 1.
        let handle = inner.get(HANDLE_KEY).and_then(|v| v.as_int()).unwrap();
        let policy = lookup_policy(handle as u64).expect("policy registered");
        assert_eq!(policy.chain[0].region.as_deref(), Some("eu-west-1"));
        assert_eq!(policy.chain[1].region, None);

        // The summary dict echoes the region back for introspection,
        // and only on the link that set it.
        let chain_summary = match inner.get("chain") {
            Some(VmValue::List(items)) => items.clone(),
            other => panic!("expected chain list, got {other:?}"),
        };
        let link0 = chain_summary[0].as_dict().expect("link0 dict");
        assert_eq!(
            link0.get("region").and_then(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            }),
            Some("eu-west-1".to_string())
        );
        let link1 = chain_summary[1].as_dict().expect("link1 dict");
        assert!(!link1.contains_key("region"));

        // link_options threads the region into the per-link call options;
        // the region-less link resolves to None (env fallback).
        let base = crate::llm::api::options::base_opts("bedrock");
        let with_region = link_options(&base, &policy, &policy.chain[0]);
        assert_eq!(with_region.region.as_deref(), Some("eu-west-1"));
        let without_region = link_options(&base, &policy, &policy.chain[1]);
        assert_eq!(without_region.region, None);
    }

    #[test]
    fn build_rejects_empty_chain() {
        clear_policy_registry();
        let config = dict(&[("chain", VmValue::List(std::sync::Arc::new(Vec::new())))]);
        let err = build_routing_policy(&config).unwrap_err();
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(message.contains("at least one"));
    }

    #[test]
    fn build_rejects_invalid_status_code() {
        clear_policy_registry();
        let config = dict(&[
            (
                "chain",
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    arcstr::ArcStr::from("mock:mock"),
                )])),
            ),
            (
                "failover",
                VmValue::dict(dict(&[(
                    "on_status",
                    VmValue::List(std::sync::Arc::new(vec![VmValue::Int(42)])),
                )])),
            ),
        ]);
        let err = build_routing_policy(&config).unwrap_err();
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(message.contains("not a valid HTTP status"));
    }

    #[test]
    fn matches_failover_default_status() {
        let rules = FailoverRules::default();
        let err = VmError::Runtime("HTTP 429 rate limit".to_string());
        let (eligible, snap) = matches_failover(&rules, &err);
        assert!(eligible);
        assert_eq!(snap.status, Some(429));
    }

    #[test]
    fn matches_failover_explicit_kind() {
        let rules = FailoverRules {
            on_error_kinds: vec!["rate_limit".to_string()],
            ..Default::default()
        };
        let err = VmError::CategorizedError {
            message: "throttled".to_string(),
            category: ErrorCategory::RateLimit,
        };
        let (eligible, _) = matches_failover(&rules, &err);
        assert!(eligible);
    }

    #[test]
    fn no_dispatch_contract_violation_does_not_failover_by_default() {
        let rules = FailoverRules::default();
        let err = VmError::Runtime(
            "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly"
                .to_string(),
        );

        let (eligible, _) = matches_failover(&rules, &err);

        assert!(!eligible);
    }

    #[test]
    fn no_dispatch_contract_violation_can_opt_into_failover() {
        let rules = FailoverRules {
            on_no_dispatch: true,
            ..Default::default()
        };
        let err = VmError::Runtime(
            "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly"
                .to_string(),
        );

        let (eligible, snap) = matches_failover(&rules, &err);

        assert!(eligible);
        assert!(snap.message.contains("upstream contract violation"));
    }

    #[test]
    fn no_dispatch_matcher_requires_billed_completion_token_contract_shape() {
        let rules = FailoverRules {
            on_no_dispatch: true,
            ..Default::default()
        };
        let cases = [
            "returned billed output with no dispatchable tool call or answer \
             (upstream contract violation)",
            "returned billed output (completion_tokens=12) with no answer \
             (upstream contract violation)",
            "returned billed output (completion_tokens=12) with no dispatchable tool call or answer",
            "completion_tokens=12 with no dispatchable tool call or answer \
             (upstream contract violation)",
        ];

        for message in cases {
            let (eligible, _) = matches_failover(&rules, &VmError::Runtime(message.to_string()));
            assert!(!eligible, "message should not be eligible: {message}");
        }
    }

    #[test]
    fn explicit_failover_kind_does_not_implicitly_match_timeout() {
        let rules = FailoverRules {
            on_error_kinds: vec!["rate_limit".to_string()],
            ..Default::default()
        };
        let err = VmError::CategorizedError {
            message: "timed out".to_string(),
            category: ErrorCategory::Timeout,
        };
        let (eligible, _) = matches_failover(&rules, &err);
        assert!(!eligible);
    }

    #[test]
    fn rejects_non_failover_error_by_default() {
        let rules = FailoverRules::default();
        let err = VmError::CategorizedError {
            message: "schema mismatch".to_string(),
            category: ErrorCategory::SchemaValidation,
        };
        let (eligible, _) = matches_failover(&rules, &err);
        assert!(!eligible);
    }

    #[test]
    fn budget_envelope_round_trips() {
        let budget = BudgetRules {
            per_call_usd: Some(0.25),
            session_usd: Some(5.0),
            on_exceed: Some(BudgetExceedAction::Skip),
        };
        let envelope = budget.envelope().unwrap();
        assert_eq!(envelope.max_cost_usd, Some(0.25));
        assert_eq!(envelope.total_budget_usd, Some(5.0));
    }
}
