//! Strict-types boundary checks (`json_parse`, `llm_call`) and cross-module call resolution.

use super::*;
use crate::diagnostic_codes::Code;
use crate::typechecker::DiagnosticDetails;

fn render_unresolved_call(source: &str) -> String {
    let diag = check_source_with_imports(source, &[])
        .into_iter()
        .find(|diag| diag.code == crate::diagnostic_codes::Code::UndefinedFunction)
        .expect("source should produce HARN-NAM-002");
    crate::diagnostic::set_color_override(Some(false));
    crate::diagnostic::render_type_diagnostic(source, "test.harn", &diag)
}

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
output: {
  schema: {type: "object", properties: {name: {type: "string"}}},
  validation: "error"
}
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
output: {schema: payload_schema, validation: "error"}
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
output: {schema: my_schema, validation: "error"}
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
fn test_cross_module_unresolved_call_render_suggests_transposed_builtin() {
    let rendered = render_unresolved_call(
        r#"pipeline t(task) {
  parse_json("{}")
}"#,
    );
    assert!(
        rendered.contains(
            "error[HARN-NAM-002]: call target `parse_json` is not defined or imported — did you mean `json_parse`?"
        ),
        "expected transposition suggestion, got:\n{rendered}"
    );
    assert!(
        rendered.contains("= help: did you mean `json_parse`?"),
        "expected rendered help suggestion, got:\n{rendered}"
    );
}

#[test]
fn test_cross_module_unresolved_call_render_suggests_plain_typo() {
    let rendered = render_unresolved_call(
        r#"pipeline t(task) {
  json_pars("{}")
}"#,
    );
    assert!(
        rendered.contains(
            "error[HARN-NAM-002]: call target `json_pars` is not defined or imported — did you mean `json_parse`?"
        ),
        "expected typo suggestion, got:\n{rendered}"
    );
    assert!(
        rendered.contains("= help: did you mean `json_parse`?"),
        "expected rendered help suggestion, got:\n{rendered}"
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
    let unresolved: Vec<&str> = diags
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::UndefinedVariable)
        .filter_map(|diagnostic| match diagnostic.details.as_ref() {
            Some(DiagnosticDetails::UnresolvedName { name }) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(unresolved, ["SOME_ALLOWLIST"]);
    assert!(diags.iter().all(|diagnostic| {
        diagnostic.code != Code::UndefinedVariable
            || matches!(
                diagnostic.details,
                Some(DiagnosticDetails::UnresolvedName { .. })
            )
    }));
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
fn test_cross_module_runtime_values_allow_explicit_harness() {
    let diags = check_source_with_imports(
        r#"import { parse, parser } from "std/cli/argparse"
pipeline t(harness: Harness, task) {
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
fn test_worker_spawn_literal_config_allows_host_extension_keys() {
    let errs = errors(
        r#"pipeline t(harness: Harness, task) {
  harness.agent.worker_spawn({
    task: "do it",
    node: {kind: "stage"},
    persmissions: {}
  })
}"#,
    );
    assert!(
        errs.is_empty(),
        "worker spawn config remains host-extensible and should not reject extension keys: {errs:?}"
    );
}

#[test]
fn test_worker_spawn_literal_config_allows_provider_extension_keys() {
    let errs = errors(
        r#"pipeline t(harness: Harness, task) {
  harness.agent.worker_spawn({task: "do it", provider: "mock", backgroun: true})
}"#,
    );
    assert!(
        errs.is_empty(),
        "worker spawn config remains host-extensible and should not reject extension keys: {errs:?}"
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
fn test_cross_module_hostlib_prefix_is_not_source_callable() {
    // Hostlib implementations are runtime details. Source reaches them only
    // through nominal Harness handles; the old ambient prefix is not an
    // escape hatch.
    let diags =
        check_source_with_imports(r"pipeline t(task) { hostlib_code_index_stats({}) }", &[]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.iter()
            .any(|message| message.contains("hostlib_code_index_stats")),
        "ambient hostlib calls must be rejected in favor of Harness handles: {errs:?}"
    );
}

/// The capability roots on `Harness` are a closed set. Before #6093 the
/// checker deferred every miss to the VM, so `harness.crypto.sha256(..)`
/// passed `harn check` and `harn lint` and only failed once a live release
/// executed that line for the first time.
#[test]
fn test_unknown_harness_capability_root_is_rejected() {
    let errs = strict_errors(
        r#"pipeline t(harness: Harness, task) {
  log(harness.crypto.sha256("hello"))
}"#,
    );
    assert!(
        errs.iter()
            .any(|message| message.contains("capability `crypto` does not exist on `Harness`")),
        "an unknown capability root must be a check-time error: {errs:?}"
    );
}

/// The valid roots must stay silent — the whole point of deferring before was
/// to never reject a valid program.
#[test]
fn test_known_harness_capability_roots_are_accepted() {
    for root in ["fs", "stdio", "env", "clock", "process", "llm"] {
        let source = format!(
            r#"pipeline t(harness: Harness, task) {{
  const cap = harness.{root}
  log(cap)
}}"#
        );
        let errs = strict_errors(&source);
        assert!(
            !errs
                .iter()
                .any(|message| message.contains("does not exist on `Harness`")),
            "`harness.{root}` must type-check: {errs:?}"
        );
    }
}

/// The VM answers `harness?.missing` with `nil` rather than raising, so the
/// checker must reject exactly what the runtime rejects and no more.
#[test]
fn test_optional_harness_capability_access_stays_gradual() {
    let errs = strict_errors(
        r#"pipeline t(harness: Harness, task) {
  log(harness?.crypto)
}"#,
    );
    assert!(
        !errs
            .iter()
            .any(|message| message.contains("does not exist on `Harness`")),
        "optional capability access must stay gradual: {errs:?}"
    );
}

/// A near-miss should name the intended root instead of only listing all of
/// them.
#[test]
fn test_unknown_harness_capability_root_suggests_closest() {
    let errs = strict_errors(
        r#"pipeline t(harness: Harness, task) {
  log(harness.clocks)
}"#,
    );
    assert!(
        errs.iter()
            .any(|message| message.contains("did you mean `clock`?")),
        "a near-miss root must suggest the real one: {errs:?}"
    );
}
