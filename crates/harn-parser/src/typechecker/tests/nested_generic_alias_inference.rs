use super::*;

#[test]
fn list_of_generic_aliases_binds_the_nested_argument() {
    let errs = errors(
        r"
type Step<T> = {value: T}

fn int_step() -> Step<int> { return {value: 1} }
fn values<T>(steps: list<Step<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong: list<string> = values([int_step()])
}
",
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected list<string>, found list<int>")),
        "nested alias argument should flow to the return type: {errs:?}"
    );
}

#[test]
fn heterogeneous_nested_aliases_report_the_inferred_union() {
    let errs = errors(
        r#"
type Step<T> = {value: T}

fn int_step() -> Step<int> { return {value: 1} }
fn string_step() -> Step<string> { return {value: "one"} }
fn values<T>(steps: list<Step<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong: list<bool> = values([int_step(), string_step()])
}
"#,
    );
    assert!(
        errs.iter().any(|err| {
            err.contains("expected list<bool>") && err.contains("int") && err.contains("string")
        }),
        "heterogeneous alias arguments should produce a useful union mismatch: {errs:?}"
    );
}

#[test]
fn nullable_alias_fields_bind_the_non_nil_argument() {
    let errs = errors(
        r"
type MaybeStep<T> = {value: T?}

fn values<T>(steps: list<MaybeStep<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong: list<string> = values([{value: 1}])
}
",
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected list<string>, found list<int>")),
        "nullable alias field should bind its non-nil member: {errs:?}"
    );
}

#[test]
fn record_aliases_bind_through_callbacks_and_nested_lists() {
    let errs = errors(
        r#"
type Receipt = {id: string}
type Verdict = {ok: bool}
type RefinementCheck<T> = {name: string, validate: fn(T, dict) -> Verdict}
type RefinedSchema<T> = {base: T, refinements: list<RefinementCheck<T>>}

fn validate_receipt(receipt: Receipt, context: dict) -> Verdict {
  return {ok: receipt.id != ""}
}
fn refinement<T>(name: string, validate: fn(T, dict) -> Verdict) -> RefinementCheck<T> {
  return {name: name, validate: validate}
}
fn receipt_schema() -> RefinedSchema<Receipt> {
  return {
    base: {id: "r-1"},
    refinements: [refinement("has-id", validate_receipt)],
  }
}
fn artifacts<T>(contract: RefinedSchema<T>) -> list<T> { return [] }
fn artifact_groups<T>(contracts: list<RefinedSchema<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong_direct: list<int> = artifacts(receipt_schema())
  let wrong_nested: list<int> = artifact_groups([receipt_schema()])
}
"#,
    );
    assert_eq!(
        errs.len(),
        2,
        "expected both alias paths to bind Receipt: {errs:?}"
    );
    assert!(
        errs.iter().all(|err| {
            err.contains("expected list<int>")
                && (err.contains("found list<Receipt>") || err.contains("found list<{"))
        }),
        "record/callback/list aliases should preserve their concrete argument: {errs:?}"
    );
}

#[test]
fn empty_nested_alias_list_remains_explicitly_resolvable() {
    let errs = errors(
        r"
type Step<T> = {value: T}

fn values<T>(steps: list<Step<T>>) -> list<T> { return [] }

pipeline t(task) {
  let inferred_gradually = values([])
  let explicit: list<int> = values<int>([])
}
",
    );
    assert!(
        errs.is_empty(),
        "an empty list is ambiguous but an explicit type argument must remain available: {errs:?}"
    );
}
