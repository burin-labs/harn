use super::*;

fn diagnostics(source: &str) -> Vec<PreflightDiagnostic> {
    let file = unique_temp_dir("harn-check-llm-composition").join("main.harn");
    let program = parse_program(source);
    collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default())
        .into_iter()
        .filter(|diagnostic| diagnostic.code.as_str() == "HARN-LLM-006")
        .collect()
}

#[test]
fn preflight_rejects_only_provably_unsafe_literal_llm_compositions() {
    let rejected = [
        r#"
fn main(harness: Harness) {
  agent_options({
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
  })
}
"#,
        r#"
fn main(harness: Harness) {
  llm_call("hello", nil, {
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
    tools: [{name: "echo"}],
  })
}
"#,
        r#"
fn main(harness: Harness) {
  llm_completion("prefix", nil, nil, {
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
    tools: [{name: "echo"}],
  })
}
"#,
    ];
    for source in rejected {
        let diagnostics = diagnostics(source);
        assert_eq!(
            diagnostics.len(),
            1,
            "known-unsafe literal composition should fail: {source}"
        );
        assert!(
            diagnostics[0].message.contains("native_unreliable"),
            "diagnostic should preserve the capability registry's reason: {}",
            diagnostics[0].message
        );
    }

    let accepted = [
        // The catalog-recommended text channel is safe on this route.
        r#"
fn main(harness: Harness) {
  agent_options({
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "text",
  })
}
"#,
        // Agent probes retain the existing audited experiment seam.
        r#"
fn main(harness: Harness) {
  agent_options({
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
    tool_format_override_reason: "measure provider behavior",
  })
}
"#,
        // Unknown custom routes remain open-world.
        r#"
fn main(harness: Harness) {
  agent_options({
    provider: "my-proxy",
    model: "custom-model",
    tool_format: "native",
  })
}
"#,
        // Dynamic configuration remains under the runtime gate.
        r#"
fn run(model, format) {
  agent_options({
    provider: "openrouter",
    model: model,
    tool_format: format,
  })
}
"#,
        r#"
fn run(provider) {
  agent_options({
    provider: provider,
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
  })
}
"#,
        // A raw LLM call without tools has no tool channel to validate.
        r#"
fn main(harness: Harness) {
  llm_call("hello", nil, {
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
  })
}
"#,
    ];
    for source in accepted {
        assert!(
            diagnostics(source).is_empty(),
            "legitimate composition should remain accepted: {source}"
        );
    }
}

#[test]
fn check_report_exposes_unsafe_llm_composition_code() {
    let dir = unique_temp_dir("harn-check-llm-composition-report");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
fn main(harness: Harness) {
  agent_options({
    provider: "openrouter",
    model: "deepseek/deepseek-v3.2",
    tool_format: "native",
  })
}
"#,
    )
    .unwrap();
    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let report = check_file_report(
        &mut analysis,
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        true,
    );

    assert!(report.outcome().has_error);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("HARN-LLM-006")),
        "public check report should expose HARN-LLM-006: {:?}",
        report.diagnostics
    );
    let _ = std::fs::remove_dir_all(&dir);
}
