use super::*;
use crate::diagnostic_codes::Code;

#[test]
fn explicit_tuple_constructor_refines_constant_indexes() {
    let errs = errors(
        r#"pipeline t(task) {
  const row = tuple(7, "seven")
  const first: int = row[0]
  const last: string = row[-1]
}"#,
    );
    assert!(errs.is_empty(), "expected precise tuple reads: {errs:?}");
}

#[test]
fn ordinary_bracket_literals_remain_lists_by_default() {
    let errs = errors(
        r#"pipeline t(task) {
  const value: string = ["a", "b"][0]
}"#,
    );
    assert!(
        errs.iter()
            .any(|error| error.contains("expected string, found string?")),
        "default list inference must retain index soundness: {errs:?}"
    );
}

#[test]
fn source_function_can_shadow_tuple_builtin() {
    let errs = errors(
        r#"fn tuple(value: int) -> string { return "user:${value}" }
pipeline t(task) {
  const value: string = tuple(1)
}"#,
    );
    assert!(
        errs.is_empty(),
        "source call resolution must precede builtin tuple inference: {errs:?}"
    );
}

#[test]
fn contextual_tuple_type_checks_bracket_literal_positions_and_arity() {
    let ok = errors(
        r#"fn consume(row: tuple<string, int>) -> int { return row[1] }
pipeline t(task) {
  const row: tuple<string, int> = ["age", 42]
  const value: int = consume(["count", 3])
}"#,
    );
    assert!(
        ok.is_empty(),
        "contextual tuple literal should type-check: {ok:?}"
    );

    let wrong_position = errors(
        r#"pipeline t(task) {
  const row: tuple<string, int> = ["age", "old"]
}"#,
    );
    assert!(
        wrong_position
            .iter()
            .any(|error| error.contains("expected int, found string")),
        "tuple positions must be checked independently: {wrong_position:?}"
    );

    let wrong_arity = errors(
        r#"pipeline t(task) {
  const row: tuple<string, int> = ["age"]
}"#,
    );
    assert!(
        wrong_arity
            .iter()
            .any(|error| error.contains("tuple<string, int>")),
        "tuple arity must be part of assignability: {wrong_arity:?}"
    );
}

#[test]
fn contextual_tuple_contracts_project_through_lists_dicts_shapes_and_aliases() {
    let ok = errors(
        r#"type Row = tuple<string, int>
type Payload = {primary: Row, rows: list<Row>}
pipeline t(task) {
  const payload: Payload = {
    primary: ["first", 1],
    rows: [["second", 2]],
  }
  const lookup: dict<string, Row> = {third: ["third", 3]}
  const first: string = payload.primary[0]
  const count: int = lookup.third[1]
}"#,
    );
    assert!(
        ok.is_empty(),
        "tuple context should project through compound contracts: {ok:?}"
    );
}

#[test]
fn contextual_tuple_lists_validate_spread_sources() {
    let ok = errors(
        r#"pipeline t(task) {
  const first: list<tuple<string, int>> = [["a", 1]]
  const rows: list<tuple<string, int>> = [...first, ["b", 2]]
}"#,
    );
    assert!(
        ok.is_empty(),
        "tuple list spreads should retain their element contract: {ok:?}"
    );

    let bad = errors(
        r#"pipeline t(task) {
  const names: list<string> = ["a"]
  const rows: list<tuple<string, int>> = [...names]
}"#,
    );
    assert!(
        bad.iter()
            .any(|error| error.contains("expected list<tuple<string, int>>")),
        "tuple list spreads must validate the spread source: {bad:?}"
    );
}

#[test]
fn tuple_positions_preserve_contextual_closure_checking() {
    let errs = errors(
        r#"pipeline t(task) {
  const callbacks: tuple<fn(int) -> int, string> = [
    { value -> value + "wrong" },
    "label",
  ]
}"#,
    );
    assert!(
        errs.iter()
            .any(|error| error.contains("can't add int and string")),
        "tuple context must reach closure parameter and return types: {errs:?}"
    );
}

#[test]
fn constant_tuple_out_of_bounds_is_a_static_error() {
    let diagnostics = check_source(
        r#"pipeline t(task) {
  const row = tuple("a", 1)
  log(row[2])
  log(row[-3])
}"#,
    );
    let bounds = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::TupleIndexOutOfBounds)
        .collect::<Vec<_>>();
    assert_eq!(
        bounds.len(),
        2,
        "expected both bounds errors: {diagnostics:?}"
    );
}

#[test]
fn dynamic_tuple_read_is_union_with_nil() {
    let errs = errors(
        r#"pipeline t(task) {
  const row = tuple(7, "seven")
  let index: int = 0
  const value: int | string | nil = row[index]
}"#,
    );
    assert!(
        errs.is_empty(),
        "dynamic reads use the sound list rule: {errs:?}"
    );
}

#[test]
fn tuple_widens_to_list_but_arbitrary_list_does_not_narrow_to_tuple() {
    let ok = errors(
        r#"fn consume(values: list<int | string>) -> nil { return nil }
pipeline t(task) {
  consume(tuple(1, "one"))
}"#,
    );
    assert!(
        ok.is_empty(),
        "tuple should widen element-wise to list: {ok:?}"
    );

    let bad = errors(
        r#"fn consume(values: tuple<int, string>) -> nil { return nil }
pipeline t(task) {
  const values: list<int | string> = [1, "one"]
  consume(values)
}"#,
    );
    assert!(
        bad.iter()
            .any(|error| error.contains("expected tuple<int, string>")),
        "an arbitrary list cannot prove tuple arity: {bad:?}"
    );
}

#[test]
fn tuple_iteration_and_nested_list_preserve_positional_type() {
    let errs = errors(
        r#"pipeline t(task) {
  const rows: list<tuple<string, int>> = [["a", 1], ["b", 2]]
  for row in rows {
    const name: string = row[0]
    const count: int = row[1]
  }
  for value in tuple(1, "one") {
    const widened: int | string = value
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "iteration should preserve tuple contracts: {errs:?}"
    );
}

#[test]
fn tuple_destructuring_is_positional_and_dynamic_writes_are_safe() {
    let ok = errors(
        r#"pipeline t(task) {
  let row = tuple(1, "one")
  let [count, name] = row
  const typed_count: int = count
  const typed_name: string = name
  row[0] = 2
}"#,
    );
    assert!(
        ok.is_empty(),
        "tuple destructuring/write should be precise: {ok:?}"
    );

    let wrong_slot = errors(
        r#"pipeline t(task) {
  let row = tuple(1, "one")
  row[0] = "wrong"
}"#,
    );
    assert!(
        wrong_slot
            .iter()
            .any(|error| error.contains("expected int, found string")),
        "constant writes must use the positional slot type: {wrong_slot:?}"
    );

    let dynamic = errors(
        r#"pipeline t(task) {
  let row = tuple(1, "one")
  let index: int = 0
  row[index] = 2
}"#,
    );
    assert!(
        !dynamic.is_empty(),
        "a dynamic heterogeneous tuple write cannot be sound for every slot"
    );
}

#[test]
fn arity_changing_tuple_operations_widen_to_lists() {
    let errs = errors(
        r#"pipeline t(task) {
  const row = tuple(1, "one")
  const appended: list<int | string | bool> = row.appending(true)
  const sliced: list<int | string> = row[0:1]
  const reversed: list<int | string> = row.reversed()
}"#,
    );
    assert!(
        errs.is_empty(),
        "arity-changing operations should forget positions: {errs:?}"
    );
}
