//! Builtin value methods typed from the method registry: result types,
//! removed spellings and their repairs, arity, and mutating methods.

use super::*;

fn with_code(source: &str, code: Code) -> Vec<TypeDiagnostic> {
    check_source_with_source(source)
        .into_iter()
        .filter(|d| d.code == code)
        .collect()
}

/// Apply every machine-applicable fix, right to left, and return the result.
fn apply_fixes(source: &str) -> String {
    let mut edits: Vec<harn_lexer::FixEdit> = check_source_with_source(source)
        .into_iter()
        .filter_map(|d| d.machine_applicable_fix().map(<[_]>::to_vec))
        .flatten()
        .collect();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    let mut out = source.to_string();
    for edit in edits {
        out.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    out
}

#[test]
fn methods_from_other_receivers_are_rejected_on_numbers_and_bools() {
    let errs = errors(
        r"fn f() {
  const flag = true
  const count = 1
  const fraction = 1.5
  flag.lower()
  count.len()
  fraction.upper()
}
fn nullable(flag: bool?, count: int?, fraction: float?) {
  flag?.lower()
  count?.len()
  fraction?.upper()
}
fn mixed(value: int | string) {
  value.lower()
}",
    );
    assert_eq!(errs.len(), 7, "{errs:?}");
    for (error, (method, receiver)) in errs.iter().zip([
        ("lower", "bool"),
        ("len", "int"),
        ("upper", "float"),
        ("lower", "bool"),
        ("len", "int"),
        ("upper", "float"),
        ("lower", "int"),
    ]) {
        assert!(
            error.contains(&format!("method `{method}` does not exist on `{receiver}`")),
            "{error}"
        );
    }
    let valid = errors(
        r#"fn f(value: string?) { "A".lower(); [1].len(); "a".upper(); value?.lower() }
fn gradual(value: unknown) { value.lower() }"#,
    );
    assert!(valid.is_empty(), "{valid:?}");
}

#[test]
fn registry_return_types_flow_into_bindings() {
    // Each annotation is deliberately wrong, so the mismatch message names
    // the type the registry inferred.
    let errs = errors(
        r#"fn f() {
  const a: int = "A".lower()
  const b: string = [1, 2].len()
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected string, found int")),
        "{errs:?}"
    );
}

#[test]
fn element_generics_bind_from_receiver_and_callbacks() {
    let errs = errors(
        r#"fn f() {
  const xs: list<int> = [1, 2]
  const a: list<bool> = xs.map({ x -> x + 1 })
  const b: string = xs.windows(2)
  const counts: dict<string, int> = {k: 1}
  const c: string = counts.map_keys({ k -> k + "!" })
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected list<bool>, found list<int>")),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected string, found list<list<int>>")),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected string, found dict<string, int>")),
        "{errs:?}"
    );
}

#[test]
fn removed_spelling_is_an_error_with_a_rename_repair() {
    let source = r#"fn f() {
  const s: string = "Hi"
  const t = s.to_lower()
  const n = [1].count()
  return "${s.uppercase()}" + t + to_string(n)
}"#;
    let diags = with_code(source, Code::RemovedBuiltinMethod);
    assert_eq!(diags.len(), 3, "{diags:?}");
    assert!(diags
        .iter()
        .all(|d| d.severity == DiagnosticSeverity::Error));
    assert!(
        diags[0].message.contains("did you mean `lower`"),
        "{diags:?}"
    );
    assert_eq!(
        apply_fixes(source),
        r#"fn f() {
  const s: string = "Hi"
  const t = s.lower()
  const n = [1].len()
  return "${s.upper()}" + t + to_string(n)
}"#
    );
}

#[test]
fn removed_spelling_on_an_unknown_receiver_is_flagged_only_when_unambiguous() {
    let diags = with_code(
        r"fn f(x) {
  x.to_lower()
  x.count()
}",
        Code::RemovedBuiltinMethod,
    );
    // `count` is still an iterator method, so an untyped receiver may be one.
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(diags[0].message.contains("to_lower"), "{diags:?}");
}

#[test]
fn appending_becomes_push_or_concat() {
    let source = r"fn f() {
  let xs = [1]
  xs = xs.appending(2)
  const ys = xs.appending(3)
  const n = xs.appending(4).len()
  return [ys, n]
}";
    assert_eq!(
        apply_fixes(source),
        r"fn f() {
  let xs = [1]
  xs.push(2)
  const ys = xs + [3]
  const n = (xs + [4]).len()
  return [ys, n]
}"
    );
}

#[test]
fn mutating_methods_need_a_mutable_place() {
    let errs = errors(
        r"fn f(param: list<int>) {
  const frozen = [1]
  frozen.push(2)
  param.pop()
  [1, 2].sort()
  let fine = [3, 1]
  fine.sort()
  fine.push(4)
  const last = fine.pop()
  return last
}",
    );
    assert_eq!(errs.len(), 3, "{errs:?}");
    assert!(errs[0].contains("mutates 'frozen'"), "{errs:?}");
    assert!(errs[1].contains("mutates 'param'"), "{errs:?}");
    // A temporary's `sort` can only have meant the copy, so it repairs to
    // `sorted` rather than reporting the missing place.
    assert!(errs[2].contains("use `sorted()`"), "{errs:?}");
}

#[test]
fn pop_returns_the_element_type_or_nil() {
    let errs = errors(
        r"fn f() {
  let xs: list<int> = [1]
  const x: int = xs.pop()
}",
    );
    assert!(
        errs.iter().any(|e| e.contains("expected int, found int?")),
        "{errs:?}"
    );
}

#[test]
fn builtin_method_arity_is_checked() {
    let errs = errors(
        r"fn f() {
  const xs = [1, 2, 3]
  xs.first(2)
  xs.take(2)
}",
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].contains("`list.first` expects 0 arguments"),
        "{errs:?}"
    );
}

#[test]
fn copy_meaning_of_a_now_mutating_method_repairs_to_its_counterpart() {
    let source = r"fn f(rows) {
  let xs = [2, 1]
  const ys = xs.sort()
  const zs = rows.sort_by({ r -> r })
  xs.sort()
  return [ys, zs, xs.reverse()]
}";
    let diags = with_code(source, Code::RemovedBuiltinMethod);
    assert_eq!(diags.len(), 3, "{diags:?}");
    assert_eq!(
        apply_fixes(source),
        r"fn f(rows) {
  let xs = [2, 1]
  const ys = xs.sorted()
  const zs = rows.sorted_by({ r -> r })
  xs.sort()
  return [ys, zs, xs.reversed()]
}"
    );
}

#[test]
fn old_alias_update_idioms_become_mutating_calls() {
    let source = r#"fn f(seen) {
  let xs = [1]
  xs = xs.push(2)
  let tags = set(1)
  tags = tags.adding(2)
  let d = {a: 1, b: 2}
  d = d.removing("a")
  d = d.remove("b")
  seen = seen.adding(3)
  return [xs, tags, d]
}"#;
    assert_eq!(
        apply_fixes(source),
        r#"fn f(seen) {
  let xs = [1]
  xs.push(2)
  let tags = set(1)
  tags.add(2)
  let d = {a: 1, b: 2}
  d.remove("a")
  d.remove("b")
  seen.add(3)
  return [xs, tags, d]
}"#
    );
}
