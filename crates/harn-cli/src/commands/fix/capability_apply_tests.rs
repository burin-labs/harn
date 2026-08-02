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
