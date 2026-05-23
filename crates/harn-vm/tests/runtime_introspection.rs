#![recursion_limit = "256"]
//! Integration tests for the runtime-introspection tool bundle (harn#2188).
//!
//! Covers the full opt-in surface from a Harn script:
//!   - `runtime_introspection()` returns the full snapshot dict
//!   - `runtime_introspection_tools(reg)` adds the model-callable tools
//!   - selective opt-in via `{only: [...]}` and `{exclude: [...]}`
//!   - the tools dispatch through the VM stdlib short-circuit and surface
//!     resolved facts (not training-prior guesses) after an `llm_call`
//!   - missing metadata gracefully degrades to a `resolved: false` envelope
//!   - the snapshot redaction allowlist holds across the wire

use harn_vm::llm::introspection;
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

fn out(source: &str) -> Vec<String> {
    let raw = run(source).unwrap();
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn runtime_introspection_starts_unresolved() {
    let lines = out(r#"
pipeline main(task) {
  let snap = runtime_introspection()
  log(snap.provider == nil)
  log(snap.model == nil)
  log(snap.harn_version != nil)
  log(snap.harness != nil)
}
"#);
    assert_eq!(lines, vec!["true", "true", "true", "true"]);
}

#[test]
fn introspection_tools_bundle_adds_all_by_default() {
    let lines = out(r#"
pipeline main(task) {
  let reg = runtime_introspection_tools(tool_registry())
  let names = []
  var collected = names
  for entry in reg.tools {
    collected = collected + [entry.name]
  }
  for name in ["current_model", "current_provider", "current_context_window",
               "current_harn_version", "current_harness",
               "available_runtime_capabilities", "current_compaction_policy"] {
    var found = false
    for n in collected {
      if n == name {
        found = true
      }
    }
    log(name + "=" + to_string(found))
  }
}
"#);
    for expected in [
        "current_model=true",
        "current_provider=true",
        "current_context_window=true",
        "current_harn_version=true",
        "current_harness=true",
        "available_runtime_capabilities=true",
        "current_compaction_policy=true",
    ] {
        assert!(
            lines.contains(&expected.to_string()),
            "missing tool registration {expected}; saw {lines:?}"
        );
    }
}

#[test]
fn introspection_tools_only_narrows_surface() {
    let lines = out(r#"
pipeline main(task) {
  let reg = runtime_introspection_tools(
    tool_registry(),
    {only: ["current_model", "current_provider"]},
  )
  log(len(reg.tools))
  let sorted = []
  var collected = sorted
  for entry in reg.tools {
    collected = collected + [entry.name]
  }
  for name in collected {
    log(name)
  }
}
"#);
    assert_eq!(lines[0], "2");
    assert!(lines.contains(&"current_model".to_string()));
    assert!(lines.contains(&"current_provider".to_string()));
}

#[test]
fn introspection_tools_exclude_drops_specified() {
    let lines = out(r#"
pipeline main(task) {
  let reg = runtime_introspection_tools(
    tool_registry(),
    {exclude: ["current_compaction_policy", "available_runtime_capabilities"]},
  )
  var has_compaction = false
  var has_capabilities = false
  var has_model = false
  for entry in reg.tools {
    if entry.name == "current_compaction_policy" {
      has_compaction = true
    }
    if entry.name == "available_runtime_capabilities" {
      has_capabilities = true
    }
    if entry.name == "current_model" {
      has_model = true
    }
  }
  log(has_compaction)
  log(has_capabilities)
  log(has_model)
}
"#);
    assert_eq!(lines, vec!["false", "false", "true"]);
}

#[test]
fn introspection_tools_are_idempotent() {
    let lines = out(r#"
pipeline main(task) {
  let once = runtime_introspection_tools(tool_registry())
  let twice = runtime_introspection_tools(once)
  log(len(once.tools) == len(twice.tools))
}
"#);
    assert_eq!(lines, vec!["true"]);
}

#[test]
fn introspection_tools_executor_is_harn() {
    let lines = out(r#"
pipeline main(task) {
  let reg = runtime_introspection_tools(tool_registry())
  for entry in reg.tools {
    log(entry.name + ":" + to_string(entry.executor))
  }
}
"#);
    for line in lines {
        assert!(
            line.ends_with(":harn"),
            "every introspection tool should declare executor: \"harn\", got `{line}`"
        );
    }
}

#[test]
fn snapshot_reads_resolved_call() {
    harn_vm::reset_thread_local_state();
    introspection::record_resolved_llm_call("anthropic", "claude-opus-4-7");
    let snapshot = introspection::current_snapshot().expect("snapshot");
    assert_eq!(snapshot.provider, "anthropic");
    assert_eq!(snapshot.model, "claude-opus-4-7");
    assert_eq!(snapshot.family, "claude");
    introspection::reset_snapshot();
}

#[test]
fn current_model_tool_dispatch_reports_resolution() {
    harn_vm::reset_thread_local_state();
    let payload =
        introspection::handle_introspection_tool("current_model", &serde_json::Value::Null)
            .expect("matched tool");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
    assert_eq!(parsed["resolved"], serde_json::json!(false));
    assert_eq!(parsed["model"], serde_json::json!(""));

    introspection::record_resolved_llm_call("anthropic", "claude-opus-4-7");
    let payload =
        introspection::handle_introspection_tool("current_model", &serde_json::Value::Null)
            .expect("matched tool");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
    assert_eq!(parsed["resolved"], serde_json::json!(true));
    assert_eq!(parsed["model"], serde_json::json!("claude-opus-4-7"));
    assert_eq!(parsed["family"], serde_json::json!("claude"));
    introspection::reset_snapshot();
}

#[test]
fn current_provider_tool_dispatch_reports_tool_format() {
    harn_vm::reset_thread_local_state();
    introspection::record_resolved_llm_call("anthropic", "claude-opus-4-7");
    let payload =
        introspection::handle_introspection_tool("current_provider", &serde_json::Value::Null)
            .expect("matched tool");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
    assert_eq!(parsed["provider"], serde_json::json!("anthropic"));
    assert!(parsed["tool_format"].is_string());
    introspection::reset_snapshot();
}

#[test]
fn harness_and_version_dispatch_without_resolution() {
    harn_vm::reset_thread_local_state();
    let version_payload =
        introspection::handle_introspection_tool("current_harn_version", &serde_json::Value::Null)
            .expect("matched tool");
    let version_parsed: serde_json::Value = serde_json::from_str(&version_payload).expect("json");
    assert!(version_parsed["harn_version"].is_string());
    assert!(!version_parsed["harn_version"].as_str().unwrap().is_empty());

    let harness_payload =
        introspection::handle_introspection_tool("current_harness", &serde_json::Value::Null)
            .expect("matched tool");
    let harness_parsed: serde_json::Value = serde_json::from_str(&harness_payload).expect("json");
    assert!(harness_parsed["harness"].is_string());
    assert!(!harness_parsed["harness"].as_str().unwrap().is_empty());
}

#[test]
fn capabilities_and_context_window_track_resolved_model() {
    harn_vm::reset_thread_local_state();
    introspection::record_resolved_llm_call("anthropic", "claude-opus-4-7");
    let payload = introspection::handle_introspection_tool(
        "available_runtime_capabilities",
        &serde_json::Value::Null,
    )
    .expect("matched tool");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
    assert_eq!(parsed["resolved"], serde_json::json!(true));
    // Capabilities is a dict (or null when the model is unknown); a known
    // catalog entry produces a populated dict.
    assert!(
        parsed["capabilities"].is_object(),
        "expected capabilities object, got {}",
        parsed["capabilities"]
    );

    let ctx = introspection::handle_introspection_tool(
        "current_context_window",
        &serde_json::Value::Null,
    )
    .expect("matched tool");
    let ctx_parsed: serde_json::Value = serde_json::from_str(&ctx).expect("json");
    assert_eq!(ctx_parsed["resolved"], serde_json::json!(true));
    // Anthropic Claude Opus 4.7 ships with a non-null catalog context window.
    assert!(
        ctx_parsed["context_window"].is_number(),
        "expected context_window number, got {}",
        ctx_parsed["context_window"]
    );
    introspection::reset_snapshot();
}

#[test]
fn host_disabled_means_no_tools_attached() {
    // The "minimal harness" path: never call runtime_introspection_tools.
    // The model can only invoke tools that are in the registry, so an
    // empty registry literally cannot expose the introspection surface.
    let lines = out(r#"
pipeline main(task) {
  let reg = tool_registry()
  log(len(reg.tools))
  log(tool_find(reg, "current_model") == nil)
}
"#);
    assert_eq!(lines, vec!["0", "true"]);
}

#[test]
fn snapshot_does_not_leak_outside_allowlist() {
    harn_vm::reset_thread_local_state();
    introspection::record_resolved_llm_call("anthropic", "claude-opus-4-7");
    let value = introspection::snapshot_to_vm_value(introspection::current_snapshot().as_ref());
    let dict = value.as_dict().expect("dict");
    let allowed: std::collections::BTreeSet<&str> = [
        "harn_version",
        "harness",
        "provider",
        "model",
        "model_alias",
        "family",
        "tool_format",
        "tier",
        "context_window",
        "runtime_context_window",
        "capabilities",
    ]
    .into_iter()
    .collect();
    for key in dict.keys() {
        assert!(
            allowed.contains(key.as_str()),
            "introspection snapshot leaked field `{key}` outside the documented allowlist"
        );
    }
    introspection::reset_snapshot();
}
