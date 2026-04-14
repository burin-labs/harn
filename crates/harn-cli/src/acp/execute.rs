//! Pipeline execution glue — compiles and runs a Harn chunk under the
//! ACP bridge, and loads MCP clients from host capabilities.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use super::{builtins, AcpBridge};

/// Compile harn source code into a bytecode chunk.
/// Execute a compiled chunk with ACP bridge builtins.
pub(super) async fn execute_chunk(
    chunk: harn_vm::Chunk,
    bridge: Rc<AcpBridge>,
    host_bridge: Rc<harn_vm::bridge::HostBridge>,
    prompt_text: &str,
    source_path: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> Result<String, String> {
    let vm_setup_started = Instant::now();
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    // Use project root (harn.toml) for metadata/store, falling back to cwd.
    let source_parent = source_path.and_then(|p| p.parent()).unwrap_or(cwd);
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent)
        .or_else(|| harn_vm::stdlib::process::find_project_root(cwd));
    let store_base = project_root.as_deref().unwrap_or(cwd);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = source_path
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("acp");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    bridge.set_script_name(pipeline_name);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    if let Some(path) = source_path {
        let path_str = path.to_string_lossy();
        let source = std::fs::read_to_string(path).unwrap_or_default();
        vm.set_source_info(&path_str, &source);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                vm.set_source_dir(parent);
            }
        }
    } else {
        vm.set_source_dir(cwd);
    }

    // Inject the prompt text as a global variable so pipelines can access it.
    vm.set_global("prompt", harn_vm::VmValue::String(Rc::from(prompt_text)));
    vm.set_global(
        "cwd",
        harn_vm::VmValue::String(Rc::from(cwd.to_string_lossy().as_ref())),
    );

    let mcp_globals = load_host_mcp_clients(host_bridge.clone()).await;
    if !mcp_globals.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::Dict(Rc::new(mcp_globals)));
    }

    // Register ACP-specific builtins that delegate file I/O to the editor.
    builtins::register_acp_builtins(&mut vm, bridge.clone()).await;

    // Set up bridge delegation so unknown builtins are forwarded to the ACP
    // client as `builtin_call` JSON-RPC requests. This remains the stable ACP
    // behavior until host-local pseudo-builtins are fully migrated to typed
    // host capabilities and explicit Harn stdlib wrappers.
    host_bridge.set_script_name(pipeline_name);
    vm.set_bridge(host_bridge.clone());

    // Override the native text-only agent_loop with the tool-aware version.
    // This allows agent_loop to execute tools via the ACP bridge (delegated
    // to the editor/CLI which has the full tool infrastructure).
    harn_vm::llm::register_agent_loop_with_bridge(&mut vm, host_bridge.clone());

    // Override llm_call with bridge-aware version for call_start/call_end observability.
    harn_vm::llm::register_llm_call_with_bridge(&mut vm, host_bridge);

    let vm_setup_ms = vm_setup_started.elapsed().as_millis() as u64;
    bridge.send_log(
        "info",
        &format!("ACP_BOOT: vm_setup_ms={vm_setup_ms} pipeline={pipeline_name}"),
        Some(serde_json::json!({
            "pipeline": pipeline_name,
            "vm_setup_ms": vm_setup_ms,
        })),
    );

    let execution = harn_vm::orchestration::RunExecutionRecord {
        cwd: Some(cwd.to_string_lossy().to_string()),
        source_dir: source_path
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string()),
        ..Default::default()
    };
    harn_vm::stdlib::process::set_thread_execution_context(Some(execution));
    let execute_started = Instant::now();
    let result = match vm.execute(&chunk).await {
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
            "pipeline": pipeline_name,
            "execute_ms": execute_ms,
        })),
    );
    harn_vm::stdlib::process::set_thread_execution_context(None);
    result
}

pub(super) async fn load_host_mcp_clients(
    host_bridge: Rc<harn_vm::bridge::HostBridge>,
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
                mcp_dict.insert(handle.name.clone(), harn_vm::VmValue::McpClient(handle));
            }
            Err(err) => {
                let name = server
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                eprintln!("warning: mcp: failed to connect to '{}': {}", name, err);
            }
        }
    }

    mcp_dict
}
