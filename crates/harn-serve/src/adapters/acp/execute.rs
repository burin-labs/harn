//! Pipeline execution glue — compiles and runs a Harn chunk under the
//! ACP bridge, and loads MCP clients from host capabilities.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::{builtins, AcpBridge, AcpRuntimeConfigurator};

pub(super) struct PromptGlobals<'a> {
    pub text: &'a str,
    pub content: &'a [serde_json::Value],
    pub messages: &'a [serde_json::Value],
}

pub(super) struct VmSetup<'a> {
    pub source: &'a str,
    pub baseline: Option<&'a harn_vm::VmBaseline>,
    pub baseline_cache_hit: Option<bool>,
    pub baseline_prepare_ms: u64,
    pub source_path: Option<&'a Path>,
    pub cwd: &'a Path,
    pub project_root: Option<&'a Path>,
    pub runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
}

fn pipeline_name_for(source_path: Option<&Path>) -> String {
    source_path
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("acp")
        .to_string()
}

fn acp_project_root(
    source_path: Option<&Path>,
    cwd: &Path,
    explicit_project_root: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(root) = explicit_project_root {
        return Some(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
    }
    if let Ok(root) = std::env::var("HARN_PROJECT_ROOT") {
        if !root.trim().is_empty() {
            return Some(PathBuf::from(root));
        }
    }
    let source_parent = source_path.and_then(|p| p.parent()).unwrap_or(cwd);
    harn_vm::stdlib::process::find_project_root(source_parent)
        .or_else(|| harn_vm::stdlib::process::find_project_root(cwd))
}

async fn configure_stable_vm(
    vm: &mut harn_vm::Vm,
    source: &str,
    source_path: Option<&Path>,
    cwd: &Path,
    project_root: Option<&Path>,
    runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
) -> Result<String, String> {
    harn_vm::register_vm_stdlib(vm);
    // Metadata/store rooted at the launched project when supplied by the host,
    // otherwise at harn.toml when present.
    let project_root = acp_project_root(source_path, cwd, project_root);
    let store_base = project_root.as_deref().unwrap_or(cwd);
    harn_vm::register_store_builtins(vm, store_base);
    harn_vm::register_metadata_builtins(vm, store_base);
    let pipeline_name = pipeline_name_for(source_path);
    harn_vm::register_checkpoint_builtins(vm, store_base, &pipeline_name);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    if let Some(path) = source_path {
        let path_str = path.to_string_lossy();
        vm.set_source_info(&path_str, source);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                vm.set_source_dir(parent);
            }
        }
    } else {
        vm.set_source_dir(cwd);
    }

    runtime_configurator.configure(vm, source_path).await?;
    Ok(pipeline_name)
}

pub(super) async fn prepare_vm_baseline(
    source: &str,
    source_path: &Path,
    cwd: &Path,
    project_root: Option<&Path>,
    runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
) -> Result<harn_vm::VmBaseline, String> {
    let mut vm = harn_vm::Vm::new();
    configure_stable_vm(
        &mut vm,
        source,
        Some(source_path),
        cwd,
        project_root,
        runtime_configurator,
    )
    .await?;
    Ok(vm.baseline())
}

/// Execute a compiled chunk with ACP bridge builtins.
pub(super) async fn execute_chunk(
    chunk: harn_vm::Chunk,
    bridge: Arc<AcpBridge>,
    host_bridge: Arc<harn_vm::bridge::HostBridge>,
    prompt: PromptGlobals<'_>,
    setup: VmSetup<'_>,
) -> Result<String, String> {
    let vm_setup_started = Instant::now();
    let vm_setup_span =
        harn_vm::tracing::span_start(harn_vm::tracing::SpanKind::VmSetup, "acp_vm_setup".into());
    let pipeline_name = pipeline_name_for(setup.source_path);
    bridge.set_script_name(&pipeline_name);

    let mut vm = if let Some(baseline) = setup.baseline {
        baseline.instantiate()
    } else {
        let mut vm = harn_vm::Vm::new();
        configure_stable_vm(
            &mut vm,
            setup.source,
            setup.source_path,
            setup.cwd,
            setup.project_root,
            setup.runtime_configurator,
        )
        .await?;
        vm
    };

    vm.set_harness(harn_vm::Harness::real());
    vm.set_global(
        "prompt",
        harn_vm::VmValue::String(arcstr::ArcStr::from(prompt.text)),
    );
    vm.set_global(
        "prompt_content",
        harn_vm::json_to_vm_value(&serde_json::Value::Array(prompt.content.to_vec())),
    );
    vm.set_global(
        "prompt_messages",
        harn_vm::json_to_vm_value(&serde_json::Value::Array(prompt.messages.to_vec())),
    );
    vm.set_global(
        "cwd",
        harn_vm::VmValue::String(arcstr::ArcStr::from(setup.cwd.to_string_lossy().as_ref())),
    );

    let mcp_globals = load_host_mcp_clients(host_bridge.clone()).await;
    if !mcp_globals.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::dict(mcp_globals));
    }

    builtins::register_acp_builtins(&mut vm, bridge.clone()).await;

    // Forward unknown builtins to the ACP client as `builtin_call` JSON-RPC
    // until host-local pseudo-builtins are migrated to typed host
    // capabilities and explicit Harn stdlib wrappers.
    host_bridge.set_script_name(&pipeline_name);
    vm.set_bridge(host_bridge.clone());

    // Replace the text-only agent_loop with a tool-aware variant that
    // dispatches tools through the bridge.
    harn_vm::llm::register_agent_loop_with_bridge(&mut vm, host_bridge.clone());

    // Bridge-aware llm_call adds call_start/call_end observability.
    harn_vm::llm::register_llm_call_with_bridge(&mut vm, host_bridge.clone());
    // Bridge-aware llm_call_structured / llm_call_structured_safe run
    // the same schema-retry loop as the non-bridge path but emit
    // call_start/call_end notifications through the bridge.
    harn_vm::llm::register_llm_call_structured_with_bridge(&mut vm, host_bridge);

    let dynamic_setup_ms = vm_setup_started.elapsed().as_millis() as u64;
    let vm_setup_ms = setup.baseline_prepare_ms.saturating_add(dynamic_setup_ms);
    harn_vm::tracing::span_set_metadata(
        vm_setup_span,
        "baseline_cache",
        serde_json::Value::String(
            match setup.baseline_cache_hit {
                Some(true) => "hit",
                Some(false) => "miss",
                None => "none",
            }
            .to_string(),
        ),
    );
    harn_vm::tracing::span_set_metadata(
        vm_setup_span,
        "vm_setup_ms",
        serde_json::json!(vm_setup_ms),
    );
    harn_vm::tracing::span_end(vm_setup_span);
    bridge.send_log(
        "info",
        &format!("ACP_BOOT: vm_setup_ms={vm_setup_ms} pipeline={pipeline_name}"),
        Some(serde_json::json!({
            "pipeline": pipeline_name.as_str(),
            "vm_setup_ms": vm_setup_ms,
            "vm_setup_dynamic_ms": dynamic_setup_ms,
            "vm_baseline_prepare_ms": setup.baseline_prepare_ms,
            "vm_baseline_cache": match setup.baseline_cache_hit {
                Some(true) => "hit",
                Some(false) => "miss",
                None => "none",
            },
        })),
    );

    let execution = harn_vm::orchestration::RunExecutionRecord {
        cwd: Some(setup.cwd.to_string_lossy().into_owned()),
        project_root: setup.project_root.map(|p| p.to_string_lossy().into_owned()),
        source_dir: setup
            .source_path
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().into_owned()),
        ..Default::default()
    };
    harn_vm::stdlib::process::set_thread_execution_context(Some(execution));
    let execute_started = Instant::now();
    let result = match vm.execute_arc(std::sync::Arc::new(chunk)).await {
        Ok(_) => Ok(vm.output().to_string()),
        Err(e) => {
            let formatted = vm.format_runtime_error(&e);
            Err(formatted)
        }
    };
    let execute_ms = execute_started.elapsed().as_millis() as u64;
    bridge.send_log(
        "info",
        &format!("ACP_BOOT: execute_ms={execute_ms} pipeline={pipeline_name}"),
        Some(serde_json::json!({
            "pipeline": pipeline_name.as_str(),
            "execute_ms": execute_ms,
        })),
    );
    harn_vm::stdlib::process::set_thread_execution_context(None);
    result
}

pub(super) async fn load_host_mcp_clients(
    host_bridge: Arc<harn_vm::bridge::HostBridge>,
) -> BTreeMap<String, harn_vm::VmValue> {
    let mut mcp_dict = BTreeMap::new();
    let capabilities = host_bridge
        .call("host/capabilities", serde_json::json!({}))
        .await
        .ok()
        .and_then(|value| value.as_object().cloned());
    let has_project_mcp_config = capabilities
        .as_ref()
        .and_then(|root| root.get("project"))
        .and_then(|entry| entry.as_array())
        .is_some_and(|ops| ops.iter().any(|value| value.as_str() == Some("mcp_config")));
    if !has_project_mcp_config {
        return mcp_dict;
    }
    let response = match host_bridge
        .call(
            "host/call",
            serde_json::json!({
                "name": "project.mcp_config",
                "args": {}
            }),
        )
        .await
    {
        Ok(value) => value,
        Err(err) => {
            eprintln!("warning: mcp: failed to load host MCP config: {err}");
            return mcp_dict;
        }
    };

    let Some(servers) = response.as_array() else {
        return mcp_dict;
    };

    for server in servers {
        match harn_vm::connect_mcp_server_from_json(server).await {
            Ok(handle) => {
                eprintln!("[harn] mcp: connected to '{}'", handle.name);
                mcp_dict.insert(handle.name.clone(), harn_vm::VmValue::mcp_client(handle));
            }
            Err(err) => {
                let name = server
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                eprintln!("warning: mcp: failed to connect to '{name}': {err}");
            }
        }
    }

    mcp_dict
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ScopedEnvVar {
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(value: &Path) -> Self {
            let previous = std::env::var("HARN_PROJECT_ROOT").ok();
            std::env::set_var("HARN_PROJECT_ROOT", value);
            Self { previous }
        }

        fn remove() -> Self {
            let previous = std::env::var("HARN_PROJECT_ROOT").ok();
            std::env::remove_var("HARN_PROJECT_ROOT");
            Self { previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("HARN_PROJECT_ROOT", value),
                None => std::env::remove_var("HARN_PROJECT_ROOT"),
            }
        }
    }

    #[test]
    fn acp_project_root_prefers_host_project_root_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let host_root = tempfile::tempdir().expect("host root");
        let pipeline_root = tempfile::tempdir().expect("pipeline root");
        let _env = ScopedEnvVar::set(host_root.path());
        let source_path = pipeline_root.path().join("agent.harn");

        assert_eq!(
            acp_project_root(Some(&source_path), pipeline_root.path(), None),
            Some(host_root.path().to_path_buf())
        );
    }

    #[test]
    fn acp_project_root_prefers_explicit_session_root_over_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let host_root = tempfile::tempdir().expect("host root");
        let session_root = tempfile::tempdir().expect("session root");
        let pipeline_root = tempfile::tempdir().expect("pipeline root");
        let _env = ScopedEnvVar::set(host_root.path());
        let source_path = pipeline_root.path().join("agent.harn");

        assert_eq!(
            acp_project_root(
                Some(&source_path),
                pipeline_root.path(),
                Some(session_root.path())
            ),
            Some(session_root.path().to_path_buf())
        );
    }

    #[test]
    fn acp_project_root_falls_back_to_nearest_harn_project() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let project_root = tempfile::tempdir().expect("project root");
        let nested = project_root.path().join("pipelines");
        std::fs::create_dir(&nested).expect("nested");
        std::fs::write(project_root.path().join("harn.toml"), "").expect("harn.toml");
        let _env = ScopedEnvVar::remove();
        let source_path = nested.join("agent.harn");

        assert_eq!(
            acp_project_root(Some(&source_path), &nested, None),
            Some(project_root.path().to_path_buf())
        );
    }
}
