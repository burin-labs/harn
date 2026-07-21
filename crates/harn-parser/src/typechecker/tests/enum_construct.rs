//! Enum construction (`EnumName.Variant(args)`) payload typechecking.
//!
//! Source syntax lowers to MethodCall; these tests lock that the declared
//! variant field types are enforced on that live path.

use super::*;

#[test]
fn test_enum_construct_checks_payload_field_types() {
    let errs = errors(
        r#"enum Box {
  Full(value: int, label: string)
}

pipeline t(task) {
  const _ = Box.Full("not-an-int", 1)
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("Box.Full argument `value`") && e.contains("expected int")),
        "expected payload type mismatch, got: {errs:?}"
    );
}

#[test]
fn test_enum_construct_rejects_non_receipt_for_verdict_receipt_field() {
    // Opaque host names such as `verdict_receipt` must stay unforgeable when
    // a `kind`-tagged shape migrates to a real enum (harn#5357).
    let errs = errors(
        r#"enum Verdict {
  Pass(proof: verdict_receipt, evidence_ref: string)
  Fail(reason: string, detail: string)
}

pipeline t(task) {
  const _ = Verdict.Pass("not-a-receipt", "x")
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("verdict_receipt") && e.contains("string")),
        "expected verdict_receipt forgery to be rejected, got: {errs:?}"
    );
}
