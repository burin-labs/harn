use crate::compiler::CompilerOptions;

use super::tests_runtime::{run_harn, run_harn_result_display_with_options};

fn mixed_pin_probe(binding_keyword: &str) -> String {
    let source = r#"pipeline default(task) {
let pins = []
for version in ["0.8.169", "0.8.168"] {
  __BINDING__ pin = {name: "harn-" + version, version: version}
  pins = pins + [pin]
}
let version = pins[0].version
let mixed = []
for pin in pins {
  if pin.version != version {
    mixed = mixed + [pin]
  }
}
const evidence = pins.map({ pin -> "${pin.name}=${pin.version}" }).join(", ")
if len(mixed) > 0 {
  log("nil|" + evidence)
} else {
  log(version + "|" + evidence)
}
}"#
    .replace("__BINDING__", binding_keyword);

    run_harn(&source).0.trim_end().to_string()
}

#[test]
fn shadowed_loop_bindings_preserve_let_to_const_semantics() {
    let mutable_output = mixed_pin_probe("let");
    let immutable_output = mixed_pin_probe("const");

    assert_eq!(immutable_output, mutable_output);
    assert_eq!(
        immutable_output,
        "[harn] nil|harn-0.8.169=0.8.169, harn-0.8.168=0.8.168"
    );
}

#[test]
fn inherited_pipeline_binding_is_captured_by_child_closure() {
    let (output, _) = run_harn(
        r"pipeline base(task) {
let value = 1
}
pipeline default(task) extends base {
const read = { -> value }
value = value + 1
log(read())
}",
    );

    assert_eq!(output.trim_end(), "[harn] 2");
}

#[test]
fn reassigned_callable_is_observed_by_reference() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
let callable = { -> "outer" }
{
let callable = { -> "before" }
const invoke = { -> callable() }
callable = { -> "after" }
fn callable() { return "later" }
log(invoke() + "|" + callable())
}
log(callable())
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] after|after\n[harn] outer");
}

#[test]
fn environment_backed_captures_preserve_collection_write_paths() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
let items = ["outer"]
let record = {value: "outer", extra: "outer"}
{
let items = ["a"]
let record = {value: "before"}
const read = { -> items.join("") + "|" + record.value + "|" + record.extra }
items = items + ["b"]
record.value = "after"
record["extra"] = "ok"
log(read())
}
log(items.join("") + "|" + record.value + "|" + record.extra)
}"#,
    );

    assert_eq!(
        output.trim_end(),
        "[harn] ab|after|ok\n[harn] outer|outer|outer"
    );
}

#[test]
fn pipeline_before_later_module_binding_captures_shared_cell() {
    let (output, _) = run_harn(
        r"pipeline default(task) {
const increment = { -> counter = counter + 1 }
increment()
log(counter)
}
let counter = 0",
    );

    assert_eq!(output.trim_end(), "[harn] 1");
}

#[test]
fn reassigned_callable_in_match_pattern_is_observed_by_reference() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
let callable = { value -> "before" }
let argument = "before"
const classify = { ->
    match "after" {
        callable(argument) -> { "matched" }
        _ -> { "missed" }
    }
}
callable = { value -> value }
argument = "after"
log(classify())
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] matched");
}

#[test]
fn reassigned_method_receiver_in_match_pattern_is_observed_by_reference() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
let values = ["before"]
const classify = { ->
    match "after" {
        values.join("") -> { "matched" }
        _ -> { "missed" }
    }
}
values = ["after"]
log(classify())
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] matched");
}

#[test]
fn reassigned_outer_binding_in_parameter_default_is_observed_by_reference() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
let value = "before"
fn read(value = value) {
    return value
}
value = "after"
log(read())
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] after");
}

#[test]
fn ambiguous_bare_enum_variant_pattern_is_rejected() {
    let error = crate::compile_source(
        r"enum First {
    Shared(value)
}
enum Second {
    Shared(value)
}
pipeline default(task) {
    match First.Shared(1) {
        Shared(value) -> { log(value) }
    }
}",
    )
    .expect_err("a bare variant shared by two enums must require qualification");

    assert!(
        error.contains("variant `Shared` is declared by enums First, Second"),
        "unexpected ambiguity error: {error}"
    );
}

#[test]
fn captured_builtin_reference_shadows_same_named_builtin_for_all_call_positions() {
    let (output, _) = run_harn(
        r#"fn local_tail(value) {
    const len = to_int
    return len(value)
}
pipeline default(task) {
const len = to_int
const ordinary = { value -> len(value) }
const tail = { value -> return len(value) }
log("${ordinary("42")}|${tail("43")}|${local_tail("44")}")
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] 42|43|44");
}

#[test]
fn loaded_builtin_reference_is_not_resolved_by_name_again() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
const convert = to_int
const to_int = 7
const spread = { args -> convert(...args) }
log("${convert("42")}|${spread(["43"])}")
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] 42|43");
}

#[test]
fn lexical_binding_shadows_special_runtime_name() {
    let (output, _) = run_harn(
        r#"pipeline default(task) {
const cancel = to_int
const ordinary = { value -> cancel(value) }
const tail = { value -> return cancel(value) }
log("${ordinary("42")}|${tail("43")}")
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] 42|43");
}

#[test]
fn captured_non_callable_shadows_same_named_builtin() {
    let error = run_harn_result_display_with_options(
        r#"pipeline default(task) {
const len = 7
const invoke = { -> len("abc") }
invoke()
}"#,
        CompilerOptions::optimized(),
    )
    .expect_err("the captured integer must not fall through to builtin len");

    assert!(error.contains("Cannot call 7"), "unexpected error: {error}");
}

#[test]
fn return_spread_calls_use_the_regular_spread_dispatcher() {
    let (output, _) = run_harn(
        r#"fn add3(first, second, third) {
    return first + second + third
}
fn invoke(callable, args) {
    return callable(...args)
}
fn largest(args) {
    return max(...args)
}
pipeline default(task) {
    log("${invoke(add3, [1, 2, 3])}|${largest([4, 9, 5])}")
}"#,
    );

    assert_eq!(output.trim_end(), "[harn] 6|9");
}

#[test]
fn nested_enum_does_not_make_outer_bare_pattern_ambiguous() {
    let (output, _) = run_harn(
        r"enum Outer {
    Shared(value)
}
fn nested_declaration() {
    enum Inner {
        Shared(value)
    }
    return Inner.Shared(2)
}
pipeline default(task) {
    match Outer.Shared(1) {
        Shared(value) -> { log(value) }
    }
}",
    );

    assert_eq!(output.trim_end(), "[harn] 1");
}

#[test]
fn duplicate_pipeline_enums_shadow_in_source_order() {
    let (output, _) = run_harn(
        r"pipeline default(task) {
    enum Event { First(value) }
    match Event.First(1) {
        First(value) -> { log(value) }
    }
    enum Event { Second(value) }
}",
    );

    assert_eq!(output.trim_end(), "[harn] 1");
}

#[test]
fn inherited_pipeline_enum_does_not_change_child_catalog() {
    let (output, _) = run_harn(
        r"pipeline base(task) {
    enum Event { Base(value) }
}
pipeline default(task) extends base {
    match Event.Child(1) {
        Child(value) -> { log(value) }
    }
    enum Event { Child(value) }
}",
    );

    assert_eq!(output.trim_end(), "[harn] 1");
}

#[test]
fn inherited_pipeline_enum_does_not_change_child_capture_analysis() {
    let (output, _) = run_harn(
        r"fn Candidate(value) { return value }
pipeline base(task) {
    enum Event { Candidate(value) }
}
pipeline default(task) extends base {
    let seed = 1
    match 1 {
        Candidate(seed) -> {
            const read = { -> seed }
            seed = 2
            log(read())
        }
    }
    enum Event { Other(value) }
}",
    );

    assert_eq!(output.trim_end(), "[harn] 2");
}
