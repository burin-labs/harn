//! Runtime introspection snapshot — the resolved provider/model/runtime
//! facts an agent loop can hand to the model so it answers identity
//! questions ("what model are you?") truthfully instead of hallucinating
//! from its training prior.
//!
//! Lifecycle: `execute_llm_call` (the single funnel every `llm_call`
//! variant flows through) captures the resolved provider/model into a
//! thread-local snapshot on every call. The snapshot persists across
//! tool dispatch (which runs between iterations after the
//! `LlmRenderContext` has popped) so the model's own tool calls can
//! read what the *just-run* turn used. A subsequent `llm_call`
//! overwrites the snapshot with its own resolution.
//!
//! Allowlist: each field below is a fact that flows from public catalog
//! / configuration data, not from process state. Adding a new field
//! requires a redaction review — never expose API keys, raw system
//! prompts, env vars, or host-private paths.
//!
//! Opt-in: the snapshot is always tracked (it's a few bytes), but the
//! *tools* that expose it (`current_model`, `current_provider`, etc.) are
//! attached to a registry only when the host script calls
//! `runtime_introspection_tools(reg)`. Minimal harnesses that omit the
//! call get no introspection surface at all.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::json;

use crate::llm_config;
use crate::value::{VmError, VmValue};

/// Environment override for the harness identity reported by
/// `current_harness()`. Hosts that embed Harn (Burin Code, the harn CLI,
/// an IDE plugin, the cloud runner) set this so a model running inside
/// the harness can answer "what's running you?" truthfully.
pub const HARN_HARNESS_ENV: &str = "HARN_HARNESS";

/// Default harness identifier when nothing else is set. The bare harn
/// CLI is the most common entry point and matches the binary name.
const DEFAULT_HARNESS: &str = "harn";

/// Snapshot of the resolved runtime facts for the most recent
/// `llm_call`. Cheap to clone; the capability block is the only heavy
/// field (a precomputed `VmValue::Dict`).
#[derive(Clone, Debug)]
pub struct RuntimeIntrospectionSnapshot {
    pub provider: String,
    pub model: String,
    pub model_alias: Option<String>,
    pub family: String,
    pub tool_format: String,
    pub tier: String,
    pub context_window: Option<u64>,
    pub runtime_context_window: Option<u64>,
    pub capabilities: VmValue,
}

thread_local! {
    /// Most recently resolved `llm_call` snapshot. Persists across tool
    /// dispatch so the model's own `current_model()` tool call sees
    /// the call that produced it. `None` before the first `llm_call` on
    /// the thread, and reset by `reset_thread_local_state`.
    static LAST_RESOLVED_LLM_CALL: RefCell<Option<RuntimeIntrospectionSnapshot>> = const {
        RefCell::new(None)
    };
}

/// Record the resolved provider/model for the active `llm_call` so the
/// introspection tools can report it. Called from `execute_llm_call`
/// after option resolution; subsequent calls overwrite.
pub fn record_resolved_llm_call(provider: &str, model: &str) {
    let snapshot = build_snapshot(provider, model);
    LAST_RESOLVED_LLM_CALL.with(|slot| *slot.borrow_mut() = Some(snapshot));
}

/// Return a clone of the most recent snapshot, or `None` when no
/// `llm_call` has run on this thread.
pub fn current_snapshot() -> Option<RuntimeIntrospectionSnapshot> {
    LAST_RESOLVED_LLM_CALL.with(|slot| slot.borrow().clone())
}

/// Drop the active snapshot. Wired into `reset_llm_state` so tests
/// don't leak resolution across runs.
pub fn reset_snapshot() {
    LAST_RESOLVED_LLM_CALL.with(|slot| *slot.borrow_mut() = None);
}

/// Return the harness identifier reported by `current_harness()`. Reads
/// `HARN_HARNESS` if set and non-empty, otherwise falls back to the
/// `"harn"` CLI default. Trims whitespace so a sloppily-set env var
/// doesn't surface as `" burin-code "`.
pub fn harness_identifier() -> String {
    std::env::var(HARN_HARNESS_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_HARNESS.to_string())
}

/// Build the dict the model sees. Mirrors the JSON returned by
/// `handle_introspection_tool`; both go through this so the Harn-callable
/// `runtime_introspection()` builtin and the tool surface stay in lockstep.
pub fn snapshot_to_vm_value(snapshot: Option<&RuntimeIntrospectionSnapshot>) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "harn_version".to_string(),
        VmValue::String(std::sync::Arc::from(crate::bytecode_cache::HARN_VERSION)),
    );
    dict.insert(
        "harness".to_string(),
        VmValue::String(std::sync::Arc::from(harness_identifier())),
    );
    let Some(snap) = snapshot else {
        for key in [
            "provider",
            "model",
            "model_alias",
            "family",
            "tool_format",
            "tier",
        ] {
            dict.insert(key.to_string(), VmValue::Nil);
        }
        dict.insert("context_window".to_string(), VmValue::Nil);
        dict.insert("runtime_context_window".to_string(), VmValue::Nil);
        dict.insert("capabilities".to_string(), VmValue::Nil);
        return VmValue::Dict(std::sync::Arc::new(dict));
    };
    dict.insert(
        "provider".to_string(),
        VmValue::String(std::sync::Arc::from(snap.provider.as_str())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(std::sync::Arc::from(snap.model.as_str())),
    );
    dict.insert(
        "model_alias".to_string(),
        snap.model_alias
            .as_deref()
            .map(|alias| VmValue::String(std::sync::Arc::from(alias)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "family".to_string(),
        VmValue::String(std::sync::Arc::from(snap.family.as_str())),
    );
    dict.insert(
        "tool_format".to_string(),
        VmValue::String(std::sync::Arc::from(snap.tool_format.as_str())),
    );
    dict.insert(
        "tier".to_string(),
        VmValue::String(std::sync::Arc::from(snap.tier.as_str())),
    );
    dict.insert(
        "context_window".to_string(),
        snap.context_window
            .map(|n| VmValue::Int(n as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "runtime_context_window".to_string(),
        snap.runtime_context_window
            .map(|n| VmValue::Int(n as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert("capabilities".to_string(), snap.capabilities.clone());
    VmValue::Dict(std::sync::Arc::new(dict))
}

/// Tool names registered by `runtime_introspection_tools`. Kept in
/// lockstep with the match arms in `handle_introspection_tool` and with
/// `VM_STDLIB_SHORT_CIRCUIT_TOOLS` so adding a new entry without wiring
/// dispatch fails fast.
pub const INTROSPECTION_TOOL_NAMES: &[&str] = &[
    "current_model",
    "current_provider",
    "current_context_window",
    "current_harn_version",
    "current_harness",
    "available_runtime_capabilities",
    "current_compaction_policy",
];

/// Dispatch a runtime-introspection tool call. Returns `Some(JSON
/// string)` when `name` matches one of [`INTROSPECTION_TOOL_NAMES`],
/// otherwise `None` so `handle_tool_locally` can fall through.
pub fn handle_introspection_tool(name: &str, _args: &serde_json::Value) -> Option<String> {
    if !INTROSPECTION_TOOL_NAMES.contains(&name) {
        return None;
    }
    let snapshot = current_snapshot();
    let snap_ref = snapshot.as_ref();
    let value = match name {
        "current_model" => json!({
            "model": snap_ref.map(|s| s.model.as_str()).unwrap_or(""),
            "model_alias": snap_ref.and_then(|s| s.model_alias.as_deref()),
            "tier": snap_ref.map(|s| s.tier.as_str()).unwrap_or(""),
            "family": snap_ref.map(|s| s.family.as_str()).unwrap_or(""),
            "resolved": snap_ref.is_some(),
        }),
        "current_provider" => json!({
            "provider": snap_ref.map(|s| s.provider.as_str()).unwrap_or(""),
            "tool_format": snap_ref.map(|s| s.tool_format.as_str()).unwrap_or(""),
            "resolved": snap_ref.is_some(),
        }),
        "current_context_window" => json!({
            "context_window": snap_ref.and_then(|s| s.context_window),
            "runtime_context_window": snap_ref.and_then(|s| s.runtime_context_window),
            "resolved": snap_ref.is_some(),
        }),
        "current_harn_version" => json!({
            "harn_version": crate::bytecode_cache::HARN_VERSION,
        }),
        "current_harness" => json!({
            "harness": harness_identifier(),
        }),
        "available_runtime_capabilities" => json!({
            "capabilities": snap_ref
                .map(|s| crate::llm::vm_value_to_json(&s.capabilities))
                .unwrap_or(serde_json::Value::Null),
            "resolved": snap_ref.is_some(),
        }),
        "current_compaction_policy" => json!({
            "policy": compaction_policy_value(),
        }),
        _ => unreachable!("INTROSPECTION_TOOL_NAMES guard above"),
    };
    Some(serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()))
}

/// Builtin used by `runtime_introspection()` / tests. Returns the full
/// snapshot dict whether or not a call has been resolved yet — fields
/// stay `nil` until the first `llm_call`. Accepts no arguments.
pub(crate) fn runtime_introspection_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(snapshot_to_vm_value(current_snapshot().as_ref()))
}

fn build_snapshot(provider: &str, model: &str) -> RuntimeIntrospectionSnapshot {
    let resolved = llm_config::resolve_model_info(model);
    let resolved_id = if model.is_empty() {
        String::new()
    } else {
        resolved.id.clone()
    };
    let family = llm_config::model_family(provider, &resolved_id);
    let (context_window, runtime_context_window) = llm_config::model_catalog_entry(&resolved_id)
        .map(|m| (Some(m.context_window), m.runtime_context_window))
        .unwrap_or_default();
    let caps = crate::llm::capabilities::lookup(provider, &resolved_id);
    let capabilities =
        crate::llm::config_builtins::capabilities_to_vm_value(provider, &resolved_id, &caps);
    RuntimeIntrospectionSnapshot {
        provider: provider.to_string(),
        model: resolved_id,
        model_alias: resolved.alias.clone().or_else(|| {
            if model.is_empty() || model == resolved.id {
                None
            } else {
                Some(model.to_string())
            }
        }),
        family,
        tool_format: resolved.tool_format,
        tier: resolved.tier,
        context_window,
        runtime_context_window,
        capabilities,
    }
}

/// Best-effort summary of the compaction policy. Today this returns the
/// stdlib defaults; once `agent_loop` publishes the resolved
/// `AutoCompactConfig` through a thread-local (tracked separately), this
/// will surface the *actual* per-call policy.
fn compaction_policy_value() -> serde_json::Value {
    let cfg = crate::orchestration::AutoCompactConfig::default();
    json!({
        "policy_strategy": cfg.policy_strategy,
        "keep_first": cfg.keep_first,
        "keep_last": cfg.keep_last,
        "token_threshold": cfg.token_threshold,
        "tool_output_max_chars": cfg.tool_output_max_chars,
        "hard_limit_tokens": cfg.hard_limit_tokens,
        "compact_strategy": crate::orchestration::compact_strategy_name(&cfg.compact_strategy),
        "hard_limit_strategy": crate::orchestration::compact_strategy_name(&cfg.hard_limit_strategy),
        "scope": "default",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_env_harness() {
        unsafe {
            std::env::remove_var(HARN_HARNESS_ENV);
        }
    }

    #[test]
    fn snapshot_starts_empty() {
        reset_snapshot();
        assert!(current_snapshot().is_none());
        let value = snapshot_to_vm_value(current_snapshot().as_ref());
        let dict = value.as_dict().expect("dict");
        assert!(matches!(dict.get("model"), Some(VmValue::Nil)));
        assert!(matches!(dict.get("provider"), Some(VmValue::Nil)));
        // harn_version + harness always populated, even with no resolved call.
        assert!(matches!(dict.get("harn_version"), Some(VmValue::String(_))));
        assert!(matches!(dict.get("harness"), Some(VmValue::String(_))));
    }

    #[test]
    fn record_then_read_round_trip() {
        reset_snapshot();
        record_resolved_llm_call("anthropic", "claude-opus-4-7");
        let snap = current_snapshot().expect("snapshot");
        assert_eq!(snap.provider, "anthropic");
        assert_eq!(snap.model, "claude-opus-4-7");
        assert_eq!(snap.family, "anthropic-claude");
        reset_snapshot();
    }

    #[test]
    fn alias_is_preserved_when_distinct_from_id() {
        reset_snapshot();
        // claude-opus-latest is an alias in the bundled providers.toml;
        // resolution should attach `model_alias` even though `model`
        // holds the concrete id.
        record_resolved_llm_call("anthropic", "claude-opus-latest");
        let snap = current_snapshot().expect("snapshot");
        if snap.model_alias.is_some() {
            assert_ne!(
                snap.model_alias.as_deref(),
                Some(snap.model.as_str()),
                "alias should be the surface name, model should be the resolved id"
            );
        }
        reset_snapshot();
    }

    #[test]
    fn harness_env_wins_over_default() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        reset_env_harness();
        assert_eq!(harness_identifier(), "harn");
        unsafe {
            std::env::set_var(HARN_HARNESS_ENV, "burin-code");
        }
        assert_eq!(harness_identifier(), "burin-code");
        unsafe {
            std::env::set_var(HARN_HARNESS_ENV, "  spaced-out  ");
        }
        assert_eq!(harness_identifier(), "spaced-out");
        reset_env_harness();
    }

    #[test]
    fn handle_introspection_tool_returns_none_for_unknown() {
        assert!(handle_introspection_tool("not_a_tool", &serde_json::Value::Null).is_none());
    }

    #[test]
    fn handle_current_model_reports_resolved_state() {
        reset_snapshot();
        let payload = handle_introspection_tool("current_model", &serde_json::Value::Null)
            .expect("matched tool");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(parsed["resolved"], serde_json::json!(false));
        assert_eq!(parsed["model"], serde_json::json!(""));

        record_resolved_llm_call("anthropic", "claude-opus-4-7");
        let payload = handle_introspection_tool("current_model", &serde_json::Value::Null)
            .expect("matched tool");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(parsed["resolved"], serde_json::json!(true));
        assert_eq!(parsed["model"], serde_json::json!("claude-opus-4-7"));
        assert_eq!(parsed["family"], serde_json::json!("anthropic-claude"));
        reset_snapshot();
    }

    #[test]
    fn handle_current_provider_reports_tool_format() {
        reset_snapshot();
        record_resolved_llm_call("anthropic", "claude-opus-4-7");
        let payload = handle_introspection_tool("current_provider", &serde_json::Value::Null)
            .expect("matched tool");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(parsed["provider"], serde_json::json!("anthropic"));
        assert!(parsed["tool_format"].is_string());
        reset_snapshot();
    }

    #[test]
    fn handle_harness_and_version_work_without_resolution() {
        reset_snapshot();
        let version = handle_introspection_tool("current_harn_version", &serde_json::Value::Null)
            .expect("matched tool");
        assert!(version.contains(crate::bytecode_cache::HARN_VERSION));
        let harness = handle_introspection_tool("current_harness", &serde_json::Value::Null)
            .expect("matched tool");
        assert!(harness.contains("harness"));
    }

    #[test]
    fn empty_model_clears_resolved_id() {
        reset_snapshot();
        record_resolved_llm_call("anthropic", "");
        let snap = current_snapshot().expect("snapshot");
        assert!(snap.model.is_empty());
        reset_snapshot();
    }

    #[test]
    fn snapshot_redaction_allowlist_is_stable() {
        reset_snapshot();
        record_resolved_llm_call("anthropic", "claude-opus-4-7");
        let value = snapshot_to_vm_value(current_snapshot().as_ref());
        let dict = value.as_dict().expect("dict");
        let allowed: std::collections::BTreeSet<&str> = [
            "harn_version",
            "harness",
            "provider",
            "model",
            "model_alias",
            "family",
            "tool_format",
            "tier",
            "context_window",
            "runtime_context_window",
            "capabilities",
        ]
        .into_iter()
        .collect();
        for key in dict.keys() {
            assert!(
                allowed.contains(key.as_str()),
                "introspection snapshot leaked field `{key}` outside the allowlist"
            );
        }
        reset_snapshot();
    }
}
