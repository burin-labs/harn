//! Per-session compaction policy declarations.
//!
//! `harn-serve` consumers (TUI, IDE, cloud) used to maintain their own
//! "when do I compact?" logic alongside the engine. This registry lifts
//! that decision into the runtime so any caller can `compaction_policy(...)`
//! once and then `compaction_check(session_id)` / `compaction_run(...)`
//! on every turn without re-encoding thresholds at the call site.
//!
//! Policies are thread-local because `agent_sessions` is. A `None` lookup
//! falls back to the default policy registered with the empty session key.
//! Policies are *additive* — registering one replaces any prior entry for
//! the same session id.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::thread_local;

use serde::{Deserialize, Serialize};

use super::{compact_strategy_name, parse_compact_strategy, CompactStrategy, CompactionPolicy};
use crate::value::VmValue;

/// Default ratio of the model's context window at which compaction fires
/// when `max_tokens` isn't set explicitly. Matches the TUI default
/// (`BURIN_TUI_COMPACTION_THRESHOLD_RATIO`) so lifting policy into harn
/// doesn't shift the firing point for existing surfaces.
pub const DEFAULT_SAFETY_RATIO: f64 = 0.7;

/// User-facing strategy names. These map to engine [`CompactStrategy`]s
/// through [`CompactionPolicyDeclaration::engine_strategy`] so the policy
/// surface can stay stable while the underlying engine evolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyStrategy {
    /// LLM summarization of archived messages.
    Summarize,
    /// LLM summarization, with a deterministic truncate fallback on error.
    SummarizeThenPrune,
    /// Keep the head and tail verbatim; compact only the middle.
    /// Maps to the engine truncate strategy with both `keep_first` and
    /// `keep_last` set.
    HeadAndTail,
    /// Rolling window: drop everything older than `keep_last`. Maps to the
    /// engine truncate strategy with `keep_first = 0`.
    Window,
    /// Deterministic observation masking — keeps short results verbatim,
    /// masks long tool outputs. Cheapest, no LLM round-trip.
    ObservationMask,
    /// Caller-supplied closure decides the summary.
    Custom,
}

impl PolicyStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarize => "summarize",
            Self::SummarizeThenPrune => "summarize-then-prune",
            Self::HeadAndTail => "head+tail",
            Self::Window => "window",
            Self::ObservationMask => "observation_mask",
            Self::Custom => "custom",
        }
    }

    /// Translate a policy-level strategy name into the engine strategy
    /// that backs it. Accepts both the policy-level aliases
    /// (`summarize`, `summarize-then-prune`, ...) and the raw engine
    /// names (`llm`, `truncate`, `observation_mask`, `custom`) so callers
    /// can mix vocabularies during the policy migration.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "summarize" | "llm" => Ok(Self::Summarize),
            "summarize-then-prune" | "summarize_then_prune" => Ok(Self::SummarizeThenPrune),
            "head+tail" | "head-tail" | "head_tail" => Ok(Self::HeadAndTail),
            "window" | "truncate" => Ok(Self::Window),
            "observation_mask" | "observation-mask" | "mask" => Ok(Self::ObservationMask),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "unknown compaction policy strategy '{other}' (expected one of: summarize, \
                 summarize-then-prune, head+tail, window, observation_mask, custom)"
            )),
        }
    }

    /// The engine strategy used for the primary compaction run.
    pub fn engine_strategy(self) -> CompactStrategy {
        match self {
            Self::Summarize | Self::SummarizeThenPrune => CompactStrategy::Llm,
            Self::HeadAndTail | Self::Window => CompactStrategy::Truncate,
            Self::ObservationMask => CompactStrategy::ObservationMask,
            Self::Custom => CompactStrategy::Custom,
        }
    }

    /// Engine fallback used when the primary strategy fails. Currently
    /// only `summarize-then-prune` declares one — everything else returns
    /// `None`, deferring to the engine's per-call decision.
    pub fn engine_fallback(self) -> Option<CompactStrategy> {
        match self {
            Self::SummarizeThenPrune => Some(CompactStrategy::Truncate),
            _ => None,
        }
    }
}

/// Declared inputs that drive the `compaction_check` decision and the
/// downstream `compaction_run` call. Values are normalized at registration
/// time so consumers can rely on every threshold being either populated
/// or explicitly `None`.
#[derive(Clone, Debug)]
pub struct CompactionPolicyDeclaration {
    pub strategy: PolicyStrategy,
    /// Hard cap on estimated tokens before `compaction_check` returns
    /// `compact_now`. `None` means tokens alone never trigger.
    pub max_tokens: Option<usize>,
    /// Hard cap on message count. `None` means message count alone never
    /// triggers.
    pub max_turns: Option<usize>,
    /// When `context_window` is set, `compaction_check` fires when
    /// estimated tokens exceed `context_window * safety_ratio`. Both must
    /// be set for the ratio rule to apply.
    pub context_window: Option<usize>,
    pub safety_ratio: f64,
    /// How many recent messages to keep verbatim during compaction.
    pub keep_last: usize,
    /// How many initial messages to keep verbatim during compaction.
    pub keep_first: usize,
    /// Token budget passed through to the engine for tier-2 compaction
    /// (used by `summarize-then-prune` to escalate from LLM to truncate
    /// when the LLM result still exceeds the cap).
    pub hard_limit_tokens: Option<usize>,
    /// Per-tool-result microcompaction threshold. `None` keeps the engine
    /// default.
    pub tool_output_max_chars: Option<usize>,
    /// Closure invoked when `strategy` is `custom`.
    pub summarize_fn: Option<VmValue>,
    /// Optional prompt template path used when the engine selects LLM
    /// summarization.
    pub summarize_prompt: Option<String>,
    /// Author/scope/preserve/drop directives that the engine threads
    /// through the LLM compaction prompt and persisted metadata.
    pub instructions: CompactionPolicy,
}

impl Default for CompactionPolicyDeclaration {
    fn default() -> Self {
        Self {
            strategy: PolicyStrategy::SummarizeThenPrune,
            max_tokens: None,
            max_turns: None,
            context_window: None,
            safety_ratio: DEFAULT_SAFETY_RATIO,
            keep_last: 12,
            keep_first: 0,
            hard_limit_tokens: None,
            tool_output_max_chars: None,
            summarize_fn: None,
            summarize_prompt: None,
            instructions: CompactionPolicy::default(),
        }
    }
}

impl CompactionPolicyDeclaration {
    /// Token budget at which the policy considers a session "full".
    /// Resolves the most restrictive of `max_tokens` and
    /// `context_window * safety_ratio`; returns `None` when neither rule
    /// is configured.
    pub fn token_threshold(&self) -> Option<usize> {
        let ratio_threshold = self.context_window.map(|window| {
            let raw = (window as f64) * self.safety_ratio;
            if raw.is_finite() && raw > 0.0 {
                raw.floor() as usize
            } else {
                window
            }
        });
        match (self.max_tokens, ratio_threshold) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Decision metadata describing every triggered threshold. Returned
    /// as part of the [`CompactionDecision`] so callers can log what
    /// pushed them over the line.
    pub fn evaluate(&self, estimated_tokens: usize, message_count: usize) -> EvaluationContext {
        let token_threshold = self.token_threshold();
        let token_trigger = token_threshold.is_some_and(|cap| estimated_tokens > cap);
        let turn_trigger = self
            .max_turns
            .is_some_and(|cap| cap > 0 && message_count > cap);
        EvaluationContext {
            token_threshold,
            token_trigger,
            turn_trigger,
            estimated_tokens,
            message_count,
            strategy: self.strategy,
        }
    }

    /// JSON snapshot for telemetry payloads.
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "strategy".to_string(),
            serde_json::Value::String(self.strategy.as_str().to_string()),
        );
        map.insert(
            "engine_strategy".to_string(),
            serde_json::Value::String(
                compact_strategy_name(&self.strategy.engine_strategy()).to_string(),
            ),
        );
        if let Some(value) = self.max_tokens {
            map.insert("max_tokens".to_string(), serde_json::json!(value));
        }
        if let Some(value) = self.max_turns {
            map.insert("max_turns".to_string(), serde_json::json!(value));
        }
        if let Some(value) = self.context_window {
            map.insert("context_window".to_string(), serde_json::json!(value));
        }
        map.insert(
            "safety_ratio".to_string(),
            serde_json::json!(self.safety_ratio),
        );
        map.insert("keep_last".to_string(), serde_json::json!(self.keep_last));
        if self.keep_first > 0 {
            map.insert("keep_first".to_string(), serde_json::json!(self.keep_first));
        }
        if let Some(value) = self.hard_limit_tokens {
            map.insert("hard_limit_tokens".to_string(), serde_json::json!(value));
        }
        if let Some(value) = self.tool_output_max_chars {
            map.insert(
                "tool_output_max_chars".to_string(),
                serde_json::json!(value),
            );
        }
        if let Some(threshold) = self.token_threshold() {
            map.insert("token_threshold".to_string(), serde_json::json!(threshold));
        }
        if let Some(policy_json) = self.instructions.metadata_json() {
            map.insert("instructions".to_string(), policy_json);
        }
        serde_json::Value::Object(map)
    }
}

/// Outcome of an [`CompactionPolicyDeclaration::evaluate`] pass, used by
/// `compaction_check` to assemble the user-facing decision dict.
#[derive(Clone, Debug)]
pub struct EvaluationContext {
    pub token_threshold: Option<usize>,
    pub token_trigger: bool,
    pub turn_trigger: bool,
    pub estimated_tokens: usize,
    pub message_count: usize,
    pub strategy: PolicyStrategy,
}

impl EvaluationContext {
    pub fn fires(&self) -> bool {
        self.token_trigger || self.turn_trigger
    }

    pub fn trigger_label(&self) -> &'static str {
        match (self.token_trigger, self.turn_trigger) {
            (true, true) => "tokens_and_turns",
            (true, false) => "tokens",
            (false, true) => "turns",
            (false, false) => "manual",
        }
    }
}

/// Symbolic action returned from [`compaction_check`]. Mirrors the spec
/// triad — `compact_now | defer | abandon`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionAction {
    CompactNow,
    Defer,
    Abandon,
}

impl CompactionAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompactNow => "compact_now",
            Self::Defer => "defer",
            Self::Abandon => "abandon",
        }
    }
}

/// Structured outcome of `compaction_check`. Lowered to a Harn dict by
/// the builtin layer; kept typed at the Rust boundary so downstream
/// consumers (replay, telemetry) can pattern match without re-parsing
/// strings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionDecision {
    pub action: String,
    pub session_id: String,
    pub estimated_tokens: usize,
    pub message_count: usize,
    pub trigger: String,
    pub strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_threshold: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_threshold: Option<usize>,
    /// Engine strategy that `compaction_run` would invoke for this
    /// decision. Distinct from `strategy` (the user-facing label) so
    /// hosts can render either.
    pub engine_strategy: String,
    /// `true` when no policy was registered and the registry returned
    /// the default policy.
    pub policy_inherited: bool,
}

/// Key used for the "default policy" entry in the registry. Empty so it
/// can't collide with a real session id.
const DEFAULT_POLICY_KEY: &str = "";

thread_local! {
    static POLICIES: RefCell<BTreeMap<String, CompactionPolicyDeclaration>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Register or replace the policy keyed by `session_id`. Pass an empty
/// `session_id` to set the default policy used when no per-session entry
/// is found.
pub fn set_policy(session_id: &str, policy: CompactionPolicyDeclaration) {
    POLICIES.with(|cell| {
        cell.borrow_mut().insert(session_id.to_string(), policy);
    });
}

/// Remove the policy keyed by `session_id`. Returns the prior value when
/// one was present.
pub fn clear_policy(session_id: &str) -> Option<CompactionPolicyDeclaration> {
    POLICIES.with(|cell| cell.borrow_mut().remove(session_id))
}

/// Fetch the active policy for a session. Falls back to the
/// default-key entry when no per-session policy is registered.
pub fn policy_for(session_id: &str) -> Option<(CompactionPolicyDeclaration, bool)> {
    POLICIES.with(|cell| {
        let borrow = cell.borrow();
        if let Some(policy) = borrow.get(session_id) {
            return Some((policy.clone(), false));
        }
        borrow
            .get(DEFAULT_POLICY_KEY)
            .map(|policy| (policy.clone(), true))
    })
}

/// Clear every entry. Used by `reset_stdlib_state` so cross-test
/// pollution doesn't leak prior policies into later runs.
pub fn reset_registry() {
    POLICIES.with(|cell| cell.borrow_mut().clear());
}

/// Compile a [`CompactionPolicyDeclaration`] into an
/// [`super::AutoCompactConfig`] for the engine. Encapsulates the
/// policy-name → engine-strategy mapping so each call site stays small.
pub fn to_auto_compact_config(policy: &CompactionPolicyDeclaration) -> super::AutoCompactConfig {
    let engine_strategy = policy.strategy.engine_strategy();
    let mut cfg = super::AutoCompactConfig {
        keep_last: policy.keep_last,
        keep_first: policy.keep_first,
        compact_strategy: engine_strategy.clone(),
        hard_limit_strategy: engine_strategy,
        fallback_strategy: policy.strategy.engine_fallback(),
        summarize_prompt: policy.summarize_prompt.clone(),
        custom_compactor: policy.summarize_fn.clone(),
        policy: policy.instructions.clone(),
        policy_strategy: policy.strategy.as_str().to_string(),
        ..Default::default()
    };
    if let Some(threshold) = policy.token_threshold() {
        cfg.token_threshold = threshold;
    } else {
        cfg.token_threshold = 0;
    }
    // `hard_limit_tokens` is the *escalation* cap that switches engine
    // tier-2 on when tier-1's summary still exceeds the budget. Honor it
    // only when explicitly declared so the typical single-tier policy
    // never accidentally re-invokes the strategy on its own output.
    cfg.hard_limit_tokens = policy.hard_limit_tokens;
    if let Some(value) = policy.tool_output_max_chars {
        cfg.tool_output_max_chars = value;
    }
    cfg
}

/// Round-trip parse a policy from a Harn dict. Returns `Err` on shape
/// mismatch so the builtin layer can surface a meaningful error.
pub fn parse_policy_dict(
    builtin: &str,
    dict: &crate::value::DictMap,
) -> Result<CompactionPolicyDeclaration, String> {
    let mut policy = CompactionPolicyDeclaration::default();
    if let Some(value) = dict.get("strategy") {
        match value {
            VmValue::String(text) => {
                policy.strategy =
                    PolicyStrategy::parse(text).map_err(|e| format!("{builtin}: {e}"))?;
            }
            VmValue::Nil => {}
            other => {
                return Err(format!(
                    "{builtin}: `strategy` must be a string, got {}",
                    other.type_name()
                ));
            }
        }
    }
    if let Some(value) = optional_usize(dict, "max_tokens", builtin)? {
        policy.max_tokens = Some(value);
    }
    if let Some(value) = optional_usize(dict, "max_turns", builtin)? {
        policy.max_turns = Some(value);
    }
    if let Some(value) = optional_usize(dict, "context_window", builtin)? {
        policy.context_window = Some(value);
    }
    if let Some(value) = optional_f64(dict, "safety_ratio", builtin)? {
        if !(0.0..=1.0).contains(&value) {
            return Err(format!(
                "{builtin}: `safety_ratio` must be between 0.0 and 1.0, got {value}"
            ));
        }
        policy.safety_ratio = value;
    }
    if let Some(value) = optional_usize(dict, "keep_last", builtin)? {
        policy.keep_last = value;
    }
    if let Some(value) = optional_usize(dict, "keep_first", builtin)? {
        policy.keep_first = value;
    }
    if let Some(value) = optional_usize(dict, "hard_limit_tokens", builtin)? {
        policy.hard_limit_tokens = Some(value);
    }
    if let Some(value) = optional_usize(dict, "tool_output_max_chars", builtin)? {
        policy.tool_output_max_chars = Some(value);
    }
    if let Some(value) = dict.get("summarize_fn") {
        match value {
            VmValue::Closure(_) => {
                policy.summarize_fn = Some(value.clone());
            }
            VmValue::Nil => {}
            other => {
                return Err(format!(
                    "{builtin}: `summarize_fn` must be a closure, got {}",
                    other.type_name()
                ));
            }
        }
    }
    if let Some(value) = dict.get("summarize_prompt") {
        match value {
            VmValue::String(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    policy.summarize_prompt = Some(trimmed.to_string());
                }
            }
            VmValue::Nil => {}
            other => {
                return Err(format!(
                    "{builtin}: `summarize_prompt` must be a string, got {}",
                    other.type_name()
                ));
            }
        }
    }

    // Engine-level instructions (`policy`, `instructions`, `scope`,
    // `preserve`, `drop`, `extend_default_instructions`, `author`) live
    // on a nested dict but also accept top-level keys for ergonomics.
    policy.instructions = super::parse_compaction_policy_options(Some(dict), builtin)
        .map_err(|error| format!("{builtin}: {}", display_vm_error(&error)))?;

    if matches!(policy.strategy, PolicyStrategy::Custom) && policy.summarize_fn.is_none() {
        return Err(format!(
            "{builtin}: `summarize_fn` is required when strategy is 'custom'"
        ));
    }
    if matches!(policy.strategy, PolicyStrategy::SummarizeThenPrune)
        && parse_compact_strategy("truncate").is_err()
    {
        // Defensive sanity check — fallback string must remain a known engine
        // strategy. The parser already accepts "truncate", so this guards the
        // future where someone renames the engine variant.
        return Err(format!(
            "{builtin}: summarize-then-prune fallback 'truncate' is no longer a known engine strategy"
        ));
    }
    Ok(policy)
}

fn display_vm_error(error: &crate::value::VmError) -> String {
    match error {
        crate::value::VmError::Runtime(message) => message.clone(),
        other => format!("{other:?}"),
    }
}

fn optional_usize(
    dict: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<usize>, String> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) => {
            if *value < 0 {
                return Err(format!("{builtin}: `{key}` must be >= 0, got {value}"));
            }
            Ok(Some(*value as usize))
        }
        Some(other) => Err(format!(
            "{builtin}: `{key}` must be an int, got {}",
            other.type_name()
        )),
    }
}

fn optional_f64(
    dict: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<f64>, String> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Float(value)) => Ok(Some(*value)),
        Some(VmValue::Int(value)) => Ok(Some(*value as f64)),
        Some(other) => Err(format!(
            "{builtin}: `{key}` must be a number, got {}",
            other.type_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_ratio_picks_more_restrictive_cap() {
        let policy = CompactionPolicyDeclaration {
            max_tokens: Some(40_000),
            context_window: Some(100_000),
            safety_ratio: 0.5,
            ..Default::default()
        };
        // ratio = 50k, max_tokens = 40k → 40k wins.
        assert_eq!(policy.token_threshold(), Some(40_000));
    }

    #[test]
    fn ratio_only_when_window_set() {
        let policy = CompactionPolicyDeclaration {
            context_window: Some(120_000),
            safety_ratio: 0.7,
            ..Default::default()
        };
        assert_eq!(policy.token_threshold(), Some(84_000));
    }

    #[test]
    fn evaluate_marks_token_trigger() {
        let policy = CompactionPolicyDeclaration {
            max_tokens: Some(10_000),
            ..Default::default()
        };
        let ctx = policy.evaluate(12_000, 5);
        assert!(ctx.token_trigger);
        assert!(ctx.fires());
        assert_eq!(ctx.trigger_label(), "tokens");
    }

    #[test]
    fn evaluate_marks_turn_trigger() {
        let policy = CompactionPolicyDeclaration {
            max_turns: Some(20),
            ..Default::default()
        };
        let ctx = policy.evaluate(0, 25);
        assert!(ctx.turn_trigger);
        assert_eq!(ctx.trigger_label(), "turns");
    }

    #[test]
    fn defer_when_no_thresholds_configured() {
        let policy = CompactionPolicyDeclaration::default();
        let ctx = policy.evaluate(1_000_000, 1_000_000);
        assert!(!ctx.fires());
    }

    #[test]
    fn default_policy_falls_back_to_session_lookup() {
        reset_registry();
        let default = CompactionPolicyDeclaration {
            max_tokens: Some(50_000),
            ..Default::default()
        };
        set_policy(DEFAULT_POLICY_KEY, default);
        let (resolved, inherited) =
            policy_for("session-without-explicit-policy").expect("default policy resolved");
        assert!(inherited);
        assert_eq!(resolved.max_tokens, Some(50_000));
        reset_registry();
    }

    #[test]
    fn session_specific_policy_takes_precedence() {
        reset_registry();
        set_policy(
            "",
            CompactionPolicyDeclaration {
                max_tokens: Some(50_000),
                ..Default::default()
            },
        );
        set_policy(
            "session-a",
            CompactionPolicyDeclaration {
                max_tokens: Some(80_000),
                ..Default::default()
            },
        );
        let (resolved, inherited) = policy_for("session-a").expect("session policy resolved");
        assert!(!inherited);
        assert_eq!(resolved.max_tokens, Some(80_000));
        reset_registry();
    }

    #[test]
    fn strategy_aliases_round_trip() {
        assert_eq!(
            PolicyStrategy::parse("summarize")
                .unwrap()
                .engine_strategy(),
            CompactStrategy::Llm
        );
        assert_eq!(
            PolicyStrategy::parse("summarize-then-prune")
                .unwrap()
                .engine_fallback(),
            Some(CompactStrategy::Truncate)
        );
        assert_eq!(
            PolicyStrategy::parse("window").unwrap().engine_strategy(),
            CompactStrategy::Truncate
        );
        assert_eq!(
            PolicyStrategy::parse("head+tail")
                .unwrap()
                .engine_strategy(),
            CompactStrategy::Truncate
        );
        assert_eq!(
            PolicyStrategy::parse("observation_mask")
                .unwrap()
                .engine_strategy(),
            CompactStrategy::ObservationMask
        );
        assert!(PolicyStrategy::parse("unknown").is_err());
    }
}
