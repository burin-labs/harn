//! `compaction.*` builtins — the harn-side policy primitive (#2505).
//!
//! Three verbs:
//!   * `compaction.policy(opts)` — declare or replace a per-session policy.
//!   * `compaction.check(session_id?)` — return `{action, ...}` without
//!     mutating state.
//!   * `compaction.run(session_id, plan?)` — execute compaction through
//!     the canonical [`run_compaction_lifecycle`] (#2323) and return the
//!     compacted transcript dict.
//!
//! Together they lift the "when do I compact?" decision out of every
//! consumer (TUI, IDE, cloud) and into the runtime so all surfaces see
//! the same trigger semantics and telemetry shape.

use std::collections::BTreeMap;

use crate::agent_sessions;
use crate::llm::helpers::{extract_llm_options, transcript_event};
use crate::orchestration::{
    self, compact_strategy_name, estimate_message_tokens, parse_policy_dict, policy_for,
    reset_registry, run_compaction_lifecycle_with_ctx, set_policy, to_auto_compact_config,
    transcript_compactable_events, CompactLifecycle, CompactMode, CompactStrategy,
    CompactionAction, CompactionPolicyDeclaration, CompactionTrigger, PolicyStrategy,
};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_compaction_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    register_compaction_namespace(vm);
}

/// Bind the `compaction` identifier in the global scope to a dict of
/// `BuiltinRef`s — the runtime-only piece that lets `.harn` code write
/// `compaction.policy(...)` even though the parser treats `compaction`
/// as a normal name reference. Mirrors `event_log` / `cookies` / etc.
fn register_compaction_namespace(vm: &mut Vm) {
    let names = ["policy", "check", "run"];
    vm.set_global(
        "compaction",
        VmValue::Dict(std::sync::Arc::new(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(std::sync::Arc::from("compaction")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(std::sync::Arc::from(format!("compaction.{name}"))),
                )
            }))
            .collect::<std::collections::BTreeMap<_, _>>(),
        )),
    );
}

/// Reset the per-thread policy registry. Called from
/// `crate::stdlib::reset_stdlib_state` so tests don't leak.
pub(crate) fn reset_compaction_state() {
    reset_registry();
}

// ---------------------------------------------------------------------------
// compaction.policy
// ---------------------------------------------------------------------------

#[harn_builtin(sig = "compaction.policy(opts: dict) -> dict", category = "compaction")]
fn compaction_policy_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let opts = require_dict(args.first(), "compaction.policy", "opts")?;
    let session_id = optional_string(&opts, "session_id", "compaction.policy")?;
    let policy = parse_policy_dict("compaction.policy", &opts).map_err(VmError::Runtime)?;
    set_policy(session_id.as_deref().unwrap_or(""), policy.clone());
    Ok(policy_snapshot_value(session_id.as_deref(), &policy, false))
}

// ---------------------------------------------------------------------------
// compaction.check
// ---------------------------------------------------------------------------

#[harn_builtin(
    sig = "compaction.check(session_id?: string) -> dict",
    category = "compaction"
)]
fn compaction_check_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = resolve_session_id(args.first(), "compaction.check")?;
    let (policy, inherited) = policy_for(&session_id).unwrap_or_else(|| {
        // No registered policy at all — use the library default so check()
        // still returns a usable decision (action=defer, no thresholds).
        (CompactionPolicyDeclaration::default(), true)
    });

    let (estimated_tokens, message_count) = estimate_session_pressure(&session_id);
    let ctx = policy.evaluate(estimated_tokens, message_count);

    let action = if ctx.fires() {
        CompactionAction::CompactNow
    } else {
        CompactionAction::Defer
    };

    Ok(decision_value(
        &session_id,
        action,
        &ctx,
        &policy,
        inherited,
    ))
}

// ---------------------------------------------------------------------------
// compaction.run
// ---------------------------------------------------------------------------

#[harn_builtin(
    sig = "compaction.run(session_id?: string, plan?: dict) -> dict",
    kind = "async",
    category = "compaction"
)]
async fn compaction_run_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = resolve_session_id(args.first(), "compaction.run")?;
    let plan = match args.get(1) {
        Some(VmValue::Dict(map)) => (**map).clone(),
        Some(VmValue::Nil) | None => BTreeMap::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "compaction.run: `plan` must be a dict or nil, got {}",
                other.type_name()
            )));
        }
    };

    if !agent_sessions::exists(&session_id) {
        return Err(VmError::Runtime(format!(
            "compaction.run: unknown session id '{session_id}'"
        )));
    }

    let (mut policy, _inherited) =
        policy_for(&session_id).unwrap_or_else(|| (CompactionPolicyDeclaration::default(), true));
    apply_plan_overrides(&mut policy, &plan)?;

    let mut config = to_auto_compact_config(&policy);

    // The plan or the policy may want LLM options threaded through. We accept
    // the same `model`, `provider`, `max_tokens` keys callers pass to
    // `agent_session_compact` so the policy primitive doesn't lose the
    // ability to drive specific models.
    let llm_opts = if matches!(config.compact_strategy, CompactStrategy::Llm) {
        let raw = if plan.is_empty() {
            VmValue::Nil
        } else {
            VmValue::Dict(std::sync::Arc::new(plan.clone()))
        };
        Some(extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            raw,
        ])?)
    } else {
        None
    };

    let mut messages = agent_sessions::messages_json(&session_id);
    let original_count = messages.len();
    let reminder_events = session_compactable_events(&session_id);
    let provider_options = if plan.is_empty() {
        serde_json::json!({})
    } else {
        crate::llm::reminder_providers::options_map_to_json(&plan)
    };

    let start = std::time::Instant::now();
    let lifecycle = CompactLifecycle::new(CompactMode::Host)
        .with_session_id(Some(&session_id))
        .with_trigger(CompactionTrigger::Threshold)
        .with_reminder_events(reminder_events)
        .with_provider_options(provider_options);

    let Some(outcome) = run_compaction_lifecycle_with_ctx(
        Some(&ctx),
        &mut messages,
        &mut config,
        llm_opts.as_ref(),
        lifecycle,
    )
    .await?
    else {
        // Nothing to compact — return an "unchanged" record with the
        // current transcript and original message count so callers can
        // treat the response uniformly.
        let transcript = agent_sessions::transcript(&session_id).unwrap_or(VmValue::Nil);
        return Ok(run_result_value(
            &session_id,
            None,
            original_count,
            original_count,
            0,
            start.elapsed().as_millis() as u64,
            &policy,
            transcript,
        ));
    };

    agent_sessions::replace_messages_with_summary(&session_id, &messages, Some(&outcome.summary))
        .map_err(VmError::Runtime)?;
    let compaction_event = transcript_event(
        "compaction",
        "system",
        "internal",
        "",
        Some(outcome.event_metadata.clone()),
    );
    agent_sessions::append_event(&session_id, compaction_event).map_err(VmError::Runtime)?;
    for preserved in outcome.reminder_report.preserved_events {
        agent_sessions::append_event(&session_id, preserved).map_err(VmError::Runtime)?;
    }

    let latency_ms = start.elapsed().as_millis() as u64;
    let transcript = agent_sessions::transcript(&session_id).unwrap_or(VmValue::Nil);
    Ok(run_result_value(
        &session_id,
        Some(&outcome.strategy),
        original_count,
        messages.len(),
        outcome.archived_messages,
        latency_ms,
        &policy,
        transcript,
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_dict(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match value {
        Some(VmValue::Dict(map)) => Ok((**map).clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: `{name}` must be a dict, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: `{name}` is required"))),
    }
}

fn optional_string(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: `{key}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn resolve_session_id(value: Option<&VmValue>, builtin: &str) -> Result<String, VmError> {
    let candidate = match value {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: `session_id` must be a string, got {}",
                other.type_name()
            )));
        }
    };
    if let Some(id) = candidate {
        return Ok(id);
    }
    agent_sessions::current_session_id().ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: no `session_id` provided and no active agent session"
        ))
    })
}

fn estimate_session_pressure(session_id: &str) -> (usize, usize) {
    if !agent_sessions::exists(session_id) {
        return (0, 0);
    }
    let messages = agent_sessions::messages_json(session_id);
    let message_count = messages.len();
    let estimated = estimate_message_tokens(&messages);
    (estimated, message_count)
}

fn session_compactable_events(id: &str) -> Vec<VmValue> {
    let Some(transcript) = agent_sessions::transcript(id) else {
        return Vec::new();
    };
    let Some(dict) = transcript.as_dict() else {
        return Vec::new();
    };
    transcript_compactable_events(dict)
}

fn apply_plan_overrides(
    policy: &mut CompactionPolicyDeclaration,
    plan: &BTreeMap<String, VmValue>,
) -> Result<(), VmError> {
    if plan.is_empty() {
        return Ok(());
    }
    if let Some(value) = plan.get("strategy") {
        match value {
            VmValue::String(text) => {
                policy.strategy = PolicyStrategy::parse(text)
                    .map_err(|error| VmError::Runtime(format!("compaction.run: {error}")))?;
            }
            VmValue::Nil => {}
            other => {
                return Err(VmError::Runtime(format!(
                    "compaction.run: `strategy` must be a string, got {}",
                    other.type_name()
                )));
            }
        }
    }
    if let Some(value) = plan_int(plan, "keep_last", "compaction.run")? {
        policy.keep_last = value;
    }
    if let Some(value) = plan_int(plan, "keep_first", "compaction.run")? {
        policy.keep_first = value;
    }
    if let Some(value) = plan_int(plan, "target_tokens", "compaction.run")? {
        policy.max_tokens = Some(value);
        policy.hard_limit_tokens = Some(value);
    }
    if let Some(value) = plan.get("summarize_fn") {
        match value {
            VmValue::Closure(_) => policy.summarize_fn = Some(value.clone()),
            VmValue::Nil => {}
            other => {
                return Err(VmError::Runtime(format!(
                    "compaction.run: `summarize_fn` must be a closure, got {}",
                    other.type_name()
                )));
            }
        }
    }
    Ok(())
}

fn plan_int(
    plan: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<usize>, VmError> {
    match plan.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) => {
            if *value < 0 {
                return Err(VmError::Runtime(format!(
                    "{builtin}: `{key}` must be >= 0, got {value}"
                )));
            }
            Ok(Some(*value as usize))
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: `{key}` must be an int, got {}",
            other.type_name()
        ))),
    }
}

fn decision_value(
    session_id: &str,
    action: CompactionAction,
    ctx: &orchestration::EvaluationContext,
    policy: &CompactionPolicyDeclaration,
    inherited: bool,
) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "action".to_string(),
        VmValue::String(std::sync::Arc::from(action.as_str())),
    );
    map.insert(
        "session_id".to_string(),
        VmValue::String(std::sync::Arc::from(session_id.to_string())),
    );
    map.insert(
        "estimated_tokens".to_string(),
        VmValue::Int(ctx.estimated_tokens as i64),
    );
    map.insert(
        "message_count".to_string(),
        VmValue::Int(ctx.message_count as i64),
    );
    map.insert(
        "trigger".to_string(),
        VmValue::String(std::sync::Arc::from(ctx.trigger_label())),
    );
    map.insert(
        "strategy".to_string(),
        VmValue::String(std::sync::Arc::from(ctx.strategy.as_str())),
    );
    map.insert(
        "engine_strategy".to_string(),
        VmValue::String(std::sync::Arc::from(compact_strategy_name(
            &ctx.strategy.engine_strategy(),
        ))),
    );
    if let Some(value) = ctx.token_threshold {
        map.insert("token_threshold".to_string(), VmValue::Int(value as i64));
    }
    if let Some(value) = policy.max_turns {
        map.insert("turn_threshold".to_string(), VmValue::Int(value as i64));
    }
    map.insert("policy_inherited".to_string(), VmValue::Bool(inherited));
    VmValue::Dict(std::sync::Arc::new(map))
}

fn policy_snapshot_value(
    session_id: Option<&str>,
    policy: &CompactionPolicyDeclaration,
    inherited: bool,
) -> VmValue {
    let mut map = BTreeMap::new();
    if let Some(id) = session_id {
        map.insert(
            "session_id".to_string(),
            VmValue::String(std::sync::Arc::from(id.to_string())),
        );
    } else {
        map.insert(
            "session_id".to_string(),
            VmValue::String(std::sync::Arc::from("")),
        );
    }
    map.insert(
        "strategy".to_string(),
        VmValue::String(std::sync::Arc::from(policy.strategy.as_str())),
    );
    if let Some(value) = policy.max_tokens {
        map.insert("max_tokens".to_string(), VmValue::Int(value as i64));
    }
    if let Some(value) = policy.max_turns {
        map.insert("max_turns".to_string(), VmValue::Int(value as i64));
    }
    if let Some(value) = policy.context_window {
        map.insert("context_window".to_string(), VmValue::Int(value as i64));
    }
    map.insert(
        "safety_ratio".to_string(),
        VmValue::Float(policy.safety_ratio),
    );
    map.insert(
        "keep_last".to_string(),
        VmValue::Int(policy.keep_last as i64),
    );
    if policy.keep_first > 0 {
        map.insert(
            "keep_first".to_string(),
            VmValue::Int(policy.keep_first as i64),
        );
    }
    if let Some(value) = policy.hard_limit_tokens {
        map.insert("hard_limit_tokens".to_string(), VmValue::Int(value as i64));
    }
    if let Some(value) = policy.tool_output_max_chars {
        map.insert(
            "tool_output_max_chars".to_string(),
            VmValue::Int(value as i64),
        );
    }
    if let Some(threshold) = policy.token_threshold() {
        map.insert(
            "token_threshold".to_string(),
            VmValue::Int(threshold as i64),
        );
    }
    map.insert("policy_inherited".to_string(), VmValue::Bool(inherited));
    VmValue::Dict(std::sync::Arc::new(map))
}

fn run_result_value(
    session_id: &str,
    strategy: Option<&CompactStrategy>,
    messages_before: usize,
    messages_after: usize,
    archived: usize,
    latency_ms: u64,
    policy: &CompactionPolicyDeclaration,
    transcript: VmValue,
) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "session_id".to_string(),
        VmValue::String(std::sync::Arc::from(session_id.to_string())),
    );
    let compacted = strategy.is_some();
    map.insert("compacted".to_string(), VmValue::Bool(compacted));
    map.insert(
        "messages_before".to_string(),
        VmValue::Int(messages_before as i64),
    );
    map.insert(
        "messages_after".to_string(),
        VmValue::Int(messages_after as i64),
    );
    map.insert(
        "archived_messages".to_string(),
        VmValue::Int(archived as i64),
    );
    map.insert(
        "engine_strategy".to_string(),
        VmValue::String(std::sync::Arc::from(
            strategy.map(compact_strategy_name).unwrap_or("none"),
        )),
    );
    map.insert(
        "strategy".to_string(),
        VmValue::String(std::sync::Arc::from(policy.strategy.as_str())),
    );
    map.insert("latency_ms".to_string(), VmValue::Int(latency_ms as i64));
    if let Some(threshold) = policy.token_threshold() {
        map.insert(
            "token_threshold".to_string(),
            VmValue::Int(threshold as i64),
        );
    }
    map.insert("transcript".to_string(), transcript);
    VmValue::Dict(std::sync::Arc::new(map))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &COMPACTION_POLICY_IMPL_DEF,
    &COMPACTION_CHECK_IMPL_DEF,
    &COMPACTION_RUN_IMPL_DEF,
];
