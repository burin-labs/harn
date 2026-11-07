use super::*;
use crate::TypeExpr;

#[test]
fn concrete_record_tails_constrain_assignments_and_preserve_free_rows() {
    let header = r"
type Settings = {provider?: string, required: int}
type Model = {system?: string, ...Settings}
type Agent = Model & {max_iterations?: int}
type Envelope<R> = {label: string, ...R}
type Open = {provider?: string, ...dict}
type ExtendedOpen = {...Open}
type Recursive = {name: string, next?: Recursive}
type Wrapped = {...Recursive}
type RecursiveRow = {name: string, ...RecursiveRow}
fn preserve<R>(value: {name: string, ...R}) -> {name: string, ...R} { return value }
";
    let valid = r#"
pipeline main(task) {
  const absent: Agent = {required: 1}
  const explicit_nil: Agent = {required: 1, provider: nil}
  const provider: Agent = {required: 1, provider: "mock"}
  const generic: Envelope<Settings> = {label: "ok", required: 1}
  const free: Envelope<dict> = {label: "ok", extension: {anything: true}}
  const open: ExtendedOpen = {provider: "mock", extension: {anything: true}}
  const recursive: Wrapped = {name: "root", next: {name: "child"}}
  const recursive_row: RecursiveRow = {name: "root"}
}
"#;
    assert!(errors(&format!("{header}{valid}")).is_empty());
    for binding in [
        "const wrong: Agent = {required: 1, provider: 7}",
        "const missing: Agent = {provider: \"mock\"}",
        "const wrong: Envelope<Settings> = {label: \"ok\", required: 1, provider: 7}",
        "const missing: Envelope<Settings> = {label: \"ok\"}",
        "const missing: Envelope<dict> = {extension: true}",
        "const wrong: ExtendedOpen = {provider: 7}",
        "const wrong: Wrapped = {name: \"root\", next: {name: 7}}",
        "const wrong: RecursiveRow = {name: 7}",
    ] {
        let source = format!("{header}\npipeline main(task) {{ {binding} }}");
        assert!(
            !errors(&source).is_empty(),
            "record tail must refuse {binding}"
        );
    }
}

#[test]
fn declared_type_projection_shares_imports_spreads_and_recursive_resolution() {
    let imported = crate::parse_source("pub type Settings = {provider: string}").unwrap();
    let program = crate::parse_source(
        "type Request = {name: string, ...Settings}\ntype Recursive = {next?: Recursive}",
    )
    .unwrap();
    let resolve = |name| {
        TypeChecker::new()
            .with_imported_type_decls(imported.clone())
            .resolve_declared_type(&program, name)
    };
    let Some(TypeExpr::Shape(fields)) = resolve("Request") else {
        panic!("resolved request shape")
    };
    assert_eq!(fields.len(), 2);
    assert!(fields.iter().any(
        |field| field.name == "provider" && field.type_expr == TypeExpr::Named("string".into())
    ));
    assert!(
        resolve("Absent").is_none(),
        "an absent contract is not an empty shape"
    );
    let Some(TypeExpr::Shape(recursive)) = resolve("Recursive") else {
        panic!("recursive shape")
    };
    assert!(matches!(&recursive[0].type_expr, TypeExpr::Named(name) if name == "Recursive"));
    let shadow =
        crate::parse_source("type Settings = {local: int}\ntype Request = {...Settings}").unwrap();
    let Some(TypeExpr::Shape(fields)) = TypeChecker::new()
        .with_imported_type_decls(imported)
        .resolve_declared_type(&shadow, "Request")
    else {
        panic!("local declaration precedence")
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "local");
    let unresolved = crate::parse_source("type Request = {...Missing}").unwrap();
    let Some(TypeExpr::OpenShape { rests, .. }) =
        TypeChecker::new().resolve_declared_type(&unresolved, "Request")
    else {
        panic!("unresolved row stays unresolved")
    };
    assert_eq!(rests, vec![TypeExpr::Named("Missing".into())]);
}

#[rstest::rstest]
#[case::list_of_generic_aliases_binds_the_nested_argument(
    r"
type Step<T> = {value: T}

fn int_step() -> Step<int> { return {value: 1} }
fn values<T>(steps: list<Step<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong: list<string> = values([int_step()])
}
",
    "expected list<string>, found list<int>",
    true
)]
#[case::nullable_alias_fields_bind_the_non_nil_argument(
    r"
type MaybeStep<T> = {value: T?}

fn values<T>(steps: list<MaybeStep<T>>) -> list<T> { return [] }

pipeline t(task) {
  let wrong: list<string> = values([{value: 1}])
}
",
    "expected list<string>, found list<int>",
    true
)]
fn diagnostic_message_errors(#[case] source: &str, #[case] message: &str, #[case] present: bool) {
    let diagnostics = errors(source);
    assert_eq!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains(message)),
        present,
        "{message}: {diagnostics:?}"
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
