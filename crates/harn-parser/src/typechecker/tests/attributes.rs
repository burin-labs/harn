use super::*;
use crate::DiagnosticDetails;

#[test]
fn parameterized_test_and_fixture_attributes_are_structurally_validated() {
    let valid = warnings(
        r#"
@test_fixture(scope: file)
fn shared() -> int { return 1 }

@test(cases: [{name: "one", args: [1]}], fixture: shared)
pipeline test_value(_fixture: int, value: int) {}
"#,
    );
    assert!(
        valid.iter().all(|warning| {
            !warning.contains("unknown attribute")
                && !warning.contains("InvalidAttribute")
                && !warning.contains("only applies")
                && !warning.contains("@test")
        }),
        "valid fixture and row metadata should typecheck cleanly: {valid:?}"
    );

    let invalid = warnings(
        r"
@test_fixture(scope: suite, extra: true)
pipeline wrong_target() {}

@test(cases: 1, fixture: 42, typo: [])
fn also_wrong() -> int { return 1 }
",
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("only applies to function")),
        "fixture target should be structural: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("only applies to pipeline")),
        "test target should be structural: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("must be one of [\"file\", \"case\"]")),
        "fixture scope should be an enum-like contract: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("unknown `@test` argument `typo`")),
        "test metadata should reject unknown fields: {invalid:?}"
    );
}

#[test]
fn flow_invariant_accepts_only_the_injected_ast_capability() {
    let diagnostics = diagnostics_with_code(
        r#"
@invariant
@deterministic
@archivist(evidence: ["https://example.com/a", "https://example.org/b"], confidence: 0.9, source_date: "2026-08-01")
fn inspect(ast: HarnessAst, slice, _ctx, _repo) -> bool { return true }
"#,
        Code::FlowInvariantAttributeInvalid,
        DiagnosticSeverity::Error,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn flow_invariant_rejects_typed_authority_outside_the_runtime_contract() {
    let diagnostics = diagnostics_with_code(
        r#"
type Network = HarnessNet
type AstAlias = HarnessAst

@invariant
@deterministic
@archivist(evidence: ["https://example.com/a", "https://example.org/b"], confidence: 0.9, source_date: "2026-08-01")
fn inspect(
  ast_alias: AstAlias,
  bundle: {ast: HarnessAst, fs: HarnessFs},
  network: Network,
  root: Harness,
  slice,
  _ctx,
  _repo,
) -> bool { return true }
"#,
        Code::FlowInvariantAttributeInvalid,
        DiagnosticSeverity::Error,
    );
    let contracts = diagnostics
        .iter()
        .map(|diagnostic| match diagnostic.details.as_ref() {
            Some(DiagnosticDetails::FlowCapabilityBoundary {
                parameter,
                capabilities,
                allowed,
            }) => (parameter.clone(), capabilities.clone(), allowed.clone()),
            details => panic!("expected typed Flow capability detail, got {details:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        contracts,
        [
            (
                "ast_alias".to_string(),
                vec!["HarnessAst".to_string()],
                vec!["HarnessAst".to_string()],
            ),
            (
                "bundle".to_string(),
                vec!["HarnessAst".to_string(), "HarnessFs".to_string()],
                vec!["HarnessAst".to_string()],
            ),
            (
                "network".to_string(),
                vec!["HarnessNet".to_string()],
                vec!["HarnessAst".to_string()],
            ),
            (
                "root".to_string(),
                vec!["Harness".to_string()],
                vec!["HarnessAst".to_string()],
            ),
        ]
    );
}

#[test]
fn handler_invariants_do_not_apply_flow_injection_rules_to_tools_or_pipelines() {
    let source = r#"
@invariant
@deterministic
@archivist(evidence: ["https://example.com/a", "https://example.org/b"], confidence: 0.9, source_date: "2026-08-01")
tool inspect_tool(fs: HarnessFs, slice, _ctx, _repo) -> bool { return true }

@invariant
@deterministic
@archivist(evidence: ["https://example.com/a", "https://example.org/b"], confidence: 0.9, source_date: "2026-08-01")
pipeline inspect_pipeline(network: HarnessNet, slice, _ctx, _repo) {}
"#;
    let diagnostics = diagnostics_with_code(
        source,
        Code::FlowInvariantAttributeInvalid,
        DiagnosticSeverity::Error,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert!(
        diagnostics_with_code(
            source,
            Code::InvalidAttributeTarget,
            DiagnosticSeverity::Warning,
        )
        .is_empty(),
        "invariant companion attributes must share the callable target contract"
    );
}
