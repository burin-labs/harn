//! Strict-types boundary checks (`json_parse`, `llm_call`) and cross-module call resolution.

use super::*;

#[test]
fn test_strict_types_json_parse_property_access() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const data = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        errs.iter().any(|w| w.contains("unvalidated")),
        "expected unvalidated error, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_direct_chain_access() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  log(json_parse("{}").name)
}"#,
    );
    assert!(
        errs.iter().any(|w| w.contains("Direct property access")),
        "expected direct access error, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_schema_expect_clears() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const my_schema = {type: "object", properties: {name: {type: "string"}}}
  const data = json_parse("{}")
  schema_expect(data, my_schema)
  log(data.name)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "expected no unvalidated error after schema_expect, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_schema_is_if_guard() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const my_schema = {type: "object", properties: {name: {type: "string"}}}
  const data = json_parse("{}")
  if schema_is(data, my_schema) {
log(data.name)
  }
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "expected no unvalidated error inside schema_is guard, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_shape_annotation_clears() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const data: {name: string, age: int} = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "expected no error with shape annotation, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_propagation() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const data = json_parse("{}")
  const x = data
  log(x.name)
}"#,
    );
    assert!(
        errs.iter()
            .any(|w| w.contains("unvalidated") && w.contains("'x'")),
        "expected propagation error for x, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_non_boundary_no_error() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const x = len("hello")
  log(x)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "non-boundary function should not be flagged, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_subscript_access() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const data = json_parse("{}")
  log(data["name"])
}"#,
    );
    assert!(
        errs.iter().any(|w| w.contains("unvalidated")),
        "expected subscript error, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_disabled_by_default() {
    let diags = check_source(
        r#"pipeline t(task) {
  const data = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("unvalidated")),
        "strict types should be off by default, got: {diags:?}"
    );
}

#[test]
fn test_strict_types_llm_call_without_schema() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const result = llm_call("prompt", "system")
  log(result.text)
}"#,
    );
    assert!(
        errs.iter().any(|w| w.contains("unvalidated")),
        "llm_call without schema should error, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_llm_call_with_schema_clean() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const result = llm_call("prompt", "system", {
schema: {type: "object", properties: {name: {type: "string"}}}
  })
  log(result.data)
  log(result.text)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "llm_call with schema should not error, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_schema_expect_result_typed() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const my_schema = {type: "object", properties: {name: {type: "string"}}}
  const validated = schema_expect(json_parse("{}"), my_schema)
  log(validated.name)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "schema_expect result should be typed, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_realistic_orchestration() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const payload_schema = {type: "object", properties: {
name: {type: "string"},
steps: {type: "list", items: {type: "string"}}
  }}

  // Good: schema-aware llm_call
  const result = llm_call("generate a workflow", "system", {
schema: payload_schema
  })
  const workflow_name = result.data.name

  // Good: validate then access
  const raw = json_parse("{}")
  schema_expect(raw, payload_schema)
  const steps = raw.steps

  log(workflow_name)
  log(steps)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "validated orchestration should be clean, got: {errs:?}"
    );
}

#[test]
fn test_strict_types_llm_call_with_schema_via_variable() {
    let errs = strict_errors(
        r#"pipeline t(task) {
  const my_schema = {type: "object", properties: {score: {type: "float"}}}
  const result = llm_call("rate this", "system", {
schema: my_schema
  })
  log(result.data.score)
}"#,
    );
    assert!(
        !errs.iter().any(|w| w.contains("unvalidated")),
        "llm_call with schema variable should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_unresolved_call_errors() {
    let diags =
        check_source_with_imports(r"pipeline t(task) { missing_helper() }", &["other_helper"]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.iter().any(|m| m.contains("missing_helper")),
        "expected undefined-call error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_unresolved_value_identifier_errors() {
    let diags = check_source_with_imports(
        r"pipeline t(task) {
  const tools = {allow: SOME_ALLOWLIST}
  log(tools)
}",
        &["other_helper"],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.iter().any(|m| m.contains("SOME_ALLOWLIST")),
        "expected undefined value identifier error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_imported_value_identifier_is_allowed() {
    let diags = check_source_with_imports(
        r"pipeline t(task) {
  const tools = {allow: SOME_ALLOWLIST}
  log(tools)
}",
        &["SOME_ALLOWLIST"],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        !errs.iter().any(|m| m.contains("SOME_ALLOWLIST")),
        "imported value identifier should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_dict_keys_and_match_binders_are_not_value_reads() {
    let diags = check_source_with_imports(
        r#"pipeline t(task) {
  const payload = {allow: "yes"}
  match payload.allow {
    accepted -> { log(accepted) }
  }
}"#,
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.is_empty(),
        "syntactic names should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_schema_of_type_identifier_is_allowed() {
    let diags = check_source_with_imports(
        r"type Payload = {name: string}
pipeline t(task) {
  const schema = schema_of(Payload)
  log(schema)
}",
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.is_empty(),
        "schema_of type name should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_ambient_runtime_values_are_allowed() {
    let diags = check_source_with_imports(
        r#"import { parse, parser } from "std/cli/argparse"
pipeline t(task) {
  const args = parse(parser({name: "test", args: []}), argv)
  const exists = harness.fs.exists(".")
  log({args: args, exists: exists, pi: pi, git: git})
}"#,
        &["parse", "parser"],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.is_empty(),
        "ambient values should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_imported_call_is_allowed() {
    let diags = check_source_with_imports(r"pipeline t(task) { helper_fn(1, 2) }", &["helper_fn"]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        !errs.iter().any(|m| m.contains("helper_fn")),
        "imported call should not error, got: {errs:?}"
    );
}

#[test]
fn test_imported_name_shadows_same_named_builtin() {
    // `render` is a builtin (`template.render(path: string?, bindings: dict)`),
    // but here it is imported from a module (e.g. `std/disclosure`, which
    // exports a 3-arg `render`). The import must shadow the builtin so the
    // call is not checked against the builtin's signature — otherwise
    // precompile reports phantom arity/argument-type errors that `harn run`
    // never hits.
    let diags = check_source_with_imports(
        r#"pipeline t(task) { render({sub: "user:k"}, "github", {project: false}) }"#,
        &["render"],
    );
    let noise: Vec<&String> = diags
        .iter()
        .filter(|d| d.message.contains("render"))
        .map(|d| &d.message)
        .collect();
    assert!(
        noise.is_empty(),
        "imported `render` must shadow the builtin, got: {noise:?}"
    );
}

#[test]
fn test_renamed_stdlib_call_suggests_replacement() {
    let diags = check_source_with_imports(
        r"pipeline t(task) { retry_with_backoff(1, 0, fn() { return true }) }",
        &["retry_predicate_with_backoff"],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.iter()
            .any(|m| m.contains("did you mean `retry_predicate_with_backoff`")),
        "expected renamed stdlib suggestion, got: {errs:?}"
    );
}

#[test]
fn test_spawn_agent_literal_config_rejects_unknown_option_key() {
    let errs = errors(
        r#"pipeline t(task) {
  spawn_agent({
    task: "do it",
    node: {kind: "stage"},
    persmissions: {}
  })
}"#,
    );
    assert!(
        errs.iter().any(
            |m| m.contains("argument 1 `config`: unknown option `persmissions`")
                && m.contains("did you mean `permissions`")
        ),
        "expected spawn_agent option-key typo error, got: {errs:?}"
    );
}

#[test]
fn test_sub_agent_run_literal_options_rejects_unknown_option_key() {
    let errs = errors(
        r#"pipeline t(task) {
  sub_agent_run("do it", {provider: "mock", backgroun: true})
}"#,
    );
    assert!(
        errs.iter().any(
            |m| m.contains("argument 2 `options`: unknown option `backgroun`")
                && m.contains("did you mean `background`")
        ),
        "expected sub_agent_run option-key typo error, got: {errs:?}"
    );
}

#[test]
fn test_sub_agent_request_accepts_registry_tools_shape() {
    let errs = errors(
        r#"pipeline t(task) {
  sub_agent_request("do it", {provider: "mock", tools: tool_registry()})
}"#,
    );
    assert!(
        errs.is_empty(),
        "sub_agent_request should accept a tool registry dict in options.tools, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_local_fn_not_flagged() {
    let diags = check_source_with_imports(
        r"fn local_fn() { 42 }
pipeline t(task) { local_fn() }",
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(errs.is_empty(), "local fn should not error, got: {errs:?}");
}

#[test]
fn test_cross_module_forward_reference_is_allowed() {
    // A pipeline that calls a fn declared *later* in the same file
    // should not trigger the strict cross-module undefined-call
    // check, because top-level names are registered up-front.
    let diags = check_source_with_imports(
        r"pipeline t(task) { helper() }
fn helper() { 42 }",
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        !errs.iter().any(|m| m.contains("helper")),
        "forward-declared fn should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_builtin_not_flagged() {
    let diags = check_source_with_imports(r#"pipeline t(task) { log("hello") }"#, &[]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(errs.is_empty(), "builtin should not error, got: {errs:?}");
}

#[test]
fn test_cross_module_hostlib_prefix_not_flagged() {
    // `hostlib_*` names are registered onto the VM at runtime by
    // `harn_hostlib::install_default`. The parser's static
    // BUILTIN_SIGNATURES table does not (and should not) enumerate
    // them, so the cross-module resolver treats the prefix as an
    // opaque escape hatch — the same way `__`-prefixed names are
    // treated.
    let diags =
        check_source_with_imports(r"pipeline t(task) { hostlib_code_index_stats({}) }", &[]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.is_empty(),
        "hostlib_-prefixed call should not error, got: {errs:?}"
    );
}
