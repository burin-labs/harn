//! One owner for how a tool registry entry spells its input schema.
//!
//! `tool_define` accepts three config spellings and normalizes them onto the
//! two entry keys the registry reads: `inputSchema` for a complete object-root
//! JSON Schema and legacy `parameters` for a per-parameter map. Every other
//! spelling must be refused by name at the boundary that owns the shape.
//!
//! The refusal half is what makes this suite non-vacuous: an entry that
//! declares properties under an unread spelling used to project
//! `{"properties": {}}`, which is indistinguishable from a tool that genuinely
//! takes no arguments.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn projected(source: &str) -> String {
    let raw = run(source).expect("script should project a catalog");
    raw.lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn refusal(source: &str) -> String {
    match run(source) {
        Ok(output) => panic!("expected a refusal, projected instead: {output}"),
        Err(error) => error,
    }
}

fn define_and_project(name: &str, declaration: &str) -> String {
    projected(&format!(
        r#"
pipeline main(harness: Harness, task: unknown) {{
  let r = tool_registry()
  r = tool_define(r, "{name}", "desc", {{
    handler: {{ _ -> "" }},
    {declaration}
    returns: {{type: "string"}},
  }})
  harness.stdio.log(to_string(tool_schema(r)))
}}
"#
    ))
}

fn define_and_refuse(name: &str, declaration: &str) -> String {
    refusal(&format!(
        r#"
pipeline main(harness: Harness, task: unknown) {{
  let r = tool_registry()
  r = tool_define(r, "{name}", "desc", {{
    handler: {{ _ -> "" }},
    {declaration}
    returns: {{type: "string"}},
  }})
  harness.stdio.log(to_string(tool_schema(r)))
}}
"#
    ))
}

/// Case D of the issue probe: the legacy per-parameter map still projects.
#[test]
fn legacy_parameter_map_projects_its_properties() {
    let schema = define_and_project(
        "read",
        r#"parameters: {path: {schema: {type: "string"}, required: true}},"#,
    );
    assert!(
        schema.contains("path") && schema.contains("required: [path]"),
        "legacy parameter map lost its properties: {schema}"
    );
}

/// Case B of the issue probe: the snake-case config spelling projects.
#[test]
fn snake_case_config_spelling_projects_its_properties() {
    let schema = define_and_project(
        "look",
        r#"input_schema: {type: "object", properties: {path: {type: "string"}}, required: ["path"]},"#,
    );
    assert!(
        schema.contains("path") && schema.contains("required: [path]"),
        "input_schema lost its properties: {schema}"
    );
}

/// Case C of the issue probe. On the reported build this was refused as a
/// double declaration even though the config declared one schema.
#[test]
fn mcp_style_config_spelling_projects_its_properties() {
    let schema = define_and_project(
        "glob",
        r#"inputSchema: {type: "object", properties: {pattern: {type: "string"}}, required: ["pattern"]},"#,
    );
    assert!(
        schema.contains("pattern") && schema.contains("required: [pattern]"),
        "inputSchema lost its properties: {schema}"
    );
    assert!(
        !schema.contains("not both"),
        "one declared schema was reported as two: {schema}"
    );
}

/// Case A of the issue probe. `parameters` owns the per-parameter map only, so
/// a complete schema there is refused, and the refusal has to name the tool and
/// the spelling that would work.
#[test]
fn complete_schema_under_parameters_is_refused_by_name() {
    let error = define_and_refuse(
        "search",
        r#"parameters: {type: "object", properties: {query: {type: "string"}}, required: ["query"]},"#,
    );
    assert!(
        error.contains("search") && error.contains("inputSchema"),
        "refusal names neither the tool nor the working spelling: {error}"
    );
}

/// Two genuinely competing declarations still lose, and the refusal says which
/// keys collided rather than inventing a declaration the author did not write.
#[test]
fn two_declared_schemas_are_refused() {
    let error = define_and_refuse(
        "both",
        r#"parameters: {path: {schema: {type: "string"}, required: true}},
    inputSchema: {type: "object", properties: {path: {type: "string"}}},"#,
    );
    assert!(
        error.contains("both") && error.contains("parameters") && error.contains("inputSchema"),
        "refusal does not name the colliding keys: {error}"
    );
}

/// The silence underneath the issue: an entry that reaches the registry
/// spelling its schema `input_schema` must be refused, never projected as a
/// tool that takes no arguments.
#[test]
fn entry_level_snake_case_spelling_is_refused_not_emptied() {
    let error = refusal(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const entry = {
    name: "look",
    description: "desc",
    input_schema: {type: "object", properties: {path: {type: "string"}}, required: ["path"]},
  }
  const r = tool_registry() + {tools: [entry]}
  harness.stdio.log(to_string(tool_schema(r)))
}
"#,
    );
    assert!(
        error.contains("look") && error.contains("inputSchema"),
        "entry-level input_schema was not refused by name: {error}"
    );
}
