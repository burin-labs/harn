//! Auto-fix generation, never/any/unknown subtyping, reachability, union helpers.

use crate::ast::*;

use super::super::format_type;
use super::super::scope::TypeScope;
use super::super::union::{remove_from_union, simplify_union};
use super::*;

#[test]
fn test_fix_string_plus_int_literal() {
    let source = "pipeline t(task) {\n  let x = \"hello \" + 42\n  log(x)\n}";
    let diags = check_source_with_source(source);
    let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
    assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
    let fix = fixable[0].fix.as_ref().unwrap();
    assert_eq!(fix.len(), 1);
    assert_eq!(fix[0].replacement, "\"hello ${42}\"");
}

#[test]
fn test_fix_int_plus_string_literal() {
    let source = "pipeline t(task) {\n  let x = 42 + \"hello\"\n  log(x)\n}";
    let diags = check_source_with_source(source);
    let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
    assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
    let fix = fixable[0].fix.as_ref().unwrap();
    assert_eq!(fix[0].replacement, "\"${42}hello\"");
}

#[test]
fn test_fix_string_plus_variable() {
    let source = "pipeline t(task) {\n  let n: int = 5\n  let x = \"count: \" + n\n  log(x)\n}";
    let diags = check_source_with_source(source);
    let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
    assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
    let fix = fixable[0].fix.as_ref().unwrap();
    assert_eq!(fix[0].replacement, "\"count: ${n}\"");
}

#[test]
fn test_no_fix_int_plus_int() {
    // int + float should error but no interpolation fix
    let source =
        "pipeline t(task) {\n  let x: int = 5\n  let y: float = 3.0\n  let z = x - y\n  log(z)\n}";
    let diags = check_source_with_source(source);
    let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
    assert!(
        fixable.is_empty(),
        "no fix expected for numeric ops, got: {fixable:?}"
    );
}

#[test]
fn test_no_fix_without_source() {
    let source = "pipeline t(task) {\n  let x = \"hello \" + 42\n  log(x)\n}";
    let diags = check_source(source);
    let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
    assert!(
        fixable.is_empty(),
        "without source, no fix should be generated"
    );
}

#[test]
fn test_union_exhaustive_match_no_warning() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: string | int | nil = nil
  match x {
"hello" -> { log("s") }
42 -> { log("i") }
nil -> { log("n") }
  }
}"#,
    );
    let union_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive match on union"))
        .collect();
    assert!(union_warns.is_empty());
}

#[test]
fn test_union_non_exhaustive_match_errors() {
    // Phase C: missing-variant `match` is now a hard error, not a warning.
    let errs = errors(
        r#"pipeline t(task) {
  let x: string | int | nil = nil
  match x {
"hello" -> { log("s") }
42 -> { log("i") }
  }
}"#,
    );
    let union_errs: Vec<_> = errs
        .iter()
        .filter(|e| e.contains("Non-exhaustive match on union"))
        .collect();
    assert_eq!(union_errs.len(), 1, "got: {:?}", errs);
    assert!(union_errs[0].contains("nil"));
}

#[test]
fn test_nil_coalesce_non_union_preserves_left_type() {
    // When left is a known non-nil type, ?? should preserve it
    let errs = errors(
        r#"pipeline t(task) {
  let x: int = 42
  let y: int = x ?? 0
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_nil_coalesce_nil_returns_right_type() {
    let errs = errors(
        r#"pipeline t(task) {
  let x: string = nil ?? "fallback"
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_never_is_subtype_of_everything() {
    let tc = TypeChecker::new();
    let scope = TypeScope::new();
    assert!(tc.types_compatible(&TypeExpr::Named("string".into()), &TypeExpr::Never, &scope));
    assert!(tc.types_compatible(&TypeExpr::Named("int".into()), &TypeExpr::Never, &scope));
    assert!(tc.types_compatible(
        &TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("nil".into()),
        ]),
        &TypeExpr::Never,
        &scope,
    ));
}

#[test]
fn test_nothing_is_subtype_of_never() {
    let tc = TypeChecker::new();
    let scope = TypeScope::new();
    assert!(!tc.types_compatible(&TypeExpr::Never, &TypeExpr::Named("string".into()), &scope));
    assert!(!tc.types_compatible(&TypeExpr::Never, &TypeExpr::Named("int".into()), &scope));
}

#[test]
fn test_never_never_compatible() {
    let tc = TypeChecker::new();
    let scope = TypeScope::new();
    assert!(tc.types_compatible(&TypeExpr::Never, &TypeExpr::Never, &scope));
}

#[test]
fn test_any_is_top_type_bidirectional() {
    let tc = TypeChecker::new();
    let scope = TypeScope::new();
    let any = TypeExpr::Named("any".into());
    // Every concrete type flows into any.
    assert!(tc.types_compatible(&any, &TypeExpr::Named("string".into()), &scope));
    assert!(tc.types_compatible(&any, &TypeExpr::Named("int".into()), &scope));
    assert!(tc.types_compatible(&any, &TypeExpr::Named("nil".into()), &scope));
    assert!(tc.types_compatible(
        &any,
        &TypeExpr::List(Box::new(TypeExpr::Named("int".into()))),
        &scope
    ));
    // any flows back out to every concrete type (escape hatch).
    assert!(tc.types_compatible(&TypeExpr::Named("string".into()), &any, &scope));
    assert!(tc.types_compatible(&TypeExpr::Named("nil".into()), &any, &scope));
}

#[test]
fn test_unknown_is_safe_top_one_way() {
    let tc = TypeChecker::new();
    let scope = TypeScope::new();
    let unknown = TypeExpr::Named("unknown".into());
    // Every concrete type flows into unknown.
    assert!(tc.types_compatible(&unknown, &TypeExpr::Named("string".into()), &scope));
    assert!(tc.types_compatible(&unknown, &TypeExpr::Named("nil".into()), &scope));
    assert!(tc.types_compatible(
        &unknown,
        &TypeExpr::List(Box::new(TypeExpr::Named("int".into()))),
        &scope
    ));
    // unknown does NOT flow back out to concrete types without narrowing.
    assert!(!tc.types_compatible(&TypeExpr::Named("string".into()), &unknown, &scope));
    assert!(!tc.types_compatible(&TypeExpr::Named("int".into()), &unknown, &scope));
    // unknown is compatible with itself.
    assert!(tc.types_compatible(&unknown, &unknown, &scope));
    // unknown flows into any (any accepts everything).
    assert!(tc.types_compatible(&TypeExpr::Named("any".into()), &unknown, &scope));
}

#[test]
fn test_unknown_narrows_via_type_of() {
    // Concrete narrowing behavior is covered end-to-end by the conformance
    // test `unknown_narrowing.harn`; this unit test guards against the
    // refinement path silently regressing to "no narrowing" for named
    // unknown types.
    let errs = errors(
        r#"pipeline t(task) {
  fn f(v: unknown) -> string {
if type_of(v) == "string" {
  return v
}
return "other"
  }
  log(f("hi"))
}"#,
    );
    assert!(
        errs.is_empty(),
        "unknown should narrow to string inside type_of guard: {errs:?}"
    );
}

#[test]
fn test_unknown_without_narrowing_errors() {
    let errs = errors(
        r#"pipeline t(task) {
  let u: unknown = "hello"
  let s: string = u
}"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("unknown")),
        "expected an error mentioning unknown, got: {errs:?}"
    );
}

#[test]
fn test_simplify_union_removes_never() {
    assert_eq!(
        simplify_union(vec![TypeExpr::Never, TypeExpr::Named("string".into())]),
        TypeExpr::Named("string".into()),
    );
    assert_eq!(
        simplify_union(vec![TypeExpr::Never, TypeExpr::Never]),
        TypeExpr::Never,
    );
    assert_eq!(
        simplify_union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Never,
            TypeExpr::Named("int".into()),
        ]),
        TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("int".into()),
        ]),
    );
}

#[test]
fn test_remove_from_union_exhausted_returns_never() {
    let result = remove_from_union(&[TypeExpr::Named("string".into())], "string");
    assert_eq!(result, Some(TypeExpr::Never));
}

#[test]
fn test_if_else_one_branch_throws_infers_other() {
    // if/else where else throws — result should be int (from then-branch)
    let errs = errors(
        r#"pipeline t(task) {
  fn foo(x: bool) -> int {
let result: int = if x { 42 } else { throw "err" }
return result
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_if_else_both_branches_throw_infers_never() {
    // Both branches exit — should infer never, which is assignable to anything
    let errs = errors(
        r#"pipeline t(task) {
  fn foo(x: bool) -> string {
let result: string = if x { throw "a" } else { throw "b" }
return result
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_unreachable_after_return() {
    let warns = warnings(
        r#"pipeline t(task) {
  fn foo() -> int {
return 1
let x = 2
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unreachable")),
        "expected unreachable warning: {warns:?}"
    );
}

#[test]
fn test_unreachable_after_throw() {
    let warns = warnings(
        r#"pipeline t(task) {
  fn foo() {
throw "err"
let x = 2
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unreachable")),
        "expected unreachable warning: {warns:?}"
    );
}

#[test]
fn test_unreachable_after_composite_exit() {
    let warns = warnings(
        r#"pipeline t(task) {
  fn foo(x: bool) {
if x { return 1 } else { throw "err" }
let y = 2
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unreachable")),
        "expected unreachable warning: {warns:?}"
    );
}

#[test]
fn test_no_unreachable_warning_when_reachable() {
    let warns = warnings(
        r#"pipeline t(task) {
  fn foo(x: bool) {
if x { return 1 }
let y = 2
  }
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unreachable")),
        "unexpected unreachable warning: {warns:?}"
    );
}

#[test]
fn test_catch_typed_error_variable() {
    // When catch has a type annotation, the error var should be typed
    let errs = errors(
        r#"pipeline t(task) {
  enum AppError { NotFound, Timeout }
  try {
throw AppError.NotFound
  } catch (e: AppError) {
let x: AppError = e
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_unreachable_with_never_arg_no_error() {
    // After exhaustive narrowing, unreachable(x) should pass
    let errs = errors(
        r#"pipeline t(task) {
  fn foo(x: string | int) {
if type_of(x) == "string" { return }
if type_of(x) == "int" { return }
unreachable(x)
  }
}"#,
    );
    assert!(
        !errs.iter().any(|e| e.contains("unreachable")),
        "unexpected unreachable error: {errs:?}"
    );
}

#[test]
fn test_unreachable_with_remaining_types_errors() {
    // Non-exhaustive narrowing — unreachable(x) should error
    let errs = errors(
        r#"pipeline t(task) {
  fn foo(x: string | int | nil) {
if type_of(x) == "string" { return }
unreachable(x)
  }
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("unreachable") && e.contains("not all cases")),
        "expected unreachable error about remaining types: {errs:?}"
    );
}

#[test]
fn test_unreachable_no_args_no_compile_error() {
    let errs = errors(
        r#"pipeline t(task) {
  fn foo() {
unreachable()
  }
}"#,
    );
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("unreachable") && e.contains("not all cases")),
        "unreachable() with no args should not produce type error: {errs:?}"
    );
}

#[test]
fn test_never_type_annotation_parses() {
    let errs = errors(
        r#"pipeline t(task) {
  fn foo() -> never {
throw "always throws"
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_format_type_never() {
    assert_eq!(format_type(&TypeExpr::Never), "never");
}
