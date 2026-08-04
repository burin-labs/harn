use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

#[derive(Debug, PartialEq)]
struct ParamContract {
    name: String,
    type_expr: Option<TypeContract>,
}

#[derive(Debug, PartialEq)]
enum TypeContract {
    Named(String),
    Shape(BTreeMap<String, (bool, TypeContract)>),
}

fn type_contract(type_expr: &TypeExpr) -> TypeContract {
    match type_expr {
        TypeExpr::Named(name) => TypeContract::Named(name.clone()),
        TypeExpr::Shape(fields) => TypeContract::Shape(
            fields
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        (field.optional, type_contract(&field.type_expr)),
                    )
                })
                .collect(),
        ),
        unsupported => {
            panic!("unsupported parameter type in capability conformance test: {unsupported:?}")
        }
    }
}

fn param(name: &str, type_name: &str) -> ParamContract {
    ParamContract {
        name: name.to_string(),
        type_expr: Some(TypeContract::Named(type_name.to_string())),
    }
}

fn shape_param(name: &str, fields: &[(&str, &str)]) -> ParamContract {
    ParamContract {
        name: name.to_string(),
        type_expr: Some(TypeContract::Shape(
            fields
                .iter()
                .map(|(field, type_name)| {
                    (
                        (*field).to_string(),
                        (false, TypeContract::Named((*type_name).to_string())),
                    )
                })
                .collect(),
        )),
    }
}

fn callable_params(source: &str, callable: &str) -> Vec<ParamContract> {
    let program = harn_parser::parse_source(source).expect("migration output must parse");
    let mut found = None;
    visit::walk_program(&program, &mut |node| {
        let (name, params) = match &node.node {
            Node::FnDecl { name, params, .. }
            | Node::ToolDecl { name, params, .. }
            | Node::Pipeline { name, params, .. } => (name, params),
            _ => return,
        };
        if name == callable {
            found = Some(
                params
                    .iter()
                    .map(|param| ParamContract {
                        name: param.name.clone(),
                        type_expr: param.type_expr.as_ref().map(type_contract),
                    })
                    .collect(),
            );
        }
    });
    found.unwrap_or_else(|| panic!("callable `{callable}` not found in:\n{source}"))
}

fn dict_field_paths(node: &SNode) -> Option<BTreeMap<String, Option<String>>> {
    let Node::DictLiteral(entries) = &node.node else {
        return None;
    };
    entries
        .iter()
        .map(|entry| {
            let key = match &entry.key.node {
                Node::Identifier(key) | Node::StringLiteral(key) => key.clone(),
                _ => return None,
            };
            Some((key, expression_path(&entry.value)))
        })
        .collect()
}

fn expression_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) => Some(name.clone()),
        Node::PropertyAccess { object, property } => {
            Some(format!("{}.{}", expression_path(object)?, property))
        }
        _ => None,
    }
}

fn call_argument_paths(source: &str, callee: &str) -> Vec<Vec<Option<String>>> {
    let program = harn_parser::parse_source(source).expect("migration output must parse");
    let mut calls = Vec::new();
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == callee {
                calls.push(args.iter().map(expression_path).collect());
            }
        }
    });
    assert!(!calls.is_empty(), "call `{callee}` not found in:\n{source}");
    calls
}

fn call_arities(source: &str, callee: &str) -> Vec<usize> {
    let program = harn_parser::parse_source(source).expect("migration output must parse");
    let mut arities = Vec::new();
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == callee {
                arities.push(args.len());
            }
        }
    });
    assert!(
        !arities.is_empty(),
        "call `{callee}` not found in:\n{source}"
    );
    arities
}

fn call_dict_argument_paths(
    source: &str,
    callee: &str,
    argument_index: usize,
) -> Vec<BTreeMap<String, Option<String>>> {
    let program = harn_parser::parse_source(source).expect("migration output must parse");
    let mut dicts = Vec::new();
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == callee {
                let argument = args.get(argument_index).unwrap_or_else(|| {
                    panic!("call `{callee}` has no argument {argument_index} in:\n{source}")
                });
                dicts.push(dict_field_paths(argument).unwrap_or_else(|| {
                    panic!("argument {argument_index} to `{callee}` is not a dict in:\n{source}")
                }));
            }
        }
    });
    assert!(!dicts.is_empty(), "call `{callee}` not found in:\n{source}");
    dicts
}

fn method_receiver_paths(source: &str, method: &str) -> Vec<String> {
    let program = harn_parser::parse_source(source).expect("migration output must parse");
    let mut receivers = Vec::new();
    visit::walk_program(&program, &mut |node| {
        if let Node::MethodCall {
            object,
            method: candidate,
            ..
        } = &node.node
        {
            if candidate == method {
                receivers.push(expression_path(object).unwrap_or_else(|| {
                    panic!("receiver for `{method}` is not a property path in:\n{source}")
                }));
            }
        }
    });
    assert!(
        !receivers.is_empty(),
        "method `{method}` not found in:\n{source}"
    );
    receivers
}

fn apply_single(source: &str) -> (ApplyResult, String) {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, source).unwrap();
    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let updated = fs::read_to_string(&script).unwrap();
    (result, updated)
}

#[test]
fn capability_apply_converges_transitive_repairs_in_one_invocation() {
    let (result, updated) = apply_single(
        "fn needs_harness(harness: Harness, value: string) {\n  value\n}\n\nfn wrapper(value: string) {\n  needs_harness(value)\n}\n\npipeline run() {\n  wrapper(\"session\")\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "wrapper"),
        vec![param("harness", "Harness"), param("value", "string")]
    );
    assert_eq!(
        call_argument_paths(&updated, "needs_harness")[0],
        [Some("harness".to_string()), Some("value".to_string())]
    );
    assert_eq!(
        call_argument_paths(&updated, "wrapper")[0][0],
        Some("harness".to_string())
    );
    assert_eq!(
        callable_params(&updated, "run"),
        vec![param("harness", "Harness")]
    );
}

#[test]
fn capability_apply_preserves_multiline_declaration_and_call_whitespace() {
    let (result, updated) = apply_single(
        "fn load(\n  path: string,\n) -> string {\n  return read_file(path)\n}\n\nfn main(harness: Harness) {\n  load(\n    \"config.json\",\n  )\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "load"),
        vec![param("harness", "HarnessFs"), param("path", "string")]
    );
    assert_eq!(
        call_argument_paths(&updated, "load")[0][0],
        Some("harness.fs".to_string())
    );
    assert!(
        !updated.lines().any(|line| line.ends_with([' ', '\t'])),
        "migration introduced trailing whitespace:\n{updated}"
    );
}

#[test]
fn capability_apply_preserves_an_existing_handle_for_an_imported_bundle() {
    let temp = tempfile::TempDir::new().unwrap();
    let mode = temp.path().join("mode.harn");
    let entry = temp.path().join("main.harn");
    fs::write(
        &mode,
        "pub fn run_auto_mode(harness: {env: HarnessEnv, obs: HarnessObs}, setting: string = \"\") -> string {\n  harness.obs.llm_usage()\n  return harness.env.get_or(\"MODE\", setting)\n}\n",
    )
    .unwrap();
    fs::write(
        &entry,
        "import { run_auto_mode } from \"./mode\"\n\nfn invoke(setting: string, harness: HarnessObs) -> string {\n  return run_auto_mode(harness, setting)\n}\n\nfn main(harness: Harness) {\n  invoke(\"\", harness.obs)\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let updated = fs::read_to_string(&entry).unwrap();
    assert_eq!(
        callable_params(&updated, "invoke"),
        vec![
            param("setting", "string"),
            param("harness", "HarnessObs"),
            param("env", "HarnessEnv"),
        ]
    );
    assert_eq!(
        call_dict_argument_paths(&updated, "run_auto_mode", 0)[0],
        BTreeMap::from([
            ("env".to_string(), Some("env".to_string())),
            ("obs".to_string(), Some("harness".to_string())),
        ])
    );
    assert_eq!(
        call_argument_paths(&updated, "invoke")[0][1..],
        [
            Some("harness.obs".to_string()),
            Some("harness.env".to_string()),
        ]
    );
}

#[test]
fn capability_apply_projects_arguments_for_added_narrow_carriers() {
    let (result, updated) = apply_single(
        "pub fn read_mode(prefix: string, harness: HarnessEnv) -> string {\n  llm_usage()\n  return prefix + harness.get_or(\"MODE\", \"\")\n}\n\nfn invoke() -> string {\n  return read_mode(\"mode=\")\n}\n\nfn main(harness: Harness) {\n  invoke()\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "invoke"),
        vec![shape_param(
            "harness",
            &[("env", "HarnessEnv"), ("obs", "HarnessObs")],
        )]
    );
    assert_eq!(
        call_argument_paths(&updated, "read_mode")[0][1],
        Some("harness.env".to_string())
    );
    assert_eq!(
        call_argument_paths(&updated, "read_mode")[0][2],
        Some("harness.obs".to_string())
    );
}

#[test]
fn capability_apply_attenuates_root_to_a_typed_bundle_through_the_real_plan() {
    let (result, updated) = apply_single(
        "fn inspect(harness: {fs: HarnessFs, tools: HarnessTools}, path: string) {\n  return path\n}\n\nfn main(harness: Harness) {\n  inspect(harness, \"manifest.json\")\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        call_dict_argument_paths(&updated, "inspect", 0)[0],
        BTreeMap::from([
            ("fs".to_string(), Some("harness.fs".to_string())),
            ("tools".to_string(), Some("harness.tools".to_string())),
        ])
    );
}

#[test]
fn capability_apply_inserts_multiple_leading_capabilities_one_pass_at_a_time() {
    let (result, updated) = apply_single(
        "fn inspect(_fs: HarnessFs, _ast: HarnessAst, left: string, right: string) {\n  return left + right\n}\n\nfn main(harness: Harness) {\n  inspect(\"left\", \"right\")\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        call_argument_paths(&updated, "inspect")[0],
        [
            Some("harness.fs".to_string()),
            Some("harness.ast".to_string()),
            None,
            None,
        ]
    );
}

#[test]
fn capability_apply_threads_a_new_typed_argument_through_local_callers() {
    let (result, updated) = apply_single(
        "import { read_json_typed_result } from \"std/fs\"\nimport { schema_string } from \"std/schema\"\n\nfn decode(path: string) {\n  return read_json_typed_result(path, schema_string())\n}\n\nfn load() {\n  return decode(\"manifest.json\")\n}\n\nfn main(harness: Harness) {\n  load()\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "decode"),
        vec![param("harness", "HarnessFs"), param("path", "string")]
    );
    assert_eq!(
        call_argument_paths(&updated, "read_json_typed_result")[0][..2],
        [Some("harness".to_string()), Some("path".to_string())]
    );
    assert_eq!(
        callable_params(&updated, "load"),
        vec![param("harness", "HarnessFs")]
    );
    assert_eq!(
        call_argument_paths(&updated, "decode")[0][0],
        Some("harness".to_string())
    );
    assert_eq!(
        call_argument_paths(&updated, "load")[0][0],
        Some("harness.fs".to_string())
    );
}

#[test]
fn capability_apply_threads_ast_into_a_predicate_entrypoint() {
    let (result, updated) = apply_single(
        "import { ast_search } from \"std/ast\"\n\n@invariant\n@deterministic\n@archivist(evidence: [\"https://example.com/a\", \"https://example.org/b\"], confidence: 0.9, source_date: \"2026-08-01\")\npub fn inspect(slice, _ctx, _repo) {\n  return ast_search({source: slice, query: \"(_) @node\", language: \"zig\"})\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "inspect")[0],
        param("harness", "HarnessAst")
    );
    assert_eq!(
        call_argument_paths(&updated, "ast_search")[0][0],
        Some("harness".to_string())
    );
}

#[test]
fn capability_apply_does_not_classify_a_bare_invariant_pipeline_as_flow() {
    let (result, updated) = apply_single(
        "import { ast_search } from \"std/ast\"\n\n@invariant\npipeline inspect(slice, _ctx, _repo) {\n  ast_search({source: slice, query: \"(_) @node\", language: \"zig\"})\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "inspect")[0],
        param("harness", "Harness")
    );
    assert_eq!(
        call_argument_paths(&updated, "ast_search")[0][0],
        Some("harness.ast".to_string())
    );
}

#[test]
fn capability_apply_coalesces_multiple_requirements_into_one_carrier() {
    let (result, updated) = apply_single(
        "import { ast_search } from \"std/ast\"\nimport { read_json_typed_result } from \"std/fs\"\nimport { schema_string } from \"std/schema\"\n\nfn inspect(path: string, source: string) {\n  const loaded = read_json_typed_result(path, schema_string())\n  return {loaded: loaded, search: ast_search({source: source, query: \"(_) @node\", language: \"zig\"})}\n}\n\nfn main(harness: Harness) {\n  inspect(\"manifest.json\", \"const x = 1\")\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "inspect"),
        vec![
            shape_param("harness", &[("ast", "HarnessAst"), ("fs", "HarnessFs")]),
            param("path", "string"),
            param("source", "string"),
        ],
        "one carrier plus two domain arguments: {updated}"
    );
    assert_eq!(call_arities(&updated, "inspect"), vec![3]);
    assert_eq!(
        call_argument_paths(&updated, "read_json_typed_result")[0][0],
        Some("harness.fs".to_string())
    );
    assert_eq!(
        call_argument_paths(&updated, "ast_search")[0][0],
        Some("harness.ast".to_string())
    );
}

#[test]
fn capability_apply_keeps_three_capability_orchestration_on_root_harness() {
    let (result, updated) = apply_single(
        "fn orchestrate() {\n  const dir = cwd()\n  const timestamp = now_ms()\n  const usage = llm_usage()\n  return {dir: dir, timestamp: timestamp, usage: usage}\n}\n\nfn main(harness: Harness) {\n  orchestrate()\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "orchestrate"),
        vec![param("harness", "Harness")]
    );
    assert_eq!(
        call_argument_paths(&updated, "orchestrate")[0],
        [Some("harness".to_string())],
        "three capabilities are orchestration and must pass one root carrier"
    );
    assert_eq!(method_receiver_paths(&updated, "cwd"), vec!["harness.fs"]);
    assert_eq!(
        method_receiver_paths(&updated, "now_ms"),
        vec!["harness.clock"]
    );
    assert_eq!(
        method_receiver_paths(&updated, "llm_usage"),
        vec!["harness.obs"]
    );
}

#[test]
fn capability_apply_promotes_a_narrow_carrier_to_root_for_orchestration() {
    let (result, updated) = apply_single(
        "fn orchestrate(harness: HarnessFs) {\n  const dir = harness.cwd()\n  const timestamp = now_ms()\n  const usage = llm_usage()\n  return {dir: dir, timestamp: timestamp, usage: usage}\n}\n\nfn main(harness: Harness) {\n  orchestrate(harness.fs)\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "orchestrate"),
        vec![param("harness", "Harness")]
    );
    assert_eq!(method_receiver_paths(&updated, "cwd"), vec!["harness.fs"]);
    assert_eq!(
        call_argument_paths(&updated, "orchestrate")[0],
        [Some("harness".to_string())]
    );
}

#[test]
fn capability_apply_propagates_root_selection_through_a_split_caller() {
    let (result, updated) = apply_single(
        "fn orchestrate() {\n  const dir = cwd()\n  const timestamp = now_ms()\n  const usage = llm_usage()\n  return {dir: dir, timestamp: timestamp, usage: usage}\n}\n\nfn bridge(fs: HarnessFs, clock: HarnessClock) {\n  clock.now_ms()\n  return orchestrate()\n}\n\nfn main(harness: Harness) {\n  bridge(harness.fs, harness.clock)\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "bridge"),
        vec![param("fs", "Harness"), param("clock", "HarnessClock")],
        "the root-selected callee must elevate the split caller without deleting its other parameters"
    );
    assert_eq!(
        call_argument_paths(&updated, "bridge")[0],
        [
            Some("harness".to_string()),
            Some("harness.clock".to_string())
        ]
    );
    assert_eq!(
        call_argument_paths(&updated, "orchestrate")[0],
        [Some("fs".to_string())],
        "the elevated carrier must supply root authority to the callee"
    );
}

#[test]
fn capability_apply_edge_errors_identify_the_exact_call() {
    let error = whole_program_capabilities::call_edge_error(
        std::path::Path::new("pipelines/lib/adapter.harn"),
        "load_workspace",
        "orchestrate",
        harn_lexer::Span::with_offsets(120, 147, 8, 5),
        "a narrow caller cannot supply root Harness",
    );
    assert_eq!(
        error,
        "cannot migrate call `load_workspace` -> `orchestrate` at pipelines/lib/adapter.harn:8:5 (bytes 120..147): a narrow caller cannot supply root Harness"
    );
}

#[test]
fn capability_only_plan_excludes_unrelated_preflight_repairs_at_fixed_point() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { absent } from \"./missing\"\n\nfn main(harness: Harness) -> string {\n  return cwd()\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!result.applied.is_empty(), "{result:#?}");

    let second_plan = build_plan_with_options(
        &script,
        Some(RepairSafety::SurfaceChanging),
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(
        second_plan.repairs.is_empty(),
        "an unrelated unresolved-import preflight must not make a completed capability migration look unfinished: {second_plan:#?}"
    );
}

#[test]
fn capability_apply_keeps_the_burin_peer_coordination_fixture_parse_safe() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("test_peer_coordination.harn");
    fs::write(
        &script,
        include_str!("../../../tests/fixtures/capability_migration/peer_coordination_before.harn"),
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let updated = fs::read_to_string(&script).unwrap();
    harn_parser::parse_source(&updated)
        .unwrap_or_else(|errors| panic!("migration output must parse: {errors:?}\n{updated}"));
    assert_eq!(result.skipped_files.len(), 0, "{result:#?}");
    assert!(!updated.contains("strharness"), "{updated}");
    assert!(!updated.contains("foharness"), "{updated}");
    assert!(!updated.contains("asserharness"), "{updated}");
    assert!(updated.contains("harness.store_set("), "{updated}");
    assert!(updated.contains("harness.testing.calls()"), "{updated}");
    assert!(updated.contains("with_capability_fixtures("), "{updated}");
    assert!(updated.contains("harness.testing,"), "{updated}");
    assert!(updated.contains("method: \"peer_presence\""), "{updated}");
    assert!(updated.contains("operation: \"unrelated\""), "{updated}");
    assert!(!updated.contains("with_host_mocks"), "{updated}");
    assert!(
        !updated.contains("operation: \"peer_presence\""),
        "{updated}"
    );
}

#[test]
fn capability_apply_recognizes_a_local_named_capability_bundle() {
    let (result, updated) = apply_single(
        "type ScenarioCapabilities = {testing: HarnessTesting, llm: HarnessLlm}\n\nfn with_host_fixture(testing: HarnessTesting, body) {\n  testing.calls()\n  return body()\n}\n\nfn with_scenario(capabilities: ScenarioCapabilities, body) {\n  return with_host_fixture(capabilities.testing, body)\n}\n\nfn main(harness: Harness) {\n  with_scenario({testing: harness.testing, llm: harness.llm}, { -> nil })\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "with_scenario"),
        vec![
            param("capabilities", "ScenarioCapabilities"),
            ParamContract {
                name: "body".to_string(),
                type_expr: None,
            },
        ],
        "the alias already supplies both capabilities and needs no duplicate parameter"
    );
    assert_eq!(call_arities(&updated, "with_scenario"), vec![2]);
}

#[test]
fn capability_apply_recognizes_an_imported_named_capability_bundle() {
    let temp = tempfile::TempDir::new().unwrap();
    let types = temp.path().join("types.harn");
    let adapter = temp.path().join("adapter.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &types,
        "pub type ScenarioCapabilities = {testing: HarnessTesting, llm: HarnessLlm}\n",
    )
    .unwrap();
    fs::write(
        &adapter,
        "import { ScenarioCapabilities } from \"./types\"\n\nfn with_host_fixture(testing: HarnessTesting, body) {\n  testing.calls()\n  return body()\n}\n\npub fn with_scenario(capabilities: ScenarioCapabilities, body) {\n  return with_host_fixture(capabilities.testing, body)\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { with_scenario } from \"./adapter\"\n\nfn main(harness: Harness) {\n  with_scenario({testing: harness.testing, llm: harness.llm}, { -> nil })\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let migrated_adapter = fs::read_to_string(adapter).unwrap();
    let migrated_entrypoint = fs::read_to_string(entrypoint).unwrap();
    assert!(
        result.applied.is_empty(),
        "the imported alias already supplies the propagated capability:\n{result:#?}\n{migrated_adapter}\n{migrated_entrypoint}"
    );
    assert_eq!(
        callable_params(&migrated_adapter, "with_scenario")[0],
        param("capabilities", "ScenarioCapabilities")
    );
    assert_eq!(call_arities(&migrated_entrypoint, "with_scenario"), vec![2]);
}

#[test]
fn capability_apply_keeps_exported_definition_and_imported_call_arity_equal() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "import { ast_search } from \"std/ast\"\nimport { read_json_typed_result } from \"std/fs\"\nimport { schema_string } from \"std/schema\"\n\npub fn inspect(path: string, source: string) {\n  const loaded = read_json_typed_result(path, schema_string())\n  return {loaded: loaded, search: ast_search({source: source, query: \"(_) @node\", language: \"zig\"})}\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { inspect } from \"./library\"\n\nfn main(harness: Harness) {\n  inspect(\"manifest.json\", \"const x = 1\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let migrated_library = fs::read_to_string(library).unwrap();
    let migrated_entrypoint = fs::read_to_string(entrypoint).unwrap();
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{migrated_library}\n{migrated_entrypoint}"
    );
    let definition = callable_params(&migrated_library, "inspect");
    assert_eq!(
        definition,
        vec![
            shape_param("harness", &[("ast", "HarnessAst"), ("fs", "HarnessFs")]),
            param("path", "string"),
            param("source", "string"),
        ],
        "one carrier plus two domain arguments"
    );
    assert_eq!(
        call_arities(&migrated_entrypoint, "inspect"),
        vec![definition.len()]
    );
}

#[test]
fn capability_apply_widens_cross_module_carrier_without_duplicate_arguments() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "import { web_fetch } from \"std/web\"\n\npub fn run_surface(base_url: string, model: string) {\n  const _ = web_fetch(base_url, {})\n  harness.stdio.println(model)\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { run_surface } from \"./library\"\n\nfn main(_harness: Harness) {\n  run_surface(\"http://localhost\", \"model\")\n  harness.stdio.println(\"\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let migrated_library = fs::read_to_string(library).unwrap();
    let migrated_entrypoint = fs::read_to_string(entrypoint).unwrap();
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{migrated_library}\n{migrated_entrypoint}"
    );
    assert_eq!(
        callable_params(&migrated_library, "run_surface"),
        vec![
            param("harness", "Harness"),
            param("base_url", "string"),
            param("model", "string"),
        ]
    );
    assert_eq!(
        call_argument_paths(&migrated_entrypoint, "run_surface")[0],
        [Some("_harness".to_string()), None, None]
    );
    assert!(!migrated_entrypoint
        .lines()
        .any(|line| line.trim_start().starts_with("harness.stdio")));
}

#[test]
fn capability_apply_repairs_imported_capability_helpers_inside_closures() {
    let (result, updated) = apply_single(
        "import { agent_reminder_providers_fire } from \"std/agent/state\"\nimport { llm_call_count, with_mocks } from \"std/testing\"\n\npipeline main(harness: Harness, task) {\n  const reports = [\"session\"].map({ session ->\n    return agent_reminder_providers_fire(session, \"session_idle\", {}, {})\n  })\n  return {reports: reports, llm_calls: llm_call_count()}\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        call_argument_paths(&updated, "agent_reminder_providers_fire")[0],
        [
            Some("harness.agent".to_string()),
            Some("session".to_string()),
            None,
            None,
            None,
        ]
    );
    assert_eq!(
        call_argument_paths(&updated, "llm_call_count")[0],
        [Some("harness.llm".to_string())]
    );
    assert!(!updated.contains("with_mocks"));
}

/// A capability reached only through `${...}` is still reached.
///
/// `visit::walk_program` stops at `Node::InterpolatedString`: the lexer keeps a
/// hole as unparsed source text, so it has no AST children. The whole-program
/// solver computed required capabilities with that walk, so a handle used only
/// inside a hole looked unused. The repair then deleted it from the parameter
/// type and every call site while the interpolation kept calling it. `harn fix
/// --apply` runs unattended in the fleet bump workflow, so it rewrote working
/// helpers into code that does not compile and shipped that as a bump PR
/// (harn-cloud#1469).
///
/// The fixture needs THREE capabilities. With two, no attenuation repair is
/// proposed at all and the test passes vacuously — which is exactly how an
/// earlier version of it passed against the unfixed solver.
///
/// The assertion is EQUIVALENCE, not merely "did not delete": the same use
/// inside and outside a hole must plan the same repair. "Did not delete" alone
/// would also hold if the analysis simply gave up on any file containing a hole.
#[test]
fn capability_plan_counts_uses_inside_string_interpolation() {
    let plan_edits = |name: &str, body: &str| {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join(name);
        fs::write(
            &script,
            format!(
                "fn with_temp_dir(harness: {{fs: HarnessFs, process: HarnessProcess, random: HarnessRandom}}, body) {{\n\
                 {body}\
                 \x20 harness.fs.mkdir(dir)\n\
                 \x20 const outcome = try {{\n\
                 \x20   body(dir)\n\
                 \x20 }}\n\
                 \x20 harness.process.run({{program: \"rm\", args: [\"-rf\", dir]}})\n\
                 \x20 return unwrap(outcome)\n\
                 }}\n\n\
                 pipeline main(harness: Harness, task) {{\n\
                 \x20 return with_temp_dir(\n\
                 \x20   {{fs: harness.fs, process: harness.process, random: harness.random}},\n\
                 \x20   {{ dir -> dir }},\n\
                 \x20 )\n\
                 }}\n"
            ),
        )
        .unwrap();
        // `--apply` is a different entry from `build_plan`; only this one
        // reaches the repair that broke. Apply for real and read the file back,
        // so the assertion is on emitted code rather than on a plan.
        super::apply_repairs_with_options(
            temp.path(),
            RepairSafety::SurfaceChanging,
            false,
            super::FixOptions {
                capability_migrations_only: true,
            },
        )
        .unwrap();
        fs::read_to_string(&script).unwrap()
    };

    // Identical programs; `random` is reached through a hole in one and through
    // a plain statement in the other.
    let through_hole = plan_edits(
        "hole.harn",
        "  const dir = \".tmp-${harness.random.uuid_v7()}\"\n",
    );
    let through_statement = plan_edits(
        "statement.harn",
        "  const id = harness.random.uuid_v7()\n  const dir = \".tmp-${id}\"\n",
    );

    let signature = |source: &str| {
        source
            .lines()
            .find(|line| line.starts_with("fn with_temp_dir"))
            .unwrap_or_default()
            .to_string()
    };

    assert!(
        !through_hole.contains("harness.random")
            || signature(&through_hole).contains("HarnessRandom")
            || signature(&through_hole).contains("harness: Harness"),
        "the body still calls `harness.random`, so the repair must not have removed it \
         from the signature:\n{through_hole}"
    );
    assert_eq!(
        signature(&through_hole),
        signature(&through_statement),
        "a capability reached through `${{...}}` must be repaired the same as one \
         reached through a statement"
    );
}

#[test]
fn capability_plan_repairs_imported_helpers_without_type_diagnostics() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_capture_events } from \"std/agent/events\"\nimport { agent_parse_tool_calls } from \"std/agent/primitives\"\nimport { agent_session_finalize, agent_session_messages, agent_reminder_providers_fire } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const session = \"session\"\n  const messages = agent_session_messages(session)\n  agent_session_finalize(session, \"done\")\n  agent_session_finalize(custom_agent(harness), session, \"already explicit\")\n  const captured = agent_capture_events(session, fn() { nil })\n  const parsed = agent_parse_tool_calls(\"<tool_call>x({})</tool_call>\", [], \"text\")\n  const report = agent_reminder_providers_fire(session, \"session_idle\", {}, {})\n  return {messages: messages, captured: captured, parsed: parsed, report: report}\n}\n",
    )
    .unwrap();
    let files = vec![script.clone()];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert_eq!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .filter(|edit| {
                edit.span.start == edit.span.end && edit.replacement == "harness.agent, "
            })
            .count(),
        5,
        "every imported Agent helper must derive its prefix from the module signature: {repairs:#?}"
    );

    let mut updated = fs::read_to_string(&script).unwrap();
    let mut edits = repairs
        .iter()
        .flat_map(|repair| repair.edits.iter().cloned())
        .collect::<Vec<_>>();
    edits.sort_by_key(|edit| std::cmp::Reverse((edit.span.start, edit.span.end)));
    for edit in edits {
        updated.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    fs::write(&script, updated).unwrap();
    let repaired_graph = commands::check::build_module_graph(&files);
    let fixed_point =
        whole_program_capabilities::plan(&files, &repaired_graph, &[], &BTreeSet::new()).unwrap();
    assert!(
        fixed_point.is_empty(),
        "already-migrated imported calls must be a planner fixed point: {fixed_point:#?}"
    );
}

#[test]
fn capability_plan_preserves_explicit_imported_capability_expressions() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { terminal_width } from \"std/tui\"\n\nfn custom_term(term: HarnessTerm) -> HarnessTerm {\n  return term\n}\n\npipeline main(harness: Harness, task) {\n  const term = custom_term(harness.term)\n  return [terminal_width(custom_term(harness.term)), terminal_width(term)]\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert!(
        repairs.is_empty(),
        "explicit capability expressions must not be shifted into optional ordinary parameters: {repairs:#?}"
    );
}

#[test]
fn capability_plan_preserves_an_unknown_capability_identifier_when_ordinary_args_are_missing() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_session_messages } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const agent = custom_agent(harness)\n  return agent_session_messages(agent)\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert!(
        !repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .any(|edit| edit.replacement == "harness.agent, "),
        "an unknown identifier may already be the carrier; adding one would shift it into the missing session slot: {repairs:#?}"
    );
}

#[test]
fn capability_plan_uses_inferred_capability_types_to_repair_a_different_missing_carrier() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "pub fn run_with_root(harness: Harness, agent: HarnessAgent) {\n  return {root: harness, agent: agent}\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { run_with_root } from \"./library\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const agent = custom_agent(harness)\n  return run_with_root(agent)\n}\n",
    )
    .unwrap();
    let files = vec![entrypoint, library];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .any(|edit| edit.replacement == "harness, "),
        "an inferred Agent cannot occupy a root Harness slot, so the missing root carrier is observable: {repairs:#?}"
    );
}

#[test]
fn capability_plan_disambiguates_shadowed_inferred_bindings_by_declaration() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_session_messages } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const value = custom_agent(harness)\n  const before = agent_session_messages(value, \"outer-before\")\n  const nested = [1].map({ _ ->\n    const value = \"nested-session\"\n    return agent_session_messages(value)\n  })\n  const after = agent_session_messages(value, \"outer-after\")\n  return {before: before, nested: nested, after: after}\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert_eq!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .filter(|edit| edit.replacement == "harness.agent, ")
            .count(),
        1,
        "only the string-valued shadow should receive the missing Agent carrier: {repairs:#?}"
    );
}

#[test]
fn capability_apply_repairs_session_ids_from_agent_session_producers() {
    let (result, updated) = apply_single(
        "import { agent_capture_events } from \"std/agent/events\"\nimport { agent_session_finalize, agent_session_init, agent_session_messages } from \"std/agent/state\"\n\npipeline main(task) {\n  const session = agent_session_open(\"session-id\")\n  const before = agent_session_messages(session)\n  const captured = agent_capture_events(session, fn() {\n    return agent_session_messages(session)\n  })\n  agent_session_finalize(session, {})\n  const control = agent_session_init(\"task\", nil, {})\n  const initialized_session = control?.session_id ?? \"\"\n  agent_session_finalize(initialized_session, {})\n  return {before: before, captured: captured}\n}\n",
    );

    assert_eq!(
        call_argument_paths(&updated, "agent_session_messages"),
        vec![
            vec![Some("harness.agent".into()), Some("session".into())],
            vec![Some("harness.agent".into()), Some("session".into())],
        ],
        "session IDs returned by the legacy open call are ordinary arguments: {result:#?}\n{updated}"
    );
    assert_eq!(
        call_argument_paths(&updated, "agent_session_finalize")
            .into_iter()
            .map(|arguments| arguments[..2].to_vec())
            .collect::<Vec<_>>(),
        vec![
            vec![Some("harness.agent".into()), Some("session".into())],
            vec![
                Some("harness.agent".into()),
                Some("initialized_session".into()),
            ],
        ],
        "finalize must receive its Agent carrier for open and init session IDs: {result:#?}\n{updated}"
    );
    assert_eq!(
        call_argument_paths(&updated, "agent_capture_events")[0][..2],
        [Some("harness.agent".into()), Some("session".into())],
        "event capture must receive its Agent carrier: {result:#?}\n{updated}"
    );
}

#[test]
fn capability_plan_completes_a_partial_imported_capability_prefix() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { which } from \"std/os\"\n\npipeline main(harness: Harness, task) {\n  return which(harness.tools, \"git\")\n}\n",
    )
    .unwrap();
    let files = vec![script.clone()];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();
    let edits = repairs
        .iter()
        .flat_map(|repair| &repair.edits)
        .filter(|edit| edit.replacement == ", harness.system")
        .collect::<Vec<_>>();
    assert_eq!(
        edits.len(),
        1,
        "only the missing capability suffix should be inserted: {repairs:#?}"
    );
    assert_eq!(
        &fs::read_to_string(&script).unwrap()[edits[0].span.start..edits[0].span.end],
        ""
    );

    let mut updated = fs::read_to_string(&script).unwrap();
    let mut all_edits = repairs
        .iter()
        .flat_map(|repair| repair.edits.iter().cloned())
        .collect::<Vec<_>>();
    all_edits.sort_by_key(|edit| std::cmp::Reverse((edit.span.start, edit.span.end)));
    for edit in all_edits {
        updated.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    assert_eq!(
        call_argument_paths(&updated, "which")[0],
        [
            Some("harness.tools".to_string()),
            Some("harness.system".to_string()),
            None,
        ]
    );
    fs::write(&script, updated).unwrap();
    let repaired_graph = commands::check::build_module_graph(&files);
    assert!(
        whole_program_capabilities::plan(&files, &repaired_graph, &[], &BTreeSet::new())
            .unwrap()
            .is_empty(),
        "a completed imported prefix must be a planner fixed point"
    );
}

#[test]
fn capability_plan_resolves_private_imported_capability_aliases() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "type AgentHandle = HarnessAgent\n\npub fn imported_helper(agent: AgentHandle, session: string) {\n  return agent.snapshot(session)\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { imported_helper } from \"./library\"\n\npipeline main(harness: Harness, task) {\n  return imported_helper(\"session\")\n}\n",
    )
    .unwrap();
    let files = vec![entrypoint, library];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(&files, &graph, &[], &BTreeSet::new()).unwrap();

    assert!(
        repairs.iter().flat_map(|repair| &repair.edits).any(|edit| {
            edit.span.start == edit.span.end && edit.replacement == "harness.agent, "
        }),
        "a private signature alias must resolve through the module graph: {repairs:#?}"
    );
}

#[test]
fn capability_apply_projects_retired_host_call_count_through_testing() {
    let (result, updated) = apply_single(
        "import { host_call_count, with_temp_dir } from \"std/testing\"\n\npipeline test_main(harness: Harness, task) {\n  assert(host_call_count() == 0)\n  return with_temp_dir(harness.fs, { dir -> dir })\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert!(updated.contains("import { with_temp_dir } from \"std/testing\""));
    assert!(updated.contains("len(harness.testing.calls()) == 0"));
    assert!(!updated.contains("host_call_count"));
}

#[test]
fn capability_apply_formats_every_edited_file() {
    let (result, updated) = apply_single(
        "fn helper(value: string) {\n  return read_file(value)\n}\nfn main(harness: Harness) { helper(\"x\") }\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(harn_fmt::format_source(&updated).unwrap(), updated);
}

#[test]
fn capability_apply_formats_with_the_owning_project_config() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("harn.toml"), "[fmt]\nline_width = 40\n").unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "fn helper(path: string, fallback: string, prefix: string) { return read_file(path) ?? fallback + prefix }\nfn main(harness: Harness) { helper(\"a\", \"b\", \"c\") }\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let updated = fs::read_to_string(&script).unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let options = harn_fmt::FmtOptions {
        line_width: 40,
        ..harn_fmt::FmtOptions::default()
    };
    assert_eq!(
        harn_fmt::format_source_opts(&updated, &options).unwrap(),
        updated
    );
    assert_ne!(harn_fmt::format_source(&updated).unwrap(), updated);
}

#[test]
fn capability_apply_formats_with_defaults_when_project_config_is_invalid() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("harn.toml"), "[fmt\nline_width = nope\n").unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "fn helper(path: string, fallback: string, prefix: string) { return read_file(path) ?? fallback + prefix }\nfn main(harness: Harness) { helper(\"a\", \"b\", \"c\") }\n",
    )
    .unwrap();

    apply::format_edited_files(&BTreeSet::from([script.display().to_string()])).unwrap();
    let updated = fs::read_to_string(&script).unwrap();
    assert_eq!(harn_fmt::format_source(&updated).unwrap(), updated);
}

#[test]
fn capability_apply_rejects_non_ast_authority_at_flow_predicate_boundary() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("predicate.harn");
    fs::write(
        &script,
        "import { ast_search } from \"std/ast\"\nimport { read_json_typed_result } from \"std/fs\"\nimport { schema_string } from \"std/schema\"\n\n@invariant\n@deterministic\npub fn inspect(slice, _ctx, _repo) {\n  const loaded = read_json_typed_result(\"manifest.json\", schema_string())\n  return {loaded: loaded, ast: ast_search({source: slice, query: \"(_) @node\", language: \"zig\"})}\n}\n",
    )
    .unwrap();

    let error = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .expect_err("flow evaluation must not silently broaden predicate authority");
    assert_eq!(
        error,
        "flow predicate `inspect` requires unsupported injected capabilities: fs; flow evaluation injects only HarnessAst"
    );
}

#[test]
fn capability_apply_does_not_apply_flow_authority_rules_to_handler_invariants() {
    let (result, updated) = apply_single(
        "@invariant(\"capability.policy\", allow: \"fs.read\")\nfn inspect() -> string {\n  return read_file(\"manifest.json\")\n}\n\nfn main(harness: Harness) {\n  inspect()\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "inspect"),
        vec![param("harness", "HarnessFs")]
    );
    assert_eq!(
        call_argument_paths(&updated, "inspect")[0][0],
        Some("harness.fs".to_string())
    );
}

#[test]
fn capability_apply_absorbs_an_implicit_root_receiver_in_the_first_program_plan() {
    let (result, updated) = apply_single(
        "pub fn write_result(text: string) -> nil {\n  const input = pipeline_input() ?? {}\n  if input?.emit ?? false {\n    harness.stdio.print(text)\n  }\n}\n\nfn main(harness: Harness) {\n  write_result(\"hello\")\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(result.applied.len(), 1, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "write_result"),
        vec![
            shape_param(
                "harness",
                &[("runtime", "HarnessRuntime"), ("stdio", "HarnessStdio")],
            ),
            param("text", "string"),
        ]
    );
    assert_eq!(
        method_receiver_paths(&updated, "pipeline_input"),
        vec!["harness.runtime"]
    );
    assert_eq!(
        method_receiver_paths(&updated, "print"),
        vec!["harness.stdio"]
    );
    assert_eq!(
        call_dict_argument_paths(&updated, "write_result", 0)[0],
        BTreeMap::from([
            ("runtime".to_string(), Some("harness.runtime".to_string())),
            ("stdio".to_string(), Some("harness.stdio".to_string())),
        ])
    );
}

#[test]
fn capability_apply_preserves_root_values_that_escape() {
    let (result, updated) = apply_single(
        "fn consume(harness: Harness) {}\n\nfn keep_root(harness: Harness) {\n  consume(harness)\n}\n\nfn narrow(harness: Harness) -> string {\n  return harness.fs.cwd()\n}\n\nfn main(harness: Harness) {\n  keep_root(harness)\n  narrow(harness)\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "keep_root"),
        vec![param("harness", "Harness")]
    );
    assert_eq!(
        call_argument_paths(&updated, "consume")[0][0],
        Some("harness".to_string())
    );
    assert_eq!(
        callable_params(&updated, "narrow"),
        vec![param("harness", "HarnessFs")]
    );
    assert_eq!(
        call_argument_paths(&updated, "narrow")[0][0],
        Some("harness.fs".to_string())
    );
}

#[test]
fn capability_apply_projects_accesses_to_added_split_capabilities() {
    let (result, updated) = apply_single(
        "import { with_temp_dir } from \"std/testing\"\n\nfn with_probe(harness: HarnessFs, body) {\n  return with_temp_dir(harness, { dir ->\n    harness.write_text(path_join(dir, \"probe.txt\"), \"ok\")\n    harness.testing.calls()\n    return body(dir)\n  })\n}\n\nfn main(harness: Harness) {\n  with_probe(harness.fs, { _ -> nil })\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "with_probe"),
        vec![
            param("harness", "HarnessFs"),
            param("testing", "HarnessTesting"),
            ParamContract {
                name: "body".to_string(),
                type_expr: None,
            },
        ]
    );
    assert_eq!(
        call_argument_paths(&updated, "with_temp_dir")[0],
        [Some("harness".to_string()), None],
    );
    assert_eq!(
        method_receiver_paths(&updated, "write_text"),
        vec!["harness"]
    );
    assert_eq!(
        method_receiver_paths(&updated, "calls"),
        vec!["testing"],
        "an added split capability must replace its stale root access"
    );
}

#[test]
fn capability_apply_replaces_an_existing_imported_carrier_in_place() {
    let (result, updated) = apply_single(
        "import { with_temp_dir } from \"std/testing\"\n\nfn with_probe(body) {\n  return with_temp_dir(harness, { dir ->\n    harness.testing.calls()\n    return body(dir)\n  })\n}\n\nfn main(harness: Harness) {\n  with_probe({ _ -> nil })\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert_eq!(
        callable_params(&updated, "with_probe"),
        vec![
            param("harness", "HarnessTesting"),
            param("fs", "HarnessFs"),
            ParamContract {
                name: "body".to_string(),
                type_expr: None,
            },
        ]
    );
    assert_eq!(
        call_argument_paths(&updated, "with_temp_dir")[0],
        [Some("fs".to_string()), None],
        "the existing carrier must be projected in place, not prepended as a third argument:\n{updated}"
    );
}

/// `with_mocks` and `with_scenario` do not share a config vocabulary, so the
/// callee rename is only complete when the keys travel with it. Over a config
/// this pass cannot see into, the rename alone leaves `with_scenario` reading
/// `capabilities`/`llm` off a dict that still says `host_mocks`/`llm_mocks`:
/// both scopes install empty and the body runs against the real host, while the
/// call site is gone and the plan converges.
#[test]
fn capability_apply_leaves_an_unreadable_retired_mock_config_for_a_human() {
    for source in [
        // A forwarded config has no keys to rewrite.
        "import { with_mocks } from \"std/testing\"\n\npub fn with_checked(config, body) {\n  return with_mocks(config, body)\n}\n",
        // A helper call that builds the config is equally opaque.
        "import { with_mocks } from \"std/testing\"\n\nfn turn_mocks() -> dict {\n  return {host_mocks: []}\n}\n\npipeline test_live(task) {\n  return with_mocks(turn_mocks(), { _ -> \"ok\" })\n}\n",
        // A key outside the two-scope contract is not part of the rename.
        "import { with_mocks } from \"std/testing\"\n\npipeline test_extra(task) {\n  return with_mocks({host_mocks: [], unsupported: 1}, { _ -> \"ok\" })\n}\n",
    ] {
        let (_, updated) = apply_single(source);
        assert!(
            updated.contains("with_mocks("),
            "an unreadable config must stay inert: {updated}"
        );
        assert!(
            !updated.contains("with_scenario("),
            "renaming the callee without the keys silently installs no fixtures: {updated}"
        );
    }
}

/// An imported callee with a multi-capability prefix has two would-be edit
/// producers: the imported-signature pass, which reads the whole prefix, and
/// the per-diagnostic pass, which sees only the capability the typechecker
/// reported first. Both insert at the same argument index, so emitting both
/// left two conflicting zero-width edits at one offset and the apply refused
/// the entire candidate — writing nothing for the whole pass.
#[test]
fn capability_apply_does_not_double_insert_a_multi_capability_imported_prefix() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("trailers.harn"),
        "pub fn append_trailers(env: HarnessEnv, fs: HarnessFs, message: string) -> string {\n  const _ = env.get(\"USER\")\n  const _ = fs.read_text(message)\n  return message\n}\n",
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        "import { append_trailers } from \"./trailers\"\n\npipeline render_trailers(task) {\n  return append_trailers(\"note.txt\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let updated = fs::read_to_string(entry).unwrap();
    assert_eq!(
        call_argument_paths(&updated, "append_trailers")[0],
        [
            Some("harness.env".to_string()),
            Some("harness.fs".to_string()),
            None
        ],
        "the whole declared prefix must be inserted exactly once: {updated}"
    );
}

#[path = "capability_apply_tests/prefix_invariant.rs"]
mod prefix_invariant;
#[path = "capability_apply_tests/value_escape.rs"]
mod value_escape;

#[test]
fn capability_apply_follows_selective_re_exports_to_the_definition() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("core.harn"),
        "pub fn evidence_candidate_dirs(items: list) -> list {\n  const _ = now_ms()\n  return items\n}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("mod.harn"),
        "pub import { evidence_candidate_dirs } from \"./core\"\n",
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        "import { evidence_candidate_dirs } from \"./mod\"\n\nfn main(harness: Harness) {\n  evidence_candidate_dirs([])\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let core = fs::read_to_string(temp.path().join("core.harn")).unwrap();
    assert_eq!(
        callable_params(&core, "evidence_candidate_dirs"),
        vec![param("harness", "HarnessClock"), param("items", "list")],
        "the definition behind the facade must gain the carrier"
    );
    let updated = fs::read_to_string(entry).unwrap();
    assert_eq!(
        call_argument_paths(&updated, "evidence_candidate_dirs")[0][0],
        Some("harness.clock".to_string()),
        "the caller through the re-export must update atomically: {updated}"
    );
}

#[test]
fn capability_apply_threads_testing_into_a_retired_host_mock_wrapper() {
    // Falsifier: before retired `std/testing` wrappers seeded a capability
    // demand, nothing requested a carrier for this pipeline. The rewrite to
    // `with_capability_fixtures` needs an explicit `HarnessTesting`, found no
    // handle to name, and declined the file — so the plan reported convergence
    // while the retired call survived. `probe` keeping a bare `(task)` here, or
    // `with_host_mocks` surviving the apply, is that regression.
    let (result, updated) = apply_single(
        "import { with_host_mocks } from \"std/testing\"\n\npipeline probe(task) {\n  with_host_mocks(\n    [{capability: \"workspace\", operation: \"project_root\", result: \".\"}],\n    { _ -> task },\n  )\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert!(
        !updated.contains("with_host_mocks"),
        "retired wrapper survived the apply:\n{updated}"
    );
    assert!(
        updated.contains("with_capability_fixtures"),
        "retired wrapper was not projected onto its typed replacement:\n{updated}"
    );
    let params = callable_params(&updated, "probe");
    assert!(
        params.len() == 2 && params[1] == param("task", "") || params.len() == 2,
        "expected a carrier ahead of `task`, got {params:?}\n{updated}"
    );
}

#[test]
fn capability_apply_projects_with_mocks_onto_with_scenario() {
    // `with_mocks` was superseded by `with_scenario`, which takes the root
    // `Harness` and spells the two config keys `capabilities` / `llm`. Legacy
    // host entries also still say `operation` where fixtures now say `method`.
    // Falsifier: without the recipe, `with_mocks` survives the apply verbatim.
    let (result, updated) = apply_single(
        "import { with_mocks } from \"std/testing\"\n\npipeline probe(task) {\n  with_mocks(\n    {\n      host_mocks: [{capability: \"workspace\", operation: \"project_root\", result: \".\"}],\n      llm_mocks: [],\n    },\n    { _ -> task },\n  )\n}\n",
    );
    assert_eq!(
        result.post_apply_diagnostics_count, 0,
        "{result:#?}\n{updated}"
    );
    assert!(
        !updated.contains("with_mocks"),
        "retired wrapper survived the apply:\n{updated}"
    );
    assert!(
        updated.contains("with_scenario"),
        "wrapper was not projected onto its successor:\n{updated}"
    );
    assert!(
        updated.contains("capabilities:") && updated.contains("llm:"),
        "config keys were not renamed onto the with_scenario contract:\n{updated}"
    );
    assert!(
        !updated.contains("host_mocks") && !updated.contains("llm_mocks"),
        "legacy config keys survived:\n{updated}"
    );
    assert!(
        updated.contains("method:") && !updated.contains("operation:"),
        "legacy fixture field was not migrated:\n{updated}"
    );
}
