//! Key-parity pins between the typed Harn option aliases in
//! `std/llm/options`, `std/agent/options_types`, and
//! `std/workflow/options` and their Rust policy twins.
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

use std::{borrow::Cow, collections::BTreeSet};

use serde::Serialize;

use super::{CompactionPolicy, ModelPolicy, RetryPolicy, StageContract, TurnPolicy, WorkflowNode};
use crate::llm::cost::LlmBudgetEnvelope;

const LLM_OPTIONS_MODULE: &str = "llm/options";
const LLM_ENVELOPE_MODULE: &str = "llm/envelope";
const AGENT_OPTIONS_TYPES_MODULE: &str = "agent/options_types";
const AGENT_CONTRACTS_MODULE: &str = "agent/contracts";
const WORKFLOW_OPTIONS_MODULE: &str = "workflow/options";

fn normalized_harn_source(source: &str) -> Cow<'_, str> {
    if source.contains('\r') {
        Cow::Owned(source.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(source)
    }
}

fn crlf_harn_source(source: &str) -> String {
    normalized_harn_source(source).replace('\n', "\r\n")
}

fn stdlib_source(module: &str) -> &'static str {
    harn_stdlib::get_stdlib_source(module)
        .unwrap_or_else(|| panic!("stdlib source module `{module}` is embedded"))
}

fn llm_options_harn() -> &'static str {
    stdlib_source(LLM_OPTIONS_MODULE)
}

fn llm_envelope_harn() -> &'static str {
    stdlib_source(LLM_ENVELOPE_MODULE)
}

fn agent_options_types_harn() -> &'static str {
    stdlib_source(AGENT_OPTIONS_TYPES_MODULE)
}

fn agent_contracts_harn() -> &'static str {
    stdlib_source(AGENT_CONTRACTS_MODULE)
}

fn workflow_options_harn() -> &'static str {
    stdlib_source(WORKFLOW_OPTIONS_MODULE)
}

const AGENT_SPEC_PARTS: [&str; 6] = [
    "AgentModelSpec",
    "AgentExecutionSpec",
    "AgentCapabilitySpec",
    "AgentLifecycleSpec",
    "AgentContextSpec",
    "AgentObservabilitySpec",
];

fn agent_spec_keys_from(source: &str) -> BTreeSet<String> {
    let source = normalized_harn_source(source);
    let declaration = concat!(
        "pub type AgentSpec = AgentModelSpec \\\n",
        "  & AgentExecutionSpec \\\n",
        "  & AgentCapabilitySpec \\\n",
        "  & AgentLifecycleSpec \\\n",
        "  & AgentContextSpec \\\n",
        "  & AgentObservabilitySpec",
    );
    assert!(
        source.contains(declaration),
        "AgentSpec must compose exactly the six named agent contract records"
    );
    AGENT_SPEC_PARTS
        .into_iter()
        .flat_map(|name| harn_alias_keys(&source, name))
        .collect()
}

fn agent_spec_keys() -> BTreeSet<String> {
    agent_spec_keys_from(agent_options_types_harn())
}

/// Extract the top-level keys declared by `type <name> = ... { ... }` in a
/// Harn source file. Handles optional-key markers (`key?:`), nested inline
/// shapes (only depth-1 keys are collected), and intersection prefixes
/// (`type X = Y & { ... }` — the intersected alias's keys are NOT included;
/// pass its own name to collect them separately).
#[expect(
    clippy::string_slice,
    reason = "offsets come from find on the same string plus ASCII literal lengths"
)]
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

fn harn_string_union_values(source: &str, name: &str) -> BTreeSet<String> {
    let source = normalized_harn_source(source);
    let marker = format!("type {name} =");
    let (_, declaration) = source
        .split_once(&marker)
        .unwrap_or_else(|| panic!("alias `{name}` not found in Harn source"));
    declaration
        .split("\n\n")
        .next()
        .expect("union declaration")
        .split('|')
        .filter_map(|part| {
            let value = part.trim().trim_end_matches('\\').trim().trim_matches('"');
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

#[test]
fn typed_contract_parsers_accept_crlf_sources() {
    let crlf_options = crlf_harn_source(agent_options_types_harn());
    assert_eq!(crlf_harn_source(&crlf_options), crlf_options);
    assert_eq!(
        agent_spec_keys_from(&crlf_options),
        agent_spec_keys_from(agent_options_types_harn()),
        "AgentSpec extraction must be independent of checkout line endings",
    );

    let crlf_contracts = crlf_harn_source(agent_contracts_harn());
    assert_eq!(
        harn_string_union_values(&crlf_contracts, "AgentTerminalKind"),
        harn_string_union_values(agent_contracts_harn(), "AgentTerminalKind"),
        "string-union extraction must be independent of checkout line endings",
    );
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
        &harn_alias_keys(agent_options_types_harn(), "TurnPolicy"),
        &serde_default_keys::<TurnPolicy>(),
        &[],
        &[],
    );
}

#[test]
fn compaction_policy_matches_compaction_policy() {
    assert_key_parity(
        "CompactionPolicy ↔ CompactionPolicy",
        &harn_alias_keys(agent_options_types_harn(), "CompactionPolicy"),
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

/// The runtime producer is the behavioral owner of `llm_call().usage`.
/// Compare its real emitted keys with both typed projections so adding or
/// removing a field on any one surface fails in both drift directions.
#[test]
fn llm_usage_runtime_and_typed_projections_match_bidirectionally() {
    let runtime_keys = crate::llm::api::result::test_public_usage_keys();
    let builtin_keys: BTreeSet<String> = match harn_builtin_meta::shapes::LLM_USAGE {
        harn_builtin_meta::Ty::Shape(fields) => {
            fields.iter().map(|field| field.name.to_string()).collect()
        }
        other => panic!("LLM_USAGE must remain a closed shape, got {other:?}"),
    };
    let stdlib_keys = harn_alias_keys(llm_envelope_harn(), "LlmUsage");

    assert_eq!(
        builtin_keys, runtime_keys,
        "builtin LLM_USAGE drifted from the runtime-emitted llm_call usage envelope"
    );
    assert_eq!(
        stdlib_keys, runtime_keys,
        "stdlib LlmUsage drifted from the runtime-emitted llm_call usage envelope"
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

/// The model component of `AgentSpec` inlines the `llm_call` option keys.
/// Composing the six same-module records keeps the public contract navigable
/// without weakening the existing strict superset check.
#[test]
fn agent_loop_options_is_superset_of_llm_call_options() {
    let llm_keys = harn_alias_keys(llm_options_harn(), "LlmCallOptions");
    let agent_keys = agent_spec_keys();
    let missing: Vec<&String> = llm_keys.difference(&agent_keys).collect();
    assert!(
        missing.is_empty(),
        "AgentSpec must inline every LlmCallOptions key (add these to std/agent/options_types): {missing:?}"
    );
}

/// Smoke-pin the agent-side aliases that have no Rust struct twin
/// (stall diagnostics, judges, and iteration budgets are stdlib-owned):
/// assert a few load-bearing keys so a rename in the alias file cannot go
/// unnoticed by CI even before the conformance suite runs.
#[test]
fn stdlib_owned_agent_aliases_declare_load_bearing_keys() {
    let budget = harn_alias_keys(agent_options_types_harn(), "IterationBudget");
    for key in [
        "mode",
        "initial",
        "max",
        "extend_by",
        "consecutive_failures",
    ] {
        assert!(budget.contains(key), "IterationBudget lost key `{key}`");
    }
    let failure_budget = harn_alias_keys(agent_options_types_harn(), "ConsecutiveFailureBudget");
    for key in ["max", "kinds", "paused_for_ms"] {
        assert!(
            failure_budget.contains(key),
            "ConsecutiveFailureBudget lost key `{key}`"
        );
    }
    let missing_tool =
        harn_alias_keys(agent_options_types_harn(), "MissingToolCallRecoveryOptions");
    for key in [
        "enabled",
        "classifier",
        "confidence_threshold",
        "timeout_ms",
    ] {
        assert!(
            missing_tool.contains(key),
            "MissingToolCallRecoveryOptions lost key `{key}`"
        );
    }
    let narrowing = harn_alias_keys(agent_options_types_harn(), "ToolSurfaceNarrowingOptions");
    for key in ["window_turns", "mode", "hard_keep", "unknown_tool_policy"] {
        assert!(
            narrowing.contains(key),
            "ToolSurfaceNarrowingOptions lost key `{key}`"
        );
    }
    let stance = harn_alias_keys(agent_options_types_harn(), "ReadOnlyStanceOptions");
    for key in ["armed", "classifier", "consent_check", "min_confidence"] {
        assert!(
            stance.contains(key),
            "ReadOnlyStanceOptions lost key `{key}`"
        );
    }
    let stall = harn_alias_keys(agent_options_types_harn(), "StallDiagnostics");
    for key in ["enabled", "threshold", "inject_feedback", "max_feedback"] {
        assert!(stall.contains(key), "StallDiagnostics lost key `{key}`");
    }
    let judge = harn_alias_keys(agent_options_types_harn(), "JudgeConfig");
    for key in ["provider", "model", "max_invocations", "cadence"] {
        assert!(judge.contains(key), "JudgeConfig lost key `{key}`");
    }
    let loop_options = agent_spec_keys();
    for key in [
        "loop_until_done",
        "iteration_budget",
        "stall_diagnostics",
        "turn_end_condition",
        // Caller-managed history seeding (#4030) is agent_loop-only; it must
        // never migrate onto LlmCallOptions.
        "history",
    ] {
        assert!(loop_options.contains(key), "AgentSpec lost key `{key}`");
    }
}

#[test]
fn agent_terminal_contract_projects_the_rust_owner_exactly() {
    let source = agent_contracts_harn();
    let harn_kinds = harn_string_union_values(source, "AgentTerminalKind");
    let rust_kinds: BTreeSet<String> = crate::agent_events::AgentTerminalKind::ALL
        .into_iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    assert_eq!(
        harn_kinds, rust_kinds,
        "AgentTerminalKind projection drifted"
    );

    let harn_owners = harn_string_union_values(source, "AgentTerminalOwner");
    let rust_owners: BTreeSet<String> = crate::agent_events::AgentTerminalKind::ALL
        .into_iter()
        .map(|kind| kind.owner().to_string())
        .collect();
    assert_eq!(
        harn_owners, rust_owners,
        "AgentTerminalOwner projection drifted"
    );

    let harn_lifecycle_states = harn_string_union_values(source, "AgentTerminalLifecycleState");
    let rust_lifecycle_states: BTreeSet<String> = crate::agent_events::AgentTerminalKind::ALL
        .into_iter()
        .map(|kind| kind.lifecycle_state().wire_name().to_string())
        .collect();
    assert_eq!(
        harn_lifecycle_states, rust_lifecycle_states,
        "AgentTerminalLifecycleState projection drifted"
    );

    assert_eq!(
        harn_alias_keys(source, "AgentTerminalOutcome"),
        [
            "kind",
            "lifecycle_state",
            "owner",
            "reason",
            "run_record_status",
        ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "AgentTerminalOutcome must expose the producer-owned decision and its canonical projections",
    );
}

#[test]
fn agent_result_contract_carries_the_typed_terminal_projection() {
    let keys = harn_alias_keys(agent_contracts_harn(), "AgentResult");
    for key in [
        "status",
        "final_status",
        "stop_reason",
        "terminal",
        "llm",
        "tools",
        "session_id",
        "run_id",
    ] {
        assert!(keys.contains(key), "AgentResult lost core field `{key}`");
    }
}
