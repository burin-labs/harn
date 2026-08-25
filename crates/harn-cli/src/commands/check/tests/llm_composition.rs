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
fn preflight_rejects_literal_portable_options_declared_unsupported() {
    let rejected = r#"
fn main(harness: Harness) {
  llm_call("hello", nil, {
    provider: "moonshot",
    model: "kimi-k3",
    temperature: 0.2,
    top_p: 0.9,
  })
}
"#;
    let found = diagnostics(rejected);
    assert_eq!(found.len(), 2, "one diagnostic per rejected intent");
    assert!(found.iter().any(|diagnostic| diagnostic
        .message
        .contains("option `temperature` is not supported")));
    assert!(found.iter().any(|diagnostic| diagnostic
        .message
        .contains("option `top_p` is not supported")));

    let open_world = r#"
fn main(harness: Harness) {
  llm_call("hello", nil, {
    provider: "my-proxy",
    model: "custom-model",
    temperature: 0.2,
  })
}
"#;
    assert!(diagnostics(open_world).is_empty());
}

#[test]
fn preflight_checks_static_cache_intent_and_defers_dynamic_values() {
    harn_vm::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.test-provider]]
model_match = "no-cache"
prompt_caching = false

[[provider.test-provider]]
model_match = "cache-with-ttl"
prompt_caching = true
prompt_cache_ttls = ["5m"]

[[provider.test-provider]]
model_match = "no-temperature"
temperature_supported = false
"#,
    )
    .expect("capability overlay");

    let source = r#"
fn main(harness: Harness) {
  let dynamic_temperature = 0.2
  llm_call("cache", nil, {
    provider: "test-provider",
    model: "no-cache",
    cache: true,
  })
  llm_call("ttl", nil, {
    provider: "test-provider",
    model: "cache-with-ttl",
    prompt_cache_ttl: "1h",
  })
  llm_call("deferred", nil, {
    provider: "test-provider",
    model: "no-temperature",
    temperature: dynamic_temperature,
    cache: false,
  })
}
"#;
    let found = diagnostics(source);
    assert_eq!(found.len(), 2, "only provable capability gaps diagnose");
    assert!(found
        .iter()
        .any(|diagnostic| diagnostic.message.contains("option `cache`")));
    assert!(found.iter().any(|diagnostic| diagnostic
        .message
        .contains("option `prompt_cache_ttl` value `1h`")));

    harn_vm::llm::capabilities::clear_user_overrides();
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

/// The `llm_*` globals this pass keys on were replaced by `harness.llm.*`, so
/// the check has to hold in three directions at once: it still fires on the
/// removed spelling, it now fires on the one the compiler recommends, and it
/// stays quiet on an unrelated receiver whose method name happens to collide.
/// Only asserting that some spelling errors would have passed before the fix.
#[test]
fn preflight_checks_llm_composition_through_the_capability_method() {
    const OPTIONS: &str = r#"{
    provider: "moonshot",
    model: "kimi-k3",
    temperature: 0.2,
  }"#;

    let legacy =
        format!("fn main(harness: Harness) {{\n  llm_call(\"hello\", nil, {OPTIONS})\n}}\n");
    let legacy_found = diagnostics(&legacy);
    assert_eq!(
        legacy_found.len(),
        1,
        "the removed ambient spelling must keep reporting: {legacy_found:?}"
    );
    assert!(legacy_found[0].message.contains("`llm_call`"));

    let capability = format!(
        "fn main(harness: Harness) {{\n  harness.llm.call(\"hello\", nil, {OPTIONS})\n}}\n"
    );
    let capability_found = diagnostics(&capability);
    assert_eq!(
        capability_found.len(),
        1,
        "the supported capability spelling must report too: {capability_found:?}"
    );
    assert!(
        capability_found[0].message.contains("`harness.llm.call`"),
        "the diagnostic should name the spelling at the call site: {}",
        capability_found[0].message
    );
    assert!(capability_found[0]
        .message
        .contains("option `temperature` is not supported"));

    // `call` is a real `harness.llm` method, so this is the exact collision the
    // receiver check exists to reject.
    let unrelated =
        format!("fn main(harness: Harness) {{\n  client.call(\"hello\", nil, {OPTIONS})\n}}\n");
    assert!(
        diagnostics(&unrelated).is_empty(),
        "a non-harness receiver must not be treated as a capability call"
    );
}

/// `completion` takes its options one slot later than `call`. Resolving the
/// receiver back to its registry name is what keeps that index correct without
/// a second table keyed by `(capability, method)`.
#[test]
fn preflight_uses_the_registry_name_to_find_the_options_slot() {
    let source = r#"
fn main(harness: Harness) {
  harness.llm.completion("prefix", nil, nil, {
    provider: "moonshot",
    model: "kimi-k3",
    temperature: 0.2,
  })
}
"#;
    let found = diagnostics(source);
    assert_eq!(
        found.len(),
        1,
        "completion options live at index 3: {found:?}"
    );
    assert!(found[0].message.contains("`harness.llm.completion`"));
}
