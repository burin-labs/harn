//! End-to-end checks for the [`Repair`] dispatch contract.
//!
//! These tests drive the typechecker on real Harn source and assert that
//! each emitted [`TypeDiagnostic`] carries the registered repair id and
//! safety class. They lock in the autonomy contract — agents and IDEs
//! that dispatch on `repair.safety` rely on these mappings staying
//! stable across releases.

use super::{check_source, check_source_with_source};
use crate::diagnostic_codes::{Code, RepairSafety};

/// Run the typechecker and return the first diagnostic whose code matches
/// `code`. Panics with diagnostic context if no such diagnostic was
/// produced — the test source is wrong if this fires.
fn first_with_code(source: &str, code: Code) -> crate::TypeDiagnostic {
    let diags = check_source(source);
    diags
        .into_iter()
        .find(|d| d.code == code)
        .unwrap_or_else(|| panic!("no diagnostic with code {code} emitted by source:\n{source}"))
}

fn first_with_code_with_source(source: &str, code: Code) -> crate::TypeDiagnostic {
    let diags = check_source_with_source(source);
    diags
        .into_iter()
        .find(|d| d.code == code)
        .unwrap_or_else(|| panic!("no diagnostic with code {code} emitted by source:\n{source}"))
}

#[test]
fn type_mismatch_attaches_scope_local_repair() {
    let diag = first_with_code(
        r#"
            pipeline main() {
                const x: int = "not an int"
            }
        "#,
        Code::VariableTypeMismatch,
    );
    let repair = diag
        .repair
        .expect("VariableTypeMismatch should carry a repair");
    assert_eq!(repair.id.as_str(), "casts/insert-explicit-conversion");
    assert_eq!(repair.safety, RepairSafety::ScopeLocal);
}

#[test]
fn non_exhaustive_match_attaches_scope_local_repair() {
    let diag = first_with_code(
        r#"
            type Color = "red" | "green" | "blue"
            pipeline main() {
                const c: Color = "red"
                match c {
                    "red" -> { 1 }
                    "green" -> { 2 }
                }
            }
        "#,
        Code::NonExhaustiveMatch,
    );
    let repair = diag
        .repair
        .expect("NonExhaustiveMatch should carry a repair");
    assert_eq!(repair.id.as_str(), "match/add-missing-arms");
    assert_eq!(repair.safety, RepairSafety::ScopeLocal);
}

#[test]
fn immutable_assignment_attaches_scope_local_repair() {
    let diag = first_with_code(
        r"
            pipeline main() {
                const x = 1
                x = 2
            }
        ",
        Code::ImmutableAssignment,
    );
    let repair = diag
        .repair
        .expect("ImmutableAssignment should carry a repair");
    assert_eq!(repair.id.as_str(), "bindings/make-mutable");
    assert_eq!(repair.safety, RepairSafety::ScopeLocal);
}

#[test]
fn string_interpolation_rewrite_attaches_behavior_preserving_repair() {
    // build_interpolation_fix only runs when the checker has source text
    // attached, so route through the source-aware helper.
    let diag = first_with_code_with_source(
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }",
        Code::StringInterpolationRewrite,
    );
    let repair = diag
        .repair
        .expect("StringInterpolationRewrite should carry a repair");
    assert_eq!(repair.id.as_str(), "style/string-interpolation");
    assert_eq!(repair.safety, RepairSafety::BehaviorPreserving);
}

#[test]
fn try_outside_function_attaches_surface_changing_repair() {
    let diag = first_with_code(
        r"
            try* maybe_fail()
        ",
        Code::TryOutsideFunction,
    );
    let repair = diag
        .repair
        .expect("TryOutsideFunction should carry a repair");
    assert_eq!(repair.id.as_str(), "errors/wrap-in-fn");
    assert_eq!(repair.safety, RepairSafety::SurfaceChanging);
}

#[test]
fn invalid_main_signature_attaches_surface_changing_param_repair() {
    let diag = first_with_code("fn main() {}", Code::InvalidMainSignature);
    let repair = diag
        .repair
        .expect("InvalidMainSignature should carry a repair");
    assert_eq!(repair.id.as_str(), "bindings/thread-harness-needs-param");
    assert_eq!(repair.safety, RepairSafety::SurfaceChanging);
}

#[test]
fn diagnostics_without_registered_template_have_no_repair() {
    // `LlmSchemaInvalid` has no registered repair template — agents
    // should treat these as "diagnose only".
    let diags = check_source(
        r#"
            pipeline main() {
                const r = llm_call("you", "instructions", {
                    "schema": 42,
                })
                r
            }
        "#,
    );
    if let Some(diag) = diags.iter().find(|d| d.code == Code::LlmSchemaInvalid) {
        assert!(
            diag.repair.is_none(),
            "LlmSchemaInvalid should not carry a repair until one is registered, got {:?}",
            diag.repair
        );
    }
}

#[test]
fn every_emitted_repair_matches_registry_safety() {
    // Defensive cross-check: every emitted diagnostic's repair (when
    // present) must match the static registry. Catches drift if a future
    // emitter constructs a Repair inline instead of going through the
    // central registry-driven default.
    let sources = [
        "pipeline main() { const x: int = \"nope\" }",
        "pipeline main() { const x = 1; x = 2 }",
        "type Color = \"red\" | \"green\"\npipeline main() { const c: Color = \"red\"; match c { \"red\" -> { 1 } } }",
    ];
    for source in sources {
        for diag in check_source(source) {
            let Some(repair) = diag.repair.as_ref() else {
                continue;
            };
            let template = diag
                .code
                .repair_template()
                .unwrap_or_else(|| panic!("{} carried repair but registry has none", diag.code));
            assert_eq!(
                repair.id.as_str(),
                template.id,
                "{} repair id drift",
                diag.code
            );
            assert_eq!(repair.safety, template.safety, "{} safety drift", diag.code);
        }
    }
}
