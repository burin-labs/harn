//! Key-parity pins between the typed Harn option aliases in
//! `std/{llm,agent,workflow}/options.harn` and their Rust policy twins.
//!
//! Design: serialize each Rust struct's `Default` to JSON (serde is the
//! single source of truth for the accepted wire keys, including renames like
//! `drop_items` → `drop` and `#[serde(skip)]` closure carriers), extract the
//! declared keys from the Harn structural alias source, and assert the two
//! sets match in both directions modulo an explicit, commented allowlist.
//!
//! When this test fails you either added a field to a Rust policy struct
//! without extending the Harn alias (extend the alias), or added an alias
//! key with no runtime backing (either wire it up or move it to the
//! allowlist with a tracking comment).

use std::collections::BTreeSet;

use serde::Serialize;

use super::{CompactionPolicy, ModelPolicy, RetryPolicy, StageContract, TurnPolicy, WorkflowNode};
use crate::llm::cost::LlmBudgetEnvelope;

const LLM_OPTIONS_MODULE: &str = "llm/options";
const AGENT_OPTIONS_MODULE: &str = "agent/options";
const WORKFLOW_OPTIONS_MODULE: &str = "workflow/options";

fn stdlib_source(module: &str) -> &'static str {
    harn_stdlib::get_stdlib_source(module)
        .unwrap_or_else(|| panic!("stdlib source module `{module}` is embedded"))
}

fn llm_options_harn() -> &'static str {
    stdlib_source(LLM_OPTIONS_MODULE)
}

fn agent_options_harn() -> &'static str {
    stdlib_source(AGENT_OPTIONS_MODULE)
}

fn workflow_options_harn() -> &'static str {
    stdlib_source(WORKFLOW_OPTIONS_MODULE)
}

/// Extract the top-level keys declared by `type <name> = ... { ... }` in a
/// Harn source file. Handles optional-key markers (`key?:`), nested inline
/// shapes (only depth-1 keys are collected), and intersection prefixes
/// (`type X = Y & { ... }` — the intersected alias's keys are NOT included;
/// pass its own name to collect them separately).
fn harn_alias_keys(source: &str, name: &str) -> BTreeSet<String> {
    let marker = format!("type {name} =");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("alias `{name}` not found in Harn source"));
    let after = &source[start + marker.len()..];
    let brace = after
        .find('{')
        .unwrap_or_else(|| panic!("alias `{name}` has no shape body"));
    let body = &after[brace + 1..];

    // Walk the shape body character by character so nested inline shapes
    // (which the formatter may collapse onto one line, e.g.
    // `cadence?: {every?: int, ...},`) are handled correctly: a key is the
    // identifier token immediately preceding a `:` at depth 1.
    let mut keys = BTreeSet::new();
    let mut depth = 1usize;
    for raw_line in body.lines() {
        let line = match raw_line.find("//") {
            Some(idx) => &raw_line[..idx],
            None => raw_line,
        };
        let mut ident = String::new();
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    ident.clear();
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return keys;
                    }
                    ident.clear();
                }
                ':' => {
                    let key = ident.trim_end_matches('?');
                    if depth == 1
                        && !key.is_empty()
                        && key
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    {
                        keys.insert(key.to_string());
                    }
                    ident.clear();
                }
                c if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '?' => {
                    ident.push(c);
                }
                c if c.is_whitespace() => {
                    // Whitespace between an identifier and `:` never occurs in
                    // formatted stdlib sources; treat it as a separator.
                    if !ident.is_empty() {
                        ident.clear();
                    }
                    let _ = c;
                }
                _ => ident.clear(),
            }
        }
    }
    panic!("alias `{name}` shape body never closed");
}

/// Serialize a struct's `Default` and return the top-level JSON object keys.
fn serde_default_keys<T: Default + Serialize>() -> BTreeSet<String> {
    let value = serde_json::to_value(T::default()).expect("serialize default");
    let map = value.as_object().expect("default serializes to an object");
    map.keys().cloned().collect()
}

/// Assert `alias ⊆ rust ∪ harn_only` and `rust ⊆ alias ∪ rust_only`.
fn assert_key_parity(
    label: &str,
    alias_keys: &BTreeSet<String>,
    rust_keys: &BTreeSet<String>,
    harn_only: &[&str],
    rust_only: &[&str],
) {
    let unknown_alias_keys: Vec<&String> = alias_keys
        .iter()
        .filter(|k| !rust_keys.contains(*k) && !harn_only.contains(&k.as_str()))
        .collect();
    assert!(
        unknown_alias_keys.is_empty(),
        "{label}: Harn alias declares keys with no Rust twin field (add the field or allowlist with a tracking comment): {unknown_alias_keys:?}"
    );
    let missing_alias_keys: Vec<&String> = rust_keys
        .iter()
        .filter(|k| !alias_keys.contains(*k) && !rust_only.contains(&k.as_str()))
        .collect();
    assert!(
        missing_alias_keys.is_empty(),
        "{label}: Rust struct serializes keys the Harn alias does not declare (extend the alias in std options): {missing_alias_keys:?}"
    );
    for key in harn_only {
        assert!(
            alias_keys.contains(*key),
            "{label}: allowlisted harn-only key `{key}` is not declared by the alias — prune the allowlist"
        );
        assert!(
            !rust_keys.contains(*key),
            "{label}: `{key}` now serializes from the Rust struct — remove it from the harn-only allowlist"
        );
    }
}

#[test]
fn llm_budget_matches_llm_budget_envelope() {
    assert_key_parity(
        "LlmBudget ↔ LlmBudgetEnvelope",
        &harn_alias_keys(llm_options_harn(), "LlmBudget"),
        &serde_default_keys::<LlmBudgetEnvelope>(),
        &[],
        &[],
    );
}

#[test]
fn workflow_retry_policy_matches_retry_policy() {
    assert_key_parity(
        "WorkflowRetryPolicy ↔ RetryPolicy",
        &harn_alias_keys(workflow_options_harn(), "WorkflowRetryPolicy"),
        &serde_default_keys::<RetryPolicy>(),
        // Both retry-with-feedback keys are real, runtime-backed `RetryPolicy`
        // fields (the embedded stage loop in std/workflow/stage.harn executes
        // them) that are absent from the serialized *default*:
        // - `repair_prompt_builder` carries a closure, so it is
        //   `#[serde(skip)]` (same pattern as `ModelPolicy::post_turn_callback`).
        // - `feedback` is `skip_serializing_if = Option::is_none` so unset
        //   policies keep pre-existing WorkflowBundle graph digests
        //   byte-stable (scripts/check_docs_workflow_quickstart.harn pins
        //   them).
        &["repair_prompt_builder", "feedback"],
        &[],
    );
}

#[test]
fn stage_contract_matches_stage_contract() {
    assert_key_parity(
        "StageContract ↔ StageContract",
        &harn_alias_keys(workflow_options_harn(), "StageContract"),
        &serde_default_keys::<StageContract>(),
        &[],
        &[],
    );
}

#[test]
fn model_policy_spec_matches_model_policy() {
    assert_key_parity(
        "ModelPolicySpec ↔ ModelPolicy",
        &harn_alias_keys(workflow_options_harn(), "ModelPolicySpec"),
        &serde_default_keys::<ModelPolicy>(),
        // `post_turn_callback` is a real ModelPolicy field but carries a
        // closure, so it is `#[serde(skip)]` and absent from the
        // serialized default.
        &["post_turn_callback"],
        &[],
    );
}

#[test]
fn stage_spec_matches_workflow_node() {
    assert_key_parity(
        "StageSpec ↔ WorkflowNode",
        &harn_alias_keys(workflow_options_harn(), "StageSpec"),
        &serde_default_keys::<WorkflowNode>(),
        // `context_assembler` is preserved as a raw VmValue on the Rust
        // side (`raw_context_assembler`, `#[serde(skip)]`) because it can
        // carry a ranker closure.
        &["context_assembler"],
        &[],
    );
}

#[test]
fn turn_policy_matches_turn_policy() {
    assert_key_parity(
        "TurnPolicy ↔ TurnPolicy",
        &harn_alias_keys(agent_options_harn(), "TurnPolicy"),
        &serde_default_keys::<TurnPolicy>(),
        &[],
        &[],
    );
}

#[test]
fn compaction_policy_matches_compaction_policy() {
    assert_key_parity(
        "CompactionPolicy ↔ CompactionPolicy",
        &harn_alias_keys(agent_options_harn(), "CompactionPolicy"),
        &serde_default_keys::<CompactionPolicy>(),
        &[],
        &[],
    );
}

/// The typed `LlmCallOptions` alias must mirror the canonical option
/// registry EXACTLY — both directions. The registry
/// (`harn_builtin_meta::llm_options::LLM_CALL_OPTION_FIELDS`) drives the
/// runtime unknown-key gate, the typechecker shape, and the stdlib
/// allowlist builtin; the Harn alias is the one hand-written mirror, so a
/// drift in either direction is a bug.
#[test]
fn llm_call_options_alias_matches_registry_bidirectionally() {
    let alias_keys = harn_alias_keys(llm_options_harn(), "LlmCallOptions");
    let registry: BTreeSet<String> = harn_builtin_meta::llm_options::LLM_CALL_OPTION_FIELDS
        .iter()
        .map(|field| field.name.to_string())
        .collect();
    let alias_only: Vec<&String> = alias_keys.difference(&registry).collect();
    let registry_only: Vec<&String> = registry.difference(&alias_keys).collect();
    assert!(
        alias_only.is_empty() && registry_only.is_empty(),
        "LlmCallOptions alias and the canonical registry drifted \
         (alias-only: {alias_only:?}, registry-only: {registry_only:?}) — \
         fix crates/harn-stdlib/src/stdlib/llm/options.harn or \
         crates/harn-builtin-meta/src/llm_options.rs"
    );
}

/// Removed keys must stay out of the typed alias, and the removal table
/// itself must stay disjoint from the registry (a key cannot be both).
#[test]
fn llm_call_options_excludes_removed_keys() {
    let alias_keys = harn_alias_keys(llm_options_harn(), "LlmCallOptions");
    for entry in harn_builtin_meta::llm_options::LLM_REMOVED_OPTIONS {
        assert!(
            !alias_keys.contains(entry.key),
            "`{}` was removed and must not join the typed LlmCallOptions surface",
            entry.key
        );
    }
    for deprecated in ["llm_retries", "llm_backoff_ms"] {
        assert!(
            !alias_keys.contains(deprecated),
            "`{deprecated}` is deprecated and must not join the typed LlmCallOptions surface"
        );
    }
}

/// `AgentLoopOptions` inlines the `llm_call` option keys (cross-module type
/// references do not structurally resolve for importing consumers yet, so
/// composing `LlmCallOptions & {...}` would break annotation at call sites).
/// This pin keeps the inlined copy a strict superset of `LlmCallOptions`.
#[test]
fn agent_loop_options_is_superset_of_llm_call_options() {
    let llm_keys = harn_alias_keys(llm_options_harn(), "LlmCallOptions");
    let agent_keys = harn_alias_keys(agent_options_harn(), "AgentLoopOptions");
    let missing: Vec<&String> = llm_keys.difference(&agent_keys).collect();
    assert!(
        missing.is_empty(),
        "AgentLoopOptions must inline every LlmCallOptions key (add these to std/agent/options): {missing:?}"
    );
}

/// Smoke-pin the agent-side aliases that have no Rust struct twin
/// (stall diagnostics, judges, and iteration budgets are stdlib-owned):
/// assert a few load-bearing keys so a rename in the alias file cannot go
/// unnoticed by CI even before the conformance suite runs.
#[test]
fn stdlib_owned_agent_aliases_declare_load_bearing_keys() {
    let budget = harn_alias_keys(agent_options_harn(), "IterationBudget");
    for key in [
        "mode",
        "initial",
        "max",
        "extend_by",
        "consecutive_failures",
    ] {
        assert!(budget.contains(key), "IterationBudget lost key `{key}`");
    }
    let stall = harn_alias_keys(agent_options_harn(), "StallDiagnostics");
    for key in ["enabled", "threshold", "inject_feedback", "max_feedback"] {
        assert!(stall.contains(key), "StallDiagnostics lost key `{key}`");
    }
    let judge = harn_alias_keys(agent_options_harn(), "JudgeConfig");
    for key in ["provider", "model", "max_invocations", "cadence"] {
        assert!(judge.contains(key), "JudgeConfig lost key `{key}`");
    }
    let loop_options = harn_alias_keys(agent_options_harn(), "AgentLoopOptions");
    for key in [
        "loop_until_done",
        "iteration_budget",
        "stall_diagnostics",
        "done_judge",
        // Caller-managed history seeding (#4030) is agent_loop-only; it must
        // never migrate onto LlmCallOptions.
        "history",
    ] {
        assert!(
            loop_options.contains(key),
            "AgentLoopOptions lost key `{key}`"
        );
    }
}
