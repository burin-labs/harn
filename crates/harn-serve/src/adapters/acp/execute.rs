//! Pipeline execution glue — compiles and runs a Harn chunk under the
//! ACP bridge, and loads MCP clients from host capabilities.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use harn_parser::acp_ambient_globals::AcpAmbientGlobal;

use super::{builtins, AcpBridge, AcpRuntimeConfigurator};

#[derive(Debug)]
pub(super) struct PromptExecutionError {
    pub message: String,
    pub terminal_class: harn_vm::llm::AgentTerminalClass,
    /// Machine facts projected from the thrown error dict (provider/model of the
    /// route that actually failed, kind/reason/category/code, retry hints).
    /// Empty for compile/setup errors and bare thrown strings.
    pub facts: super::types::AcpPromptFailureFacts,
}

impl PromptExecutionError {
    fn from_vm_error(vm: &harn_vm::Vm, error: &harn_vm::VmError) -> Self {
        let message = vm.format_runtime_error(error);
        let thrown = harn_vm::llm::vm_value_to_json(&error.thrown_value());
        let classification_input = if thrown.is_object() {
            thrown.clone()
        } else {
            serde_json::json!({ "message": message.as_str() })
        };
        let terminal_class =
            harn_vm::llm::agent_terminal_class("error", "", Some(&classification_input))
                .unwrap_or(harn_vm::llm::AgentTerminalClass::GenericThrow);
        Self {
            message,
            terminal_class,
            facts: super::types::AcpPromptFailureFacts::from_thrown(&thrown),
        }
    }
}

impl From<String> for PromptExecutionError {
    fn from(message: String) -> Self {
        Self {
            message,
            terminal_class: harn_vm::llm::AgentTerminalClass::GenericThrow,
            facts: super::types::AcpPromptFailureFacts::default(),
        }
    }
}

#[cfg(test)]
mod prompt_execution_error_tests {
    use super::*;

    #[test]
    fn typed_vm_category_outweighs_misleading_message_prose() {
        let vm = harn_vm::Vm::new();
        let error = harn_vm::VmError::CategorizedError {
            category: harn_vm::ErrorCategory::ToolRejected,
            message: "provider rate limit 429 in /tmp/run-429/result".to_string(),
        };

        let prompt_error = PromptExecutionError::from_vm_error(&vm, &error);
        assert_eq!(
            prompt_error.terminal_class,
            harn_vm::llm::AgentTerminalClass::ToolPolicyRejected
        );
    }

    #[test]
    fn ambiguous_vm_category_does_not_claim_provider_provenance() {
        let vm = harn_vm::Vm::new();
        let error = harn_vm::VmError::CategorizedError {
            category: harn_vm::ErrorCategory::Auth,
            message: "missing harness tenant principal".to_string(),
        };

        let prompt_error = PromptExecutionError::from_vm_error(&vm, &error);
        assert_eq!(
            prompt_error.terminal_class,
            harn_vm::llm::AgentTerminalClass::GenericThrow
        );
    }

    #[test]
    fn resource_contention_preserves_its_typed_terminal_class() {
        let vm = harn_vm::Vm::new();
        let error = harn_vm::VmError::CategorizedError {
            category: harn_vm::ErrorCategory::ResourceBusy,
            message: "session_store: database is locked".to_string(),
        };

        let prompt_error = PromptExecutionError::from_vm_error(&vm, &error);
        assert_eq!(
            prompt_error.terminal_class,
            harn_vm::llm::AgentTerminalClass::ResourceBusy
        );
    }
}

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
    /// The session's capability profile, resolved at `session/new`. When
    /// present, its allowlist + grants govern this turn's subprocess
    /// environments; when `None`, subprocesses inherit the server env (legacy).
    pub session_profile: Option<harn_vm::security::SessionProfile>,
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
) -> Result<String, PromptExecutionError> {
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
    // Bind the ACP session-prompt ambient globals from the single source of
    // truth the type-checker allowlist also consumes (`harn_parser`), so a
    // global bound here can never be one `harn check` rejects. The exhaustive
    // match makes adding a global a deliberate, checked change on both sides.
    let mut mcp_globals = load_host_mcp_clients(host_bridge.clone()).await;
    for global in AcpAmbientGlobal::ALL {
        let value = match global {
            AcpAmbientGlobal::Prompt => harn_vm::VmValue::String(arcstr::ArcStr::from(prompt.text)),
            AcpAmbientGlobal::PromptContent => {
                harn_vm::json_to_vm_value(&serde_json::Value::Array(prompt.content.to_vec()))
            }
            AcpAmbientGlobal::PromptMessages => {
                harn_vm::json_to_vm_value(&serde_json::Value::Array(prompt.messages.to_vec()))
            }
            AcpAmbientGlobal::Cwd => {
                harn_vm::VmValue::String(arcstr::ArcStr::from(setup.cwd.to_string_lossy().as_ref()))
            }
            AcpAmbientGlobal::Mcp => {
                // Only bind `mcp` when a host actually supplied MCP clients; an
                // empty map is left unbound, preserving prior behavior.
                if mcp_globals.is_empty() {
                    continue;
                }
                harn_vm::VmValue::dict(std::mem::take(&mut mcp_globals))
            }
        };
        vm.set_global(global.name(), value);
    }

    builtins::register_acp_builtins(&mut vm, bridge.clone()).await;

    // Share the same cancellation flag the ACP session's `cancellation`
    // already installed on `host_bridge` (see prompt.rs) with the VM's
    // cooperative cancel token, so Esc / `session/cancel` unwinds the VM step
    // loop and kills in-flight `process.exec` children instead of only
    // interrupting outstanding bridge calls.
    vm.install_cancel_token(host_bridge.cancelled_flag());

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
        // Non-secret receipts for the session's grants (empty for a hermetic or
        // no-profile run) travel on the propagated execution record.
        grants: setup
            .session_profile
            .as_ref()
            .map(harn_vm::security::SessionProfile::receipts)
            .unwrap_or_default(),
        ..Default::default()
    };
    harn_vm::stdlib::process::set_thread_execution_context(Some(execution));
    // Install the session's capability profile so this turn's subprocesses build
    // their environment through the closed allowlist + grants resolver. `None`
    // leaves the legacy inherit-the-server-env behavior untouched.
    harn_vm::stdlib::process::set_session_profile(setup.session_profile.clone());
    // Module preparation is lazy: imports are resolved while the chunk runs, so
    // this work lands inside `execute_ms` rather than in `vm_setup_ms`. Without
    // this attribution a pipeline whose import tree dominates the turn is
    // indistinguishable from one doing real work — the recorder exists in
    // harn-vm but nothing in production had ever switched it on.
    let module_phases = vm.enable_module_phase_timing();
    let execute_started = Instant::now();
    let result = match vm.execute_arc(std::sync::Arc::new(chunk)).await {
        Ok(_) => Ok(vm.output().to_string()),
        Err(e) => Err(PromptExecutionError::from_vm_error(&vm, &e)),
    };
    let execute_ms = execute_started.elapsed().as_millis() as u64;
    let modules = module_phases.snapshot();
    bridge.send_log(
        "info",
        &format!(
            "ACP_BOOT: execute_ms={execute_ms} module_load_ms={} module_compile_ms={} modules_loaded={} pipeline={pipeline_name}",
            modules.module_load_ms, modules.module_compile_ms, modules.modules_loaded,
        ),
        Some(serde_json::json!({
            "pipeline": pipeline_name.as_str(),
            "execute_ms": execute_ms,
            "module_load_ms": modules.module_load_ms,
            "module_compile_ms": modules.module_compile_ms,
            "modules_loaded": modules.modules_loaded,
            "modules_compiled": modules.modules_compiled,
        })),
    );
    harn_vm::stdlib::process::set_session_profile(None);
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

    #[test]
    fn acp_project_root_prefers_explicit_session_root() {
        let session_root = tempfile::tempdir().expect("session root");
        let pipeline_root = tempfile::tempdir().expect("pipeline root");
        let source_path = pipeline_root.path().join("agent.harn");

        assert_eq!(
            acp_project_root(
                Some(&source_path),
                pipeline_root.path(),
                Some(session_root.path())
            ),
            Some(std::fs::canonicalize(session_root.path()).expect("canonical session root"))
        );
    }

    #[test]
    fn acp_project_root_falls_back_to_nearest_harn_project() {
        let project_root = tempfile::tempdir().expect("project root");
        let nested = project_root.path().join("pipelines");
        std::fs::create_dir(&nested).expect("nested");
        std::fs::write(project_root.path().join("harn.toml"), "").expect("harn.toml");
        let source_path = nested.join("agent.harn");

        assert_eq!(
            acp_project_root(Some(&source_path), &nested, None),
            Some(project_root.path().to_path_buf())
        );
    }

    /// Regression for the missing `vm.install_cancel_token(...)` call: the ACP
    /// prompt path already shares one `cancelled` flag between the session's
    /// `SessionCancellation` and `HostBridge` (see `prompt.rs`), but the VM
    /// itself never learned about it, so `is_cancelled()` (and every
    /// `process.exec` child registered through `op_interrupt`) stayed blind to
    /// a `session/cancel` that arrived mid-turn. Pre-cancelling before the turn
    /// starts and asserting the VM observes it end-to-end is deterministic and
    /// avoids needing a real subprocess or timing race.
    #[tokio::test(flavor = "current_thread")]
    async fn execute_chunk_installs_host_bridge_cancel_token_on_the_vm() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::sync::Mutex as TokioMutex;

        let cwd = tempfile::tempdir().expect("cwd");
        let source =
            "pipeline main() {\n  __io_println(json_stringify({cancelled: is_cancelled()}))\n}\n"
                .to_string();
        let chunk = harn_vm::compile_source(&source).expect("compile inline pipeline");

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cancellation = super::super::SessionCancellation::default();
        // Pre-cancel: this is what a `session/cancel` that raced ahead of
        // `session/prompt` looks like on the wire, and it lets this test stay
        // synchronous instead of racing a background cancel against the turn.
        cancellation.cancelled.store(true, Ordering::SeqCst);

        let bridge = Arc::new(AcpBridge {
            session_id: "cancel-token-test".to_string(),
            output: super::super::AcpOutput::Channel(tx),
            pending: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            next_id_counter: AtomicU64::new(1),
            cancellation: cancellation.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(
                harn_vm::visible_text::VisibleTextState::default(),
            ),
        });

        let host_bridge = Arc::new(
            harn_vm::bridge::HostBridge::from_parts_with_writer_and_cancel_notify(
                Arc::new(TokioMutex::new(std::collections::HashMap::new())),
                cancellation.cancelled.clone(),
                cancellation.notify.clone(),
                Arc::new(|_line: &str| Ok(())),
                1,
            ),
        );

        let output = execute_chunk(
            chunk,
            bridge,
            host_bridge,
            PromptGlobals {
                text: "",
                content: &[],
                messages: &[],
            },
            VmSetup {
                source: &source,
                baseline: None,
                baseline_cache_hit: None,
                baseline_prepare_ms: 0,
                source_path: None,
                cwd: cwd.path(),
                project_root: None,
                runtime_configurator: Arc::new(super::super::NoopAcpRuntimeConfigurator),
                session_profile: None,
            },
        )
        .await
        .expect("cancelled-but-otherwise-normal turn should still execute");

        assert_eq!(
            output, "{\"cancelled\":true}\n",
            "the VM's cancel token must observe the same flag the ACP session \
             already shares with HostBridge, so is_cancelled() (and, by the \
             same wiring, in-flight process.exec children) see a session/cancel"
        );
    }
}
