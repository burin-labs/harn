//! Callable attribute placement and Flow predicate contracts.

use super::*;

#[test]
fn test_flow_predicate_mode_attributes_are_recognized_on_functions() {
    let warns = warnings(
        r"
@deterministic
fn pure_check(slice) -> bool { return true }

@semantic
fn semantic_check(slice) -> bool { return true }
",
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")),
        "predicate mode attributes should not warn as unknown: {warns:?}"
    );
}

#[test]
fn test_runtime_attributes_are_recognized_on_valid_declarations() {
    let warns = warnings(
        r#"
@test
pipeline smoke(task) {}

@acp_skill(name: "deploy", when_to_use: "ship", invocation: "explicit")
fn deploy_activate() -> string { return "ready" }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")),
        "runtime attributes should not warn on valid declarations: {warns:?}"
    );
}

#[test]
fn test_test_scheduler_attributes_are_recognized_and_validated() {
    let warns = warnings(
        r#"
@test
@serial(group: "shared-fixture")
pipeline test_login_first(task) {}

@test
@heavy(threads: 2)
pipeline test_full_rebuild(task) {}

@test
@serial
pipeline test_bare_serial(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")
                && !warning.contains("@serial")
                && !warning.contains("@heavy")),
        "test scheduler attributes should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_job_retry_dict_and_standalone_retry_validate_identically() {
    // The compact `@job(retry: {...})` dict and the standalone `@retry(...)`
    // attribute are documented aliases and now share one validator, so they
    // MUST accept/reject the same backoff strategies. A valid strategy warns
    // on neither; an invalid one warns on both. Guards against the two
    // surfaces drifting (e.g. one list keeping a `"exp"` the other dropped).
    let valid = warnings(
        r#"
@job("nightly", retry: {max: 3, backoff: "exponential"})
@retry(max: 3, backoff: "linear")
fn nightly() -> string { return "ok" }
"#,
    );
    assert!(
        valid.iter().all(|w| !w.contains("backoff")),
        "recognized backoff strategies must warn on neither retry surface: {valid:?}"
    );

    let invalid = warnings(
        r#"
@job("nightly", retry: {max: 3, backoff: "exp"})
@retry(max: 3, backoff: "exp")
fn nightly() -> string { return "ok" }
"#,
    );
    let backoff_warns = invalid.iter().filter(|w| w.contains("backoff")).count();
    assert_eq!(
        backoff_warns, 2,
        "an unrecognized backoff must warn on BOTH retry surfaces (compact + standalone): {invalid:?}"
    );
}

#[test]
fn test_heavy_attribute_requires_positive_int_threads() {
    let warns = warnings(
        r#"
@test
@heavy
pipeline test_missing_threads(task) {}

@test
@heavy(threads: 0)
pipeline test_zero_threads(task) {}

@test
@heavy(threads: "lots")
pipeline test_string_threads(task) {}
"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("must specify `threads:")),
        "expected missing-threads warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .filter(|w| w.contains("must be a positive integer"))
            .count()
            == 2,
        "expected two positive-int warnings (for 0 and \"lots\"), got {warns:?}"
    );
}

#[test]
fn test_serial_heavy_attributes_warn_on_non_pipeline_targets() {
    let warns = warnings(
        r#"
@serial(group: "fixture")
fn helper(x) -> int { return x }

@heavy(threads: 2)
fn other_helper() -> int { return 0 }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@serial` only applies to pipeline declarations")),
        "expected @serial target warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@heavy` only applies to pipeline declarations")),
        "expected @heavy target warning, got {warns:?}"
    );
}

#[test]
fn test_durable_persona_annotations_are_recognized_and_validated() {
    let warns = warnings(
        r#"
@persona(
  triggers: [github.pr_opened, schedule("*/30 * * * *")],
  tools: [github, ci, linear],
  autonomy: act_with_approval,
  budget: {daily_usd: 20, frontier_escalations: 3},
  handoffs: [review_captain, human_maintainer],
  receipts: required,
)
@trigger(github.check_failed)
@handoff(target: review_captain, reason: "risky diff")
@budget(daily_usd: 20, max_tokens: 100000)
fn merge_captain(ctx) -> string { return "ok" }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")
                && !warning.contains("must")),
        "durable persona annotations should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_durable_persona_annotation_arg_type_warnings() {
    let warns = warnings(
        r#"
@persona(triggers: "github.pr_opened", tools: [github, 1], budget: {daily_usd: "twenty"})
@budget(max_tokens: "many")
fn bad_persona(ctx) { return ctx }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(triggers: ...)` must be a list")),
        "expected persona trigger list warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(tools: ...)` must contain only")),
        "expected persona tools warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(daily_usd: ...)` must be a number")),
        "expected inline budget warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@budget(max_tokens: ...)` must be a number")),
        "expected budget warning, got {warns:?}"
    );
}

#[test]
fn test_command_attribute_recognized_on_pipelines_with_known_args() {
    let warns = warnings(
        r#"
@command(name: "review", description: "Review the diff", hint: "focus area")
pipeline review_branch(task) {}
"#,
    );
    assert!(
        warns.iter().all(|warning| !warning.contains("unknown")
            && !warning.contains("only applies")
            && !warning.contains("must")),
        "@command on a pipeline with known args should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_command_attribute_warns_on_unknown_args() {
    let warns = warnings(
        r#"
@command(label: "oops")
pipeline review_branch(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unknown `@command` argument `label`")),
        "expected unknown-arg warning, got {warns:?}"
    );
}

#[test]
fn test_policy_attribute_is_recognized_and_validates_args() {
    // A well-formed `@policy(...)` is recognized (no unknown-attr warning)
    // and clean.
    let clean = warnings(
        r#"
@policy(kinds: "operator platform_admin", matches: "tenant owner", methods: "doc.read doc.write")
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        clean
            .iter()
            .all(|w| !w.contains("unknown attribute") && !w.contains("@policy")),
        "well-formed @policy should not warn: {clean:?}"
    );

    // An unknown key warns but the attribute is still recognized.
    let bad_key = warnings(
        r#"
@policy(roles: "operator")
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        bad_key
            .iter()
            .any(|w| w.contains("unknown `@policy` argument `roles`")),
        "expected unknown-arg warning, got {bad_key:?}"
    );

    // A non-string value warns.
    let bad_value = warnings(
        r#"
@policy(kinds: 42)
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        bad_value
            .iter()
            .any(|w| w.contains("`@policy(kinds: ...)` must be a string literal")),
        "expected non-string warning, got {bad_value:?}"
    );
}

#[test]
fn test_stream_route_attribute_is_recognized() {
    let warns = warnings(
        r#"
@stream
@route("GET", "/events")
fn events(req) { return req }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|w| !w.contains("unknown attribute") && !w.contains("@stream")),
        "stream route attribute should not warn as unknown: {warns:?}"
    );
}

#[test]
fn test_raw_route_attribute_is_recognized() {
    let warns = warnings(
        r#"
@raw
@route("POST", "/hooks")
fn hooks(req) { return req }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|w| !w.contains("unknown attribute") && !w.contains("@raw")),
        "raw route attribute should not warn as unknown: {warns:?}"
    );
}

#[test]
fn test_command_attribute_warns_on_function_decls() {
    let warns = warnings(
        r#"
@command(name: "review")
fn review_branch(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@command` only applies to pipeline declarations")),
        "expected placement warning, got {warns:?}"
    );
}

#[test]
fn test_flow_predicate_mode_attributes_warn_off_function_declarations() {
    let diagnostics = diagnostics_with_code(
        r"
@deterministic
pipeline invalid(task) {}
",
        Code::InvalidAttributeTarget,
        DiagnosticSeverity::Warning,
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
}

#[test]
fn test_flow_invariant_archivist_attributes_recognized() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["https://example.com/spec"], confidence: 0.95, source_date: "2026-04-01", coverage_examples: ["case-a"])
@retroactive
fn complete_predicate(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")),
        "archivist/retroactive attributes should be recognised: {warns:?}"
    );
}

#[test]
fn test_flow_invariant_requires_kind_and_archivist() {
    let warns = warnings(
        r"
@invariant
fn bare_predicate(slice) -> bool { return true }
",
    );
    assert!(
        warns.iter().any(|w| w.contains("requires exactly one of")),
        "expected kind-required warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("missing `@archivist(...)`")),
        "expected archivist-required warning, got {warns:?}"
    );
}

#[test]
fn test_flow_invariant_with_kind_only_still_warns_about_archivist() {
    let warns = warnings(
        r"
@invariant
@deterministic
fn kinded_predicate(slice) -> bool { return true }
",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("missing `@archivist(...)`")),
        "expected archivist-required warning, got {warns:?}"
    );
    assert!(
        warns.iter().all(|w| !w.contains("requires exactly one of")),
        "should not also warn about missing kind: {warns:?}"
    );
}

#[test]
fn test_flow_invariant_kinds_are_mutually_exclusive() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@semantic
@archivist(evidence: ["x"])
fn confused(slice) -> bool { return true }
"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("mutually exclusive")),
        "expected mutual-exclusion warning, got {warns:?}"
    );
}

#[test]
fn test_archivist_without_invariant_warns() {
    let warns = warnings(
        r#"
@archivist(evidence: ["https://x"])
fn standalone() -> int { return 1 }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("only applies to Flow predicates marked")),
        "expected standalone-archivist warning, got {warns:?}"
    );
}

#[test]
fn test_handler_ir_invariant_does_not_trigger_flow_lints() {
    // `@invariant("name")` is the harn-ir handler form, validated
    // separately. Flow lints must not fire for it.
    let warns = warnings(
        r#"
@invariant("approval.reachability")
fn handler() -> int { return 1 }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|w| !w.contains("`@archivist(...)`") && !w.contains("requires exactly one of")),
        "handler-IR @invariant should not trigger Flow lints: {warns:?}"
    );
}

#[test]
fn test_archivist_unknown_arg_warns() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["x"], typo_key: "oops")
fn oops(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unknown `@archivist` argument `typo_key`")),
        "expected unknown-arg warning, got {warns:?}"
    );
}

#[test]
fn test_archivist_confidence_out_of_range_warns() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["x"], confidence: 1.5)
fn loud(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("confidence") && w.contains("[0.0, 1.0]")),
        "expected confidence-range warning, got {warns:?}"
    );
}

#[test]
fn test_host_entry_is_recognized_on_a_function() {
    let warns = warnings(
        r"
@host_entry
pub fn dispatch(harness: Harness, args: dict) -> dict { return args }
",
    );
    assert!(
        warns
            .iter()
            .all(|w| !w.contains("unknown attribute") && !w.contains("only applies")),
        "`@host_entry` should be recognized on a function: {warns:?}"
    );
}

#[test]
fn test_host_entry_on_a_non_function_warns() {
    let warns = warnings(
        r"
@host_entry
pipeline dispatch(task) {}
",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@host_entry` only applies to function declarations")),
        "expected an invalid-target warning, got {warns:?}"
    );
}

/// The attribute records only *that* a host is the caller. Accepting an
/// argument silently would let an author believe a name or capability list had
/// been declared when nothing reads one.
#[test]
fn test_host_entry_arguments_warn() {
    let warns = warnings(
        r#"
@host_entry(name: "dispatch")
pub fn dispatch(harness: Harness) -> int { return 1 }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@host_entry` takes no arguments")),
        "expected a no-arguments warning, got {warns:?}"
    );
}
