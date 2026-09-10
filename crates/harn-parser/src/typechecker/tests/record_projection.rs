use super::*;

#[test]
fn pick_preserves_heterogeneous_fields_and_capabilities() {
    let diagnostics = errors(
        r#"
type Context = {env: HarnessEnv, fs: HarnessFs, tools: HarnessTools}
fn consume(ctx: Context) -> string? { return ctx.env.get("PICK_TEST_VALUE") }
fn main(harness: Harness) {
  consume(pick(harness, ["env", "fs", "tools"]))
  const row = pick({name: "Ada", age: 37, absent: nil}, ["name", "age", "absent"])
  const name: string = row.name
  const age: int = row.age
  const absent: nil = row.absent
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let wrong_value = errors(
        r#"fn main(harness: Harness) {
  const row = pick({name: "Ada", age: 37}, ["age"])
  const bad: string = row.age
}"#,
    );
    assert!(
        wrong_value
            .iter()
            .any(|error| error.contains("expected string, found int")),
        "{wrong_value:?}"
    );
}

#[test]
fn pick_rejects_typos_and_unselected_fields() {
    for source in [
        r#"fn main(harness: Harness) { pick(harness, ["fss"]) }"#,
        r#"fn main(harness: Harness) { pick({name: "Ada"}, ["nmae"]) }"#,
    ] {
        let diagnostics = check_source(source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ArgumentTypeMismatch
                    && diagnostic.message.contains("unknown field")),
            "{diagnostics:?}"
        );
    }
    let diagnostics = errors(
        r#"fn main(harness: Harness) {
  const selected = pick(harness, ["env"])
  selected.fs.read_text("unused")
}"#,
    );
    assert!(
        !diagnostics.is_empty(),
        "unselected capability must be rejected"
    );
}

#[test]
fn pick_empty_duplicate_optional_and_dynamic_keys_are_sound() {
    let diagnostics = errors(
        r#"
type Person = {name: string, age?: int}
fn project_person(person: Person, keys: list<string>) {
  const empty: {} = pick(person, [])
  const named: {name: string} = pick(person, ["name", "name"])
  const optional: {age?: int} = pick(person, ["age"])
  const dynamic: {name?: string, age?: int} = pick(person, keys)
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    for body in [
        "const bad: {name: string} = pick(person, keys)",
        r#"const bad: {age: int} = pick(person, ["age"])"#,
    ] {
        let diagnostics = errors(&format!("type Person = {{name: string, age?: int}}\nfn project_person(person: Person, keys: list<string>) {{ {body} }}"));
        assert!(
            !diagnostics.is_empty(),
            "must reject falsely required fields: {body}"
        );
    }
}

#[test]
fn pick_preserves_aliases_union_branches_and_typed_maps() {
    let diagnostics = errors(
        r#"
type Input = {kind: "text", value: string} | {kind: "number", value: int}
fn project_input(input: Input, scores: dict<string, int>) {
  const selected: Input = pick(input, ["kind", "value"])
  const score: {alice?: int} = pick(scores, ["alice"])
}
fn open_record(input: {name: string, ...dict<string, int>}) {
  const selected: {name: string, extra?: int} = pick(input, ["name", "extra"])
}
struct Pair<A, B> { first: A, second: B }
fn generic_record(input: Pair<int, string>) {
  const selected: {first: int, second: string} = pick(input, ["first", "second"])
}
fn intersected_record(input: {name: string} & {age: int}) {
  const selected: {name: string, age: int} = pick(input, ["name", "age"])
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn pick_does_not_capture_shadowing_functions_or_values() {
    for source in [
        r#"fn pick(value: int) -> string { return "local" }
fn main(harness: Harness) { const value: string = pick(1) }"#,
        r#"fn main(harness: Harness) {
const pick = {value: int -> "local"}
const value: string = pick(1)
}"#,
    ] {
        let diagnostics = errors(source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}

#[test]
fn pick_rejects_invalid_sources_keys_and_arity() {
    for expression in ["pick(42, [])", "pick(nil, [])", "pick({}, [1])"] {
        let diagnostics = errors(&format!("fn main(harness: Harness) {{ {expression} }}"));
        assert!(!diagnostics.is_empty(), "must reject {expression}");
    }
    for expression in ["pick({})", "pick({}, [], [])"] {
        let diagnostics = check_source(&format!("fn main(harness: Harness) {{ {expression} }}"));
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::BuiltinArity),
            "{diagnostics:?}"
        );
    }
}

#[test]
fn pick_spread_arguments_do_not_invent_field_types() {
    let diagnostics = errors(
        r"fn project(args: list<any>) {
  const selected: dict<string, unknown> = pick(...args)
}",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let diagnostics = errors(
        r"fn project(args: list<any>) {
  const selected = pick(...args)
  const name: string = selected.name
}",
    );
    assert!(
        !diagnostics.is_empty(),
        "spread must not invent a field type"
    );
}

#[test]
fn pick_constant_key_lists_and_pipe_calls_keep_precision() {
    let diagnostics = errors(
        r#"fn main(harness: Harness) {
  const fields = ["env", "fs"]
  const alias = fields
  const selected: {env: HarnessEnv, fs: HarnessFs} = pick(harness, alias)
  const key = "tools"
  const tools: {tools: HarnessTools} = harness |> pick(_, [key])
}"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let wrong = errors(
        r#"fn main(harness: Harness) {
      const selected = harness |> pick(_, ["env"])
      const alias = selected
      const wrong: HarnessFs = alias.env
      alias.tools
    }"#,
    );
    assert!(
        wrong
            .iter()
            .any(|error| error.contains("expected HarnessFs, found HarnessEnv")),
        "{wrong:?}"
    );
    assert!(
        wrong
            .iter()
            .any(|error| error.contains("tools") && error.contains("does not exist")),
        "{wrong:?}"
    );
}

#[test]
fn optional_record_fields_cannot_satisfy_required_fields() {
    for input in ["{name?: string}", "{name?: string, ...dict}"] {
        let source =
            format!("fn consume(input: {input}) {{ const required: {{name: string}} = input }}");
        let diagnostics = errors(&source);
        assert!(
            !diagnostics.is_empty(),
            "optional field must not prove presence: {source}"
        );
    }
}

#[test]
fn pick_key_alias_captures_the_original_literal_across_shadowing() {
    let diagnostics = errors(
        r#"fn main(harness: Harness) {
  const keys = ["env"]
  const saved = keys
  const key = "fs"
  const saved_key = key
  if true {
    const keys = ["tools"]
    const key = "tools"
    const selected: {env: HarnessEnv} = pick(harness, saved)
    const scalar: {fs: HarnessFs} = pick(harness, [saved_key])
  }
}"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn pick_result_aliases_capture_the_contract_before_shadowing() {
    let source = r#"
fn main(harness: Harness) {
  const selected = pick(harness, ["env"])
  if true {
    const saved = selected
    const selected = saved
    const final_copy = selected
    const env: HarnessEnv = final_copy.env
  }
}
"#;
    assert!(errors(source).is_empty());
    let invalid = source.replace("const env: HarnessEnv = final_copy.env", "final_copy.fs");
    let diagnostics = errors(&invalid);
    assert!(
        diagnostics
            .iter()
            .any(|error| error.contains("fs") && error.contains("does not exist")),
        "{diagnostics:?}"
    );
}

#[test]
fn projected_contract_does_not_follow_an_unrelated_shadowing_binding() {
    let source = r#"
fn main(harness: Harness) {
  const selected = pick(harness, ["env"])
  if true {
    const selected = {name: "Ada"}
    const absent = selected.missing
  }
}
"#;
    let diagnostics = errors(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn mutable_pick_aliases_and_narrowed_records_keep_their_contract() {
    for source in [
        r#"fn main(harness: Harness) {
  let selected = pick(harness, ["env"])
  const alias = selected
  alias.fs
}"#,
        r#"fn use(input: {kind: "a", name: string} | {kind: "b", age: int}) {
  const selected = pick(input, ["kind"])
  if selected.kind == "a" { selected.missing }
}"#,
    ] {
        let diagnostics = errors(source);
        assert!(
            diagnostics
                .iter()
                .any(|error| error.contains("does not exist")),
            "{source}\n{diagnostics:?}"
        );
    }
}
