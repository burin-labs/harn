use super::*;

const RULE: &str = "prefer-pick";

fn lint_source(source: &str) -> Vec<LintDiagnostic> {
    let tokens = Lexer::new(source).tokenize().unwrap();
    let program = Parser::new(tokens).parse().unwrap();
    let diagnostics = harn_parser::TypeChecker::new()
        .check_with_facts(&program, source)
        .diagnostics;
    lint_diagnostics_from_type_diagnostics(&diagnostics, &[])
}

#[test]
fn harness_capability_record_becomes_pick_and_keeps_behavior() {
    let source = r#"
type Context = {env: HarnessEnv, fs: HarnessFs}

fn config_exists(ctx: Context) -> bool {
  return ctx.fs.exists(ctx.env.get_or("PREFER_PICK_TEST_MISSING", "."))
}

fn main(harness: Harness) {
  const ctx = {env: harness.env, fs: harness.fs}
  assert_eq(config_exists(ctx), true)
  harness.stdio.println("pass")
}
"#;
    let diagnostics = lint_source(source);
    assert_eq!(count_rule(&diagnostics, RULE), 1, "{diagnostics:?}");
    assert_eq!(execute_strict_source(source), "pass");
    let fixed = apply_fixes(source, &diagnostics);
    assert!(
        fixed.contains(r#"const ctx = pick(harness, ["env", "fs"])"#),
        "{fixed}"
    );
    assert_eq!(execute_strict_source(&fixed), "pass");
    assert!(!has_rule(&lint_source(&fixed), RULE));
}

#[test]
fn typed_record_parameter_and_struct_value_are_rewritten() {
    let source = r#"
type Person = {name: string, age: int}
struct Point { x: int, y: int }

fn describe(person: Person, point: Point) -> string {
  const who = {name: person.name, age: person.age}
  const at = {x: point.x, y: point.y}
  return "${who.name}:${who.age}@${at.x},${at.y}"
}

fn main(harness: Harness) {
  assert_eq(describe({name: "Ada", age: 37}, Point {x: 1, y: 2}), "Ada:37@1,2")
  harness.stdio.println("pass")
}
"#;
    let diagnostics = lint_source(source);
    assert_eq!(count_rule(&diagnostics, RULE), 2, "{diagnostics:?}");
    assert_eq!(execute_strict_source(source), "pass");
    let fixed = apply_fixes(source, &diagnostics);
    assert!(
        fixed.contains(r#"pick(person, ["name", "age"])"#),
        "{fixed}"
    );
    assert!(fixed.contains(r#"pick(point, ["x", "y"])"#), "{fixed}");
    assert_eq!(execute_strict_source(&fixed), "pass");
}

#[test]
fn inferred_record_binding_is_rewritten() {
    let source = r#"
fn main(harness: Harness) {
  const source = {name: "Ada", age: 37, city: "Paris"}
  const copy = {name: source.name, city: source.city}
  harness.stdio.println(copy.name)
}
"#;
    let diagnostics = lint_source(source);
    assert_eq!(count_rule(&diagnostics, RULE), 1, "{diagnostics:?}");
    let fixed = apply_fixes(source, &diagnostics);
    assert!(
        fixed.contains(r#"pick(source, ["name", "city"])"#),
        "{fixed}"
    );
}

#[test]
fn stays_silent_when_a_missing_key_would_change_the_result() {
    for source in [
        // A dictionary may not hold the key: the literal reads `nil`, `pick`
        // omits the field.
        r"
fn copy(settings: dict<string, int>) -> dict<string, int> {
  return {retries: settings.retries, timeout: settings.timeout}
}
",
        // An optional field behaves the same way.
        r"
type Person = {name: string, age?: int}
fn copy(person: Person) { return {name: person.name, age: person.age} }
",
        // A type the file cannot see is not proven to be a record.
        r#"
import { Settings } from "./settings"
fn copy(settings: Settings) { return {a: settings.a, b: settings.b} }
"#,
    ] {
        let diagnostics = lint_source(source);
        assert!(!has_rule(&diagnostics, RULE), "{source}\n{diagnostics:?}");
    }
}

#[test]
fn stays_silent_when_the_literal_is_not_a_plain_projection() {
    for source in [
        // One field gains nothing from `pick`.
        "fn main(harness: Harness) { const ctx = {env: harness.env} }",
        // A renamed field is not a projection.
        "fn main(harness: Harness) { const ctx = {environment: harness.env, fs: harness.fs} }",
        // Two different sources.
        "fn main(harness: Harness, other: Harness) { const ctx = {env: harness.env, fs: other.fs} }",
        // An extra computed entry.
        "fn main(harness: Harness) { const ctx = {env: harness.env, fs: harness.fs, n: 1} }",
        // Optional chaining is not a plain field read.
        r"
type Person = {name: string, age: int}
fn copy(person: Person?) { return {name: person?.name, age: person?.age} }
",
        // A capability that does not exist cannot be picked.
        "fn main(harness: Harness) { const ctx = {env: harness.env, nope: harness.nope} }",
        // A local `harness` that is not the host handle is untyped.
        "fn helper(harness: any) { return {env: harness.env, fs: harness.fs} }",
    ] {
        let diagnostics = lint_source(source);
        assert!(!has_rule(&diagnostics, RULE), "{source}\n{diagnostics:?}");
    }
}

#[test]
fn stays_silent_when_pick_is_shadowed() {
    let source = r"
fn pick(value: int) -> int { return value }
fn main(harness: Harness) {
  const ctx = {env: harness.env, fs: harness.fs}
  harness.stdio.println(pick(1))
}
";
    let diagnostics = lint_source(source);
    assert!(!has_rule(&diagnostics, RULE), "{diagnostics:?}");
}

#[test]
fn repair_is_behavior_preserving_and_machine_applicable() {
    let source = "fn main(harness: Harness) { const ctx = {env: harness.env, fs: harness.fs} }";
    let diagnostics = lint_source(source);
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule == RULE)
        .expect("prefer-pick fires");
    let repair = diagnostic.repair().expect("repair template");
    assert_eq!(repair.id.as_str(), "records/pick-fields");
    assert!(repair.safety.is_machine_applicable());
    assert!(diagnostic.machine_applicable_fix().is_some());
}

#[test]
fn aliases_unions_nested_records_and_contextual_results_use_checker_types() {
    for source in [
        r"
type Person = {name: string, age: int}
type Alias = Person
fn copy(person: Alias) -> Person { return {name: person.name, age: person.age} }
",
        r#"
type Choice = {kind: "a", value: string} | {kind: "b", value: int}
fn copy(choice: Choice) { return {kind: choice.kind, value: choice.value} }
"#,
        r"
type Person = {name: string, age: int}
fn copy(wrapper: {person: Person}) -> Person {
  return {name: wrapper.person.name, age: wrapper.person.age}
}
",
        r"
struct Pair<A, B> { first: A, second: B }
fn copy(pair: Pair<int, string>) -> {first: int, second: string} {
  const result: {first: int, second: string} = {first: pair.first, second: pair.second}
  return result
}
",
        r"
fn copy(source: {left: tuple<int, string>, right: tuple<int, string>}) -> {left: tuple<int, string>, right: tuple<int, string>} {
  return {left: source.left, right: source.right}
}
",
    ] {
        let diagnostics = lint_source(source);
        assert_eq!(
            count_rule(&diagnostics, RULE),
            1,
            "{source}\n{diagnostics:?}"
        );
        let fixed = apply_fixes(source, &diagnostics);
        assert!(!has_rule(&lint_source(&fixed), RULE), "{fixed}");
    }
}

#[test]
fn shadowing_mutation_missing_fields_and_comments_prevent_unsafe_rewrites() {
    for source in [
        r"
fn copy(source: {a: int, b: int}) {
  const shadow = {source -> {a: source.a, b: source.b}}
  return shadow({})
}
",
        r"
fn copy(source: {a: int, b: int}) {
  fn inner(pick: any) { return {a: source.a, b: source.b} }
  return inner(nil)
}
",
        r"
fn copy(source: {nested?: {a: int, b: int}}) {
  return {a: source.nested.a, b: source.nested.b}
}
",
        r"
fn copy(source: {a: int, b: int} | {a: int, b?: int}) {
  return {a: source.a, b: source.b}
}
",
        r"
fn copy() {
  let source: dict<string, int> = {a: 1, b: 2}
  source = {}
  return {a: source.a, b: source.b}
}
",
        r"
fn copy(source: {a: int, b: int}) {
  return {a: source.a, /* Keep the second value. */ b: source.b}
}
",
        r"
fn next() -> {a: int, b: int} { return {a: 1, b: 2} }
fn copy() { return {a: next().a, b: next().b} }
",
    ] {
        let diagnostics = lint_source(source);
        assert!(!has_rule(&diagnostics, RULE), "{source}\n{diagnostics:?}");
    }
}
