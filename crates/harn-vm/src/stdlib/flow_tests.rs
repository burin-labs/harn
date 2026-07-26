use super::*;
use crate::vm::Vm;

fn vm_with_flow_builtins() -> Vm {
    let mut vm = Vm::new();
    register_flow_builtins(&mut vm);
    vm
}

fn call(vm: &Vm, name: &str, args: &[VmValue]) -> VmValue {
    let mut out = String::new();
    let builtin = vm
        .builtins
        .get(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"))
        .clone();
    builtin(args, &mut out).expect("builtin call failed")
}

#[test]
fn flow_invariant_allow_returns_dict_round_trippable() {
    let vm = vm_with_flow_builtins();
    let result = call(&vm, "flow_invariant_allow", &[]);
    let decoded = InvariantResult::from_vm_value(&result).unwrap();
    assert_eq!(decoded, InvariantResult::allow());
}

#[test]
fn flow_invariant_warn_carries_reason() {
    let vm = vm_with_flow_builtins();
    let result = call(
        &vm,
        "flow_invariant_warn",
        &[VmValue::String(arcstr::ArcStr::from("untested helper"))],
    );
    let decoded = InvariantResult::from_vm_value(&result).unwrap();
    match decoded.verdict {
        Verdict::Warn { reason } => assert_eq!(reason, "untested helper"),
        other => panic!("expected warn verdict, got {other:?}"),
    }
}

#[test]
fn flow_invariant_block_carries_code_and_message() {
    let vm = vm_with_flow_builtins();
    let result = call(
        &vm,
        "flow_invariant_block",
        &[
            VmValue::String(arcstr::ArcStr::from("missing_test")),
            VmValue::String(arcstr::ArcStr::from("no test covers this atom")),
        ],
    );
    let decoded = InvariantResult::from_vm_value(&result).unwrap();
    assert!(decoded.is_blocking());
    let error = decoded.block_error().unwrap();
    assert_eq!(error.code, "missing_test");
    assert_eq!(error.message, "no test covers this atom");
}

#[test]
fn flow_invariant_require_approval_routes_to_principal_or_role() {
    let vm = vm_with_flow_builtins();
    let principal_value = call(
        &vm,
        "flow_invariant_require_approval",
        &[
            VmValue::String(arcstr::ArcStr::from("principal")),
            VmValue::String(arcstr::ArcStr::from("user:alice")),
        ],
    );
    let role_value = call(
        &vm,
        "flow_invariant_require_approval",
        &[
            VmValue::String(arcstr::ArcStr::from("role")),
            VmValue::String(arcstr::ArcStr::from("security-reviewer")),
        ],
    );
    let principal = InvariantResult::from_vm_value(&principal_value).unwrap();
    let role = InvariantResult::from_vm_value(&role_value).unwrap();
    assert_eq!(
        principal.verdict,
        Verdict::RequireApproval {
            approver: Approver::principal("user:alice"),
        }
    );
    assert_eq!(
        role.verdict,
        Verdict::RequireApproval {
            approver: Approver::role("security-reviewer"),
        }
    );
}

#[test]
fn flow_invariant_require_approval_rejects_unknown_kind() {
    let vm = vm_with_flow_builtins();
    let mut out = String::new();
    let builtin = vm
        .builtins
        .get("flow_invariant_require_approval")
        .unwrap()
        .clone();
    let error = builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("squad")),
            VmValue::String(arcstr::ArcStr::from("ship-captains")),
        ],
        &mut out,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("kind must be"));
}

#[test]
fn flow_with_evidence_attaches_all_four_evidence_kinds() {
    let vm = vm_with_flow_builtins();
    let allow = call(&vm, "flow_invariant_allow", &[]);
    let atom_hex = "01".repeat(32);
    let atom_evidence = call(
        &vm,
        "flow_evidence_atom",
        &[
            VmValue::String(arcstr::ArcStr::from(atom_hex.as_str())),
            VmValue::Int(0),
            VmValue::Int(64),
        ],
    );
    let metadata_evidence = call(
        &vm,
        "flow_evidence_metadata",
        &[
            VmValue::String(arcstr::ArcStr::from("src/auth")),
            VmValue::String(arcstr::ArcStr::from("policy")),
            VmValue::String(arcstr::ArcStr::from("min_review_count")),
        ],
    );
    let transcript_evidence = call(
        &vm,
        "flow_evidence_transcript",
        &[
            VmValue::String(arcstr::ArcStr::from("transcript-0001")),
            VmValue::Int(128),
            VmValue::Int(256),
        ],
    );
    let citation_evidence = call(
        &vm,
        "flow_evidence_citation",
        &[
            VmValue::String(arcstr::ArcStr::from("https://harnlang.com/spec")),
            VmValue::String(arcstr::ArcStr::from("verdicts may grade")),
            VmValue::String(arcstr::ArcStr::from("2026-04-26T00:00:00Z")),
        ],
    );
    let evidence_list = VmValue::List(std::sync::Arc::new(vec![
        atom_evidence,
        metadata_evidence,
        transcript_evidence,
        citation_evidence,
    ]));
    let attached = call(&vm, "flow_with_evidence", &[allow, evidence_list]);
    let decoded = InvariantResult::from_vm_value(&attached).unwrap();
    assert_eq!(decoded.evidence.len(), 4);
    assert!(matches!(
        decoded.evidence[0],
        EvidenceItem::AtomPointer { .. }
    ));
    assert!(matches!(
        decoded.evidence[1],
        EvidenceItem::MetadataPath { .. }
    ));
    assert!(matches!(
        decoded.evidence[2],
        EvidenceItem::TranscriptExcerpt { .. }
    ));
    assert!(matches!(
        decoded.evidence[3],
        EvidenceItem::ExternalCitation { .. }
    ));
}

#[test]
fn flow_with_remediation_attaches_description() {
    let vm = vm_with_flow_builtins();
    let block = call(
        &vm,
        "flow_invariant_block",
        &[
            VmValue::String(arcstr::ArcStr::from("style")),
            VmValue::String(arcstr::ArcStr::from("trailing whitespace")),
        ],
    );
    let remediation = call(
        &vm,
        "flow_remediation",
        &[VmValue::String(arcstr::ArcStr::from(
            "strip trailing whitespace",
        ))],
    );
    let attached = call(&vm, "flow_with_remediation", &[block, remediation]);
    let decoded = InvariantResult::from_vm_value(&attached).unwrap();
    assert_eq!(
        decoded.remediation.unwrap().description,
        "strip trailing whitespace"
    );
}

#[test]
fn flow_with_confidence_clamps_to_unit_interval() {
    let vm = vm_with_flow_builtins();
    let warn = call(
        &vm,
        "flow_invariant_warn",
        &[VmValue::String(arcstr::ArcStr::from("low signal"))],
    );
    let attached = call(&vm, "flow_with_confidence", &[warn, VmValue::Float(1.5)]);
    let decoded = InvariantResult::from_vm_value(&attached).unwrap();
    assert_eq!(decoded.confidence, 1.0);
}

#[test]
fn flow_invariant_kind_returns_string_label() {
    let vm = vm_with_flow_builtins();
    let allow = call(&vm, "flow_invariant_allow", &[]);
    let kind = call(&vm, "flow_invariant_kind", &[allow]);
    match kind {
        VmValue::String(s) => assert_eq!(s.as_str(), "allow"),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn flow_invariant_feedback_summarizes_non_allow_records() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": false,
        "status": "fail",
        "records": [
            {
                "name": "no_bad",
                "kind": "deterministic",
                "result": {
                    "verdict": {
                        "kind": "block",
                        "error": {
                            "code": "no_bad",
                            "message": "bad sentinel text remains"
                        }
                    },
                    "confidence": 1.0
                },
                "raw_result": {
                    "remediation": "Remove bad sentinel text.",
                    "findings": [
                        {"path": "src/main.zig", "pattern": "bad"},
                        {"path": "src/lib.zig", "line": 42},
                        {"path": "src/extra.zig"},
                        {"path": "src/omitted.zig"}
                    ]
                }
            },
            {
                "name": "style_nit",
                "kind": "deterministic",
                "result": {
                    "verdict": {
                        "kind": "warn",
                        "reason": "comment is stale"
                    },
                    "remediation": {
                        "description": "Update the comment."
                    },
                    "confidence": 1.0
                }
            },
            {
                "name": "passed",
                "kind": "deterministic",
                "result": {
                    "verdict": {"kind": "allow"},
                    "confidence": 1.0
                }
            }
        ],
        "skipped": []
    }));
    let feedback = call(&vm, "flow_invariant_feedback", &[report]);
    let VmValue::String(text) = feedback else {
        panic!("expected feedback string");
    };

    assert!(text.contains("Flow invariants need attention:"));
    assert!(text.contains("- Block no_bad: no_bad: bad sentinel text remains"));
    assert!(text.contains("Remediation: Remove bad sentinel text."));
    assert!(text.contains("Findings: src/main.zig, src/lib.zig:42, src/extra.zig (+1 more)"));
    assert!(text.contains("- Warn style_nit: comment is stale"));
    assert!(text.contains("Remediation: Update the comment."));
    assert!(!text.contains("passed"));
}

#[test]
fn flow_invariant_feedback_can_omit_findings() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": false,
        "status": "fail",
        "records": [{
            "name": "no_bad",
            "kind": "deterministic",
            "result": {
                "verdict": {
                    "kind": "block",
                    "error": {
                        "code": "no_bad",
                        "message": "bad sentinel text remains"
                    }
                },
                "confidence": 1.0
            },
            "raw_result": {
                "findings": [{"path": "src/main.zig"}]
            }
        }],
        "skipped": []
    }));
    let options = json_to_vm_value(&serde_json::json!({"include_findings": false}));
    let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
    let VmValue::String(text) = feedback else {
        panic!("expected feedback string");
    };

    assert!(text.contains("- Block no_bad: no_bad: bad sentinel text remains"));
    assert!(!text.contains("Findings:"));
    assert!(!text.contains("src/main.zig"));
}

#[test]
fn flow_invariant_feedback_caps_findings_per_item() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": false,
        "status": "fail",
        "records": [{
            "name": "no_bad",
            "kind": "deterministic",
            "result": {
                "verdict": {
                    "kind": "block",
                    "error": {
                        "code": "no_bad",
                        "message": "bad sentinel text remains"
                    }
                },
                "confidence": 1.0
            },
            "raw_result": {
                "findings": [
                    {"path": "src/main.zig"},
                    {"path": "src/main.zig"},
                    {"pattern": "not enough to label"},
                    "src/lib.zig",
                    {"message": "src/extra.zig"}
                ]
            }
        }],
        "skipped": []
    }));
    let options = json_to_vm_value(&serde_json::json!({"max_findings_per_item": 1}));
    let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
    let VmValue::String(text) = feedback else {
        panic!("expected feedback string");
    };

    assert!(text.contains("Findings: src/main.zig (+2 more)"));
    assert!(!text.contains("src/lib.zig"));
    assert!(!text.contains("src/extra.zig"));
    assert!(!text.contains("not enough to label"));
}

#[test]
fn flow_invariant_feedback_is_empty_when_clear_by_default() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": true,
        "status": "pass",
        "records": [{
            "name": "passed",
            "result": {
                "verdict": {"kind": "allow"},
                "confidence": 1.0
            }
        }],
        "skipped": []
    }));
    let feedback = call(&vm, "flow_invariant_feedback", &[report]);

    match feedback {
        VmValue::String(text) => assert_eq!(text.as_str(), ""),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn flow_invariant_feedback_can_include_skipped_records() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": true,
        "status": "pass",
        "records": [],
        "skipped": [{
            "name": "semantic_review",
            "kind": "semantic",
            "reason": "semantic predicates require an explicit include_semantic option"
        }]
    }));
    let options = json_to_vm_value(&serde_json::json!({
        "include_skipped": true,
        "empty_if_clear": false
    }));
    let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
    let VmValue::String(text) = feedback else {
        panic!("expected feedback string");
    };

    assert!(text.contains("- Skipped semantic_review: semantic predicates require"));
}

#[test]
fn flow_invariant_feedback_summarizes_discovery_errors() {
    let vm = vm_with_flow_builtins();
    let report = json_to_vm_value(&serde_json::json!({
        "ok": false,
        "status": "discovery_error",
        "diagnostics": [
            {"severity": "warning", "message": "missing archivist metadata"},
            {"severity": "error", "message": "semantic fallback is unresolved"}
        ],
        "records": [],
        "skipped": []
    }));
    let feedback = call(&vm, "flow_invariant_feedback", &[report]);
    let VmValue::String(text) = feedback else {
        panic!("expected feedback string");
    };

    assert!(text.contains("- Diagnostic warning: missing archivist metadata"));
    assert!(text.contains("- Diagnostic error: semantic fallback is unresolved"));
}

#[test]
fn flow_evidence_atom_rejects_invalid_span() {
    let vm = vm_with_flow_builtins();
    let mut out = String::new();
    let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
    let atom_hex = "ab".repeat(32);
    let error = builtin(
        &[
            VmValue::String(arcstr::ArcStr::from(atom_hex.as_str())),
            VmValue::Int(64),
            VmValue::Int(32),
        ],
        &mut out,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("must be >="));
}

#[test]
fn flow_evidence_atom_rejects_bad_hex() {
    let vm = vm_with_flow_builtins();
    let mut out = String::new();
    let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
    let error = builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("not-hex")),
            VmValue::Int(0),
            VmValue::Int(8),
        ],
        &mut out,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("invalid atom id"));
}

async fn eval_source(source: &str, slice: serde_json::Value) -> serde_json::Value {
    eval_source_with_options(source, slice, serde_json::json!({})).await
}

async fn eval_source_with_options(
    source: &str,
    slice: serde_json::Value,
    options: serde_json::Value,
) -> serde_json::Value {
    let mut vm = vm_with_flow_builtins();
    let value = vm
        .call_named_builtin(
            "flow_evaluate_invariants",
            vec![
                VmValue::String(arcstr::ArcStr::from(source)),
                json_to_vm_value(&slice),
                json_to_vm_value(&options),
            ],
        )
        .await
        .unwrap();
    vm_value_to_json(&value)
}

#[tokio::test]
async fn flow_evaluate_invariants_adapts_harn_canon_result_shape() {
    let report = eval_source(
        r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn no_bad(slice, _ctx, _repo_at_base) {
  return {
    verdict: "Block",
    rule: "no_bad",
    findings: [{path: slice.files[0].path}],
    remediation: "Remove bad sentinel text.",
  }
}
"#,
        serde_json::json!({
            "files": [{"path": "src/lib.rs", "text": "bad"}],
        }),
    )
    .await;

    assert_eq!(report["ok"], false);
    assert_eq!(report["status"], "fail");
    assert_eq!(report["records"][0]["name"], "no_bad");
    assert_eq!(
        report["records"][0]["result"]["verdict"]["error"]["code"],
        "no_bad"
    );
    assert_eq!(
        report["records"][0]["raw_result"]["findings"][0]["path"],
        "src/lib.rs"
    );
}

#[tokio::test]
async fn flow_evaluate_invariants_accepts_typed_flow_results() {
    let report = eval_source(
        r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn typed_block(_slice, _ctx, _repo_at_base) {
  return flow_invariant_block("typed_rule", "Typed Flow result blocked.")
}
"#,
        serde_json::json!({"files": []}),
    )
    .await;

    assert_eq!(report["ok"], false);
    assert_eq!(
        report["records"][0]["result"]["verdict"]["error"]["code"],
        "typed_rule"
    );
    assert_eq!(
        report["records"][0]["raw_result"]["verdict"]["kind"],
        "block"
    );
}

#[tokio::test]
async fn flow_evaluate_invariants_skips_semantic_by_default() {
    let report = eval_source(
        r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "fallback", findings: [], remediation: ""}
}

@invariant
@semantic(fallback: "fallback")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Block", rule: "semantic_check", findings: [], remediation: "should not run"}
}
"#,
        serde_json::json!({"files": []}),
    )
    .await;

    assert_eq!(report["ok"], true);
    assert_eq!(report["records"][0]["name"], "fallback");
    assert_eq!(report["skipped"][0]["name"], "semantic_check");
    assert_eq!(
        report["skipped"][0]["reason"],
        "semantic predicates require an explicit include_semantic option"
    );
}

#[tokio::test]
async fn flow_evaluate_invariants_reports_missing_fallback_code() {
    let report = eval_source_with_options(
        r#"
@invariant
@semantic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "semantic_check", findings: [], remediation: ""}
}
"#,
        serde_json::json!({"files": []}),
        serde_json::json!({"include_semantic": true}),
    )
    .await;

    assert_eq!(report["status"], "discovery_error");
    assert_eq!(report["diagnostics"][0]["code"], "fallback_missing");
}

#[tokio::test]
async fn flow_evaluate_invariants_reports_unresolved_fallback_code() {
    let report = eval_source_with_options(
        r#"
@invariant
@semantic(fallback: "does_not_exist")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "semantic_check", findings: [], remediation: ""}
}
"#,
        serde_json::json!({"files": []}),
        serde_json::json!({
            "include_semantic": true,
            "predicates": ["semantic_check"],
            "budget_ms": 1_000,
        }),
    )
    .await;

    assert_eq!(report["ok"], false);
    assert!(report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .any(|diagnostic| diagnostic["code"] == "fallback_unresolved"));
}

#[tokio::test]
async fn flow_evaluate_invariants_runs_semantic_and_fallback_once() {
    let report = eval_source_with_options(
        r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Warn", rule: "fallback", findings: [], remediation: "same concern"}
}

@invariant
@semantic(fallback: "fallback")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Warn", rule: "semantic_check", findings: [], remediation: "same concern"}
}
"#,
        serde_json::json!({"files": []}),
        serde_json::json!({
            "include_semantic": true,
            "predicates": ["semantic_check"],
            "budget_ms": 1_000,
        }),
    )
    .await;

    let records = report["records"].as_array().expect("records");
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .any(|record| record["name"] == "semantic_check"));
    assert!(records.iter().any(|record| record["name"] == "fallback"));
    assert!(records.iter().all(|record| record["attempts"] == 1));
    assert!(report["skipped"]
        .as_array()
        .expect("skipped")
        .iter()
        .all(|record| record["name"] != "fallback"));
}

#[tokio::test]
async fn flow_evaluate_invariants_composes_the_stricter_fallback_verdict() {
    let report = eval_source_with_options(
        r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Block", rule: "fallback_policy", findings: [], remediation: "fallback blocked"}
}

@invariant
@semantic(fallback: "fallback")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "semantic_check", findings: [], remediation: ""}
}
"#,
        serde_json::json!({"files": []}),
        serde_json::json!({
            "include_semantic": true,
            "predicates": ["semantic_check"],
            "budget_ms": 1_000,
        }),
    )
    .await;

    let semantic = report["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|record| record["name"] == "semantic_check")
        .expect("semantic record");
    assert_eq!(semantic["raw_result"]["verdict"], "Allow");
    assert_eq!(
        semantic["result"]["verdict"]["error"]["code"],
        "fallback_policy"
    );
    assert_eq!(report["ok"], false);
}

#[tokio::test]
async fn flow_evaluate_invariants_records_advisory_fallback_without_composing_it() {
    let report = eval_source_with_options(
            r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Block", rule: "unrelated_advisory", findings: [], remediation: "fallback blocked"}
}

@invariant
@semantic(fallback: "fallback", policy: "advisory")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "semantic_check", findings: [], remediation: ""}
}
"#,
            serde_json::json!({"files": []}),
            serde_json::json!({
                "include_semantic": true,
                "predicates": ["semantic_check"],
                "budget_ms": 1_000,
            }),
        )
        .await;

    let records = report["records"].as_array().expect("records");
    assert_eq!(records.len(), 2);
    let semantic = records
        .iter()
        .find(|record| record["name"] == "semantic_check")
        .expect("semantic record");
    assert_eq!(semantic["fallback_policy"], "advisory");
    assert_eq!(semantic["result"]["verdict"]["kind"], "allow");
    let fallback = records
        .iter()
        .find(|record| record["name"] == "fallback")
        .expect("fallback record");
    assert_eq!(fallback["enforced"], false);
    assert_eq!(report["ok"], true);
}

#[tokio::test]
async fn flow_evaluate_invariants_preserves_semantic_judge_closure() {
    let mut vm = vm_with_flow_builtins();
    let judge_exports = vm
        .load_module_exports_from_source(
            "<semantic-judge>.harn",
            r#"
pub fn judge(_request) {
  return {verdict: "Block", rule: "semantic_judge", findings: [], remediation: "judge blocked"}
}
"#,
        )
        .await
        .expect("judge module");
    let judge = judge_exports.get("judge").expect("judge export").clone();
    let options = VmValue::dict([
        ("include_semantic", VmValue::Bool(true)),
        (
            "predicates",
            VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
                "semantic_check",
            ))])),
        ),
        (
            "ctx",
            VmValue::dict([("semantic_judge", VmValue::Closure(judge))]),
        ),
        ("budget_ms", VmValue::Int(1_000)),
    ]);
    let value = vm
        .call_named_builtin(
            "flow_evaluate_invariants",
            vec![
                VmValue::String(arcstr::ArcStr::from(
                    r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "fallback", findings: [], remediation: ""}
}

@invariant
@semantic(fallback: "fallback")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-25")
pub fn semantic_check(_slice, ctx, _repo_at_base) {
  return ctx.semantic_judge({})
}
"#,
                )),
                json_to_vm_value(&serde_json::json!({"files": []})),
                options,
            ],
        )
        .await
        .expect("evaluate invariants");
    let report = vm_value_to_json(&value);
    let semantic = report["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|record| record["name"] == "semantic_check")
        .expect("semantic record");

    assert_eq!(
        semantic["result"]["verdict"]["error"]["code"],
        "semantic_judge"
    );
}
