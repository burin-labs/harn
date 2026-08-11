//! ACP builtin registrations and terminal-exec glue.
//!
//! These builtins delegate VM-side presentation and editor-owned execution
//! (`log`, `print`, `exec`, ...) to the ACP client via the `AcpBridge`.
//! `host_call` itself is **not** replaced: ACP installs a
//! [`harn_vm::HostCallBridge`] so every session shares canonical dispatch
//! (mocks, command policy, process-handle registry, turn memo). See harn#5523.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use harn_vm::{HostCallBridge, HostCallDispatchFuture};

use super::AcpBridge;

fn log_message_and_fields(args: &[harn_vm::VmValue]) -> (String, Option<serde_json::Value>) {
    let message = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let fields = args.get(1).and_then(|value| {
        if matches!(value, harn_vm::VmValue::Nil) {
            None
        } else {
            Some(harn_vm::llm::vm_value_to_json(value))
        }
    });
    (message, fields)
}

/// Host capability contract is `request: dict`; accept either that shape
/// (`{text: ...}`) or a legacy bare string from older pipelines.
fn emit_response_text(args: &[harn_vm::VmValue]) -> String {
    match args.first() {
        Some(value) => {
            if let Some(map) = value.as_dict() {
                map.get("text")
                    .map(|item| item.display())
                    .unwrap_or_else(|| value.display())
            } else {
                value.display()
            }
        }
        None => String::new(),
    }
}

/// ACP embedder bridge for canonical `host_call` dispatch.
///
/// Forwards unhandled capability/operation pairs to the editor over
/// `host/call`. Runtime-owned ops (`process.exec` policy, spawn registry,
/// mocks, turn memo) are applied by harn-vm *before* this bridge is
/// consulted — that is the whole point of not replacing the builtin.
pub(super) struct AcpHostCallBridge {
    bridge: Arc<AcpBridge>,
    prompt_content: harn_vm::VmValue,
}

impl AcpHostCallBridge {
    pub(super) fn new(bridge: Arc<AcpBridge>, prompt_content: harn_vm::VmValue) -> Self {
        Self {
            bridge,
            prompt_content,
        }
    }
}

impl HostCallBridge for AcpHostCallBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a harn_vm::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        Box::pin(async move {
            // Session prompt content is local to the ACP prompt — serve it
            // without a host round-trip, matching the pre-#5523 short-circuit.
            if capability == "runtime" && operation == "prompt_content" {
                return Ok(Some(self.prompt_content.clone()));
            }
            let name = format!("{capability}.{operation}");
            let args = harn_vm::VmValue::dict(params.clone());
            let args_json = harn_vm::llm::vm_value_to_json(&args);
            let result = self
                .bridge
                .call_client(
                    "host/call",
                    serde_json::json!({
                        "sessionId": self.bridge.session_id,
                        "name": name,
                        "args": args_json,
                    }),
                )
                .await?;
            Ok(Some(harn_vm::bridge::json_result_to_vm_value(&result)))
        })
    }
}

/// Register builtins that delegate to the ACP client (editor).
pub(super) async fn register_acp_builtins(
    vm: &mut harn_vm::Vm,
    bridge: Arc<AcpBridge>,
    prompt_content: harn_vm::VmValue,
) {
    let host_capability_manifest = bridge
        .call_client(
            "host/capabilities",
            serde_json::json!({
                "sessionId": bridge.session_id,
            }),
        )
        .await
        .map(|result| {
            normalize_host_capability_manifest(harn_vm::bridge::json_result_to_vm_value(&result))
        })
        .unwrap_or_else(|_| harn_vm::VmValue::dict_map(Default::default()));
    let host_capability_manifest = advertise_runtime_prompt_content(host_capability_manifest);
    let selected_shell =
        if manifest_has_operation(&host_capability_manifest, "process", "get_default_shell") {
            bridge
                .call_client(
                    "host/call",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "name": "process.get_default_shell",
                        "args": {},
                    }),
                )
                .await
                .map(|result| harn_vm::bridge::json_result_to_vm_value(&result))
                .unwrap_or_else(|_| harn_vm::shells::default_shell_vm_value())
        } else {
            harn_vm::shells::default_shell_vm_value()
        };

    // Diagnostic logs must not become assistant-visible reply text.
    // Product paths call `harness.stdio.log` (capability method), not the bare
    // ambient `log` builtin. Routing either through `agent_message_chunk`
    // pollutes the streaming bubble and suppresses Response-only final-reply
    // fallback on interactive surfaces (Burin TUI smoke: AUTO stdio.log hid
    // `TUI_SMOKE_REPLY` from set_result).
    let b = bridge.clone();
    vm.register_builtin("log", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_log("info", &msg, None);
        Ok(harn_vm::VmValue::Nil)
    });
    let b = bridge.clone();
    vm.override_capability_method(
        harn_builtin_meta::CapabilityId::Stdio,
        "log",
        move |args, _out| {
            let msg = args.first().map(|a| a.display()).unwrap_or_default();
            b.send_log("info", &msg, None);
            Ok(harn_vm::VmValue::Nil)
        },
    );

    let b = bridge.clone();
    vm.register_builtin("print", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&msg);
        Ok(harn_vm::VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("println", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&format!("{msg}\n"));
        Ok(harn_vm::VmValue::Nil)
    });

    // Install the embedder bridge and keep the stdlib `host_call` builtin.
    // Replacing that builtin by name is what previously detached ACP from
    // mocks, command policy, the process-handle registry, and (until
    // harn#5526) the per-turn memo. harn#5523.
    harn_vm::set_host_call_bridge(Arc::new(AcpHostCallBridge::new(
        bridge.clone(),
        prompt_content,
    )));

    let host_capabilities_cache = host_capability_manifest.clone();
    vm.register_builtin("host_capabilities", move |_args, _out| {
        Ok(host_capabilities_cache.clone())
    });

    let host_has_cache = host_capability_manifest.clone();
    vm.register_builtin("host_has", move |args, _out| {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let op = args.get(1).map(|a| a.display());
        let valid = if let Some(manifest) = host_has_cache.as_dict() {
            if let Some(value) = manifest.get(capability.as_str()) {
                if let Some(cap) = value.as_dict() {
                    if let Some(op) = op {
                        cap.get("ops")
                            .and_then(|ops| match ops {
                                harn_vm::VmValue::List(list) => {
                                    Some(list.iter().any(|item| item.display() == op))
                                }
                                _ => None,
                            })
                            .unwrap_or(false)
                    } else {
                        true
                    }
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        Ok(harn_vm::VmValue::Bool(valid))
    });

    let b = bridge.clone();
    let shell = selected_shell.clone();
    vm.register_async_builtin("run_command", move |_ctx, args| {
        let bridge = b.clone();
        let shell = shell.clone();
        async move { acp_terminal_exec(&bridge, &args, &shell).await }
    });

    for level in ["log_debug", "log_info", "log_warn", "log_error"] {
        let builtin_bridge = bridge.clone();
        let builtin_level = level.strip_prefix("log_").unwrap_or(level).to_string();
        vm.register_builtin(level, move |args, _out| {
            let (message, fields) = log_message_and_fields(args);
            builtin_bridge.send_log(&builtin_level, &message, fields);
            Ok(harn_vm::VmValue::Nil)
        });

        let capability_bridge = bridge.clone();
        let capability_level = level.strip_prefix("log_").unwrap_or(level).to_string();
        vm.override_capability_method(
            harn_builtin_meta::CapabilityId::Observability,
            level,
            move |args, _out| {
                let (message, fields) = log_message_and_fields(args);
                capability_bridge.send_log(&capability_level, &message, fields);
                Ok(harn_vm::VmValue::Nil)
            },
        );
    }

    // The default `trace_end` writes to the VM's `out` buffer, which only
    // flushes when the pipeline completes. Override it so span ends stream
    // live — pipelines stuck in hot loops never reach the flush point and
    // timing data would otherwise be invisible when it matters.
    let b = bridge.clone();
    vm.register_builtin("trace_end", move |args, _out| {
        let (name, trace_id, span_id, duration_ms) =
            harn_vm::stdlib::tracing::finish_span_from_args(args)?;
        // Stamp timing into the human-readable message so formatters that
        // only surface `message` still show span name + duration.
        let message = format!("span_end {name} duration_ms={duration_ms}");
        let fields = serde_json::json!({
            "trace_id": trace_id,
            "span_id": span_id,
            "name": name,
            "duration_ms": duration_ms,
        });
        b.send_log("info", &message, Some(fields));
        Ok(harn_vm::VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("progress", move |args, _out| {
        let phase = args.first().map(|a| a.display()).unwrap_or_default();
        let message = args.get(1).map(|a| a.display()).unwrap_or_default();
        let progress_val = args.get(2).and_then(|a| a.as_int());
        let total_val = args.get(3).and_then(|a| a.as_int());
        let data = args.get(4).and_then(|a| {
            if matches!(a, harn_vm::VmValue::Nil) {
                None
            } else {
                Some(harn_vm::llm::vm_value_to_json(a))
            }
        });
        b.send_progress(&phase, &message, progress_val, total_val, data);
        Ok(harn_vm::VmValue::Nil)
    });

    // Product paths call `harness.runtime.emit_response` after capability
    // migration. Without a typed override that path goes through `host/call`
    // (and previously carried HOST_MUTATE), so ACP consumers lost the
    // assistant timeline entry that the ambient builtin projects. Keep the
    // ambient name as a legacy alias; both routes share `send_update`.
    let b = bridge.clone();
    vm.register_builtin("emit_response", move |args, _out| {
        b.send_update(&emit_response_text(args));
        Ok(harn_vm::VmValue::Nil)
    });
    let b = bridge.clone();
    vm.override_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "emit_response",
        move |args, _out| {
            b.send_update(&emit_response_text(args));
            Ok(harn_vm::VmValue::Nil)
        },
    );

    // exec/shell route through terminal/create + wait + output + release.
    for name in ["exec", "shell"] {
        vm.unregister_builtin(name);
    }

    let b = bridge.clone();
    let shell = selected_shell.clone();
    vm.register_async_builtin("exec", move |_ctx, args| {
        let bridge = b.clone();
        let shell = shell.clone();
        async move { acp_terminal_exec(&bridge, &args, &shell).await }
    });

    let b = bridge;
    let shell = selected_shell;
    vm.register_async_builtin("shell", move |_ctx, args| {
        let bridge = b.clone();
        let shell = shell.clone();
        async move { acp_terminal_exec(&bridge, &args, &shell).await }
    });
}

/// Execute a command through ACP terminal/create + wait_for_exit + output + release.
pub(super) async fn acp_terminal_exec(
    bridge: &AcpBridge,
    args: &[harn_vm::VmValue],
    shell: &harn_vm::VmValue,
) -> Result<harn_vm::VmValue, harn_vm::VmError> {
    let cmd = args.first().map(|a| a.display()).unwrap_or_default();
    if cmd.is_empty() {
        return Err(harn_vm::VmError::Thrown(harn_vm::VmValue::String(
            arcstr::ArcStr::from("exec: command is required"),
        )));
    }

    // If cancellation races terminal creation, Harn still needs the
    // terminal id so it can terminate the client-side process.
    let create_result = bridge
        .call_client_for_cleanup(
            "terminal/create",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "command": cmd,
                "shell": harn_vm::llm::vm_value_to_json(shell),
            }),
        )
        .await?;

    let terminal_id = create_result
        .get("terminalId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if bridge.cancellation.cancelled.load(Ordering::SeqCst) {
        kill_and_release_terminal(bridge, &terminal_id).await;
        return Err(harn_vm::VmError::Runtime("Cancelled".into()));
    }

    if terminal_id.is_empty() {
        // Client doesn't support terminal — fall back to local exec.
        let output = local_shell_exec(&cmd, shell).map_err(|e| {
            harn_vm::VmError::Thrown(harn_vm::VmValue::String(arcstr::ArcStr::from(format!(
                "exec failed: {e}"
            ))))
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "stdout".to_string(),
            harn_vm::VmValue::String(arcstr::ArcStr::from(stdout)),
        );
        map.insert(
            "stderr".to_string(),
            harn_vm::VmValue::String(arcstr::ArcStr::from(stderr)),
        );
        map.insert(
            "combined".to_string(),
            harn_vm::VmValue::String(arcstr::ArcStr::from(format!(
                "{}{}",
                map.get("stdout").map(|v| v.display()).unwrap_or_default(),
                map.get("stderr").map(|v| v.display()).unwrap_or_default()
            ))),
        );
        map.insert(
            "status".to_string(),
            harn_vm::VmValue::Int(exit_code as i64),
        );
        map.insert(
            "success".to_string(),
            harn_vm::VmValue::Bool(output.status.success()),
        );
        return Ok(harn_vm::VmValue::dict(map));
    }

    // wait_for_exit returns the stdout/stderr/combined/exitCode payload.
    let wait_result = match bridge
        .call_client(
            "terminal/wait_for_exit",
            terminal_params(&bridge.session_id, &terminal_id),
        )
        .await
    {
        Ok(result) => result,
        Err(error) if bridge.cancellation.cancelled.load(Ordering::SeqCst) => {
            kill_and_release_terminal(bridge, &terminal_id).await;
            return Err(error);
        }
        Err(_) => serde_json::json!({}),
    };

    // Usually empty since wait_for_exit already drained the pipes.
    let _output_result = bridge
        .call_client_for_cleanup(
            "terminal/output",
            terminal_params(&bridge.session_id, &terminal_id),
        )
        .await
        .unwrap_or(serde_json::json!({}));

    let output_result = wait_result;

    let _ = bridge
        .call_client_for_cleanup(
            "terminal/release",
            terminal_params(&bridge.session_id, &terminal_id),
        )
        .await;

    let output = harn_vm::bridge::json_result_to_vm_value(&output_result);
    if let harn_vm::VmValue::Dict(map) = &output {
        let mut normalized = (**map).clone();
        let stdout = normalized
            .get("stdout")
            .map(|v| v.display())
            .unwrap_or_default();
        let stderr = normalized
            .get("stderr")
            .map(|v| v.display())
            .unwrap_or_default();
        if !normalized.contains_key("combined") {
            normalized.insert(
                harn_vm::value::intern_key("combined"),
                harn_vm::VmValue::String(arcstr::ArcStr::from(format!("{stdout}{stderr}"))),
            );
        }
        if !normalized.contains_key("status") {
            let status = normalized
                .get("exit_code")
                .or_else(|| normalized.get("exitCode"))
                .and_then(|v| v.as_int())
                .unwrap_or(-1);
            normalized.insert(
                harn_vm::value::intern_key("status"),
                harn_vm::VmValue::Int(status),
            );
        }
        if !normalized.contains_key("success") {
            let success = normalized
                .get("status")
                .and_then(|v| v.as_int())
                .is_some_and(|code| code == 0);
            normalized.insert(
                harn_vm::value::intern_key("success"),
                harn_vm::VmValue::Bool(success),
            );
        }
        return Ok(harn_vm::VmValue::dict(normalized));
    }
    Ok(output)
}

fn terminal_params(session_id: &str, terminal_id: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session_id,
        "terminalId": terminal_id,
    })
}

async fn kill_and_release_terminal(bridge: &AcpBridge, terminal_id: &str) {
    if terminal_id.is_empty() {
        return;
    }
    let params = terminal_params(&bridge.session_id, terminal_id);
    let _ = bridge
        .call_client_for_cleanup("terminal/kill", params.clone())
        .await;
    let _ = bridge
        .call_client_for_cleanup("terminal/release", params)
        .await;
}

pub(super) fn normalize_host_capability_manifest(value: harn_vm::VmValue) -> harn_vm::VmValue {
    let Some(root) = value.as_dict() else {
        return harn_vm::VmValue::dict_map(Default::default());
    };

    let mut normalized = BTreeMap::new();
    for (capability, entry) in root.iter() {
        match entry {
            harn_vm::VmValue::Dict(dict) => {
                let mut normalized_entry = (**dict).clone();
                if !normalized_entry.contains_key("ops") {
                    if let Some(ops) =
                        operation_names_from_value(normalized_entry.get("operations"))
                    {
                        normalized_entry.insert(
                            harn_vm::value::intern_key("ops"),
                            harn_vm::VmValue::List(ops),
                        );
                    }
                }
                normalized.insert(capability.clone(), harn_vm::VmValue::dict(normalized_entry));
            }
            harn_vm::VmValue::List(list) => {
                let mut dict = BTreeMap::new();
                dict.insert("ops".to_string(), harn_vm::VmValue::List(list.clone()));
                normalized.insert(capability.clone(), harn_vm::VmValue::dict(dict));
            }
            _ => {}
        }
    }

    harn_vm::VmValue::dict(normalized)
}

pub(super) fn advertise_runtime_prompt_content(manifest: harn_vm::VmValue) -> harn_vm::VmValue {
    let mut root = manifest.as_dict().cloned().unwrap_or_default();
    let mut runtime = root
        .get("runtime")
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default();
    let mut operations = runtime
        .get("ops")
        .and_then(|value| match value {
            harn_vm::VmValue::List(values) => Some(values.clone()),
            _ => None,
        })
        .unwrap_or_else(|| Arc::new(Vec::new()));
    if !operations
        .iter()
        .any(|value| value.display() == "prompt_content")
    {
        Arc::make_mut(&mut operations).push(harn_vm::VmValue::String(arcstr::ArcStr::from(
            "prompt_content",
        )));
    }
    runtime.insert(
        harn_vm::value::intern_key("ops"),
        harn_vm::VmValue::List(operations),
    );
    root.insert(
        harn_vm::value::intern_key("runtime"),
        harn_vm::VmValue::dict(runtime),
    );
    harn_vm::VmValue::dict(root)
}

fn operation_names_from_value(
    value: Option<&harn_vm::VmValue>,
) -> Option<Arc<Vec<harn_vm::VmValue>>> {
    let value = value?;
    match value {
        harn_vm::VmValue::List(list) => Some(list.clone()),
        harn_vm::VmValue::Dict(dict) => Some(Arc::new(
            dict.keys()
                .map(|name| harn_vm::VmValue::String(name.clone()))
                .collect(),
        )),
        _ => None,
    }
}

fn manifest_has_operation(manifest: &harn_vm::VmValue, capability: &str, op: &str) -> bool {
    manifest
        .as_dict()
        .and_then(|root| root.get(capability))
        .and_then(|value| value.as_dict())
        .and_then(|capability| capability.get("ops"))
        .and_then(|ops| match ops {
            harn_vm::VmValue::List(list) => Some(list.iter().any(|item| item.display() == op)),
            _ => None,
        })
        .unwrap_or(false)
}

/// Cross-platform fallback shell exec used when the ACP client doesn't
/// expose a terminal capability. Uses the same selected-shell descriptor
/// carried on `terminal/create`.
fn local_shell_exec(cmd: &str, shell: &harn_vm::VmValue) -> std::io::Result<std::process::Output> {
    let mut params = harn_vm::value::DictMap::new();
    params.insert(
        harn_vm::value::intern_key("command"),
        harn_vm::VmValue::String(arcstr::ArcStr::from(cmd.to_string())),
    );
    params.insert(harn_vm::value::intern_key("shell"), shell.clone());
    let invocation =
        harn_vm::shells::resolve_invocation_from_vm_params(&params).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid shell invocation: {error}"),
            )
        })?;
    std::process::Command::new(invocation.program)
        .args(invocation.args)
        .output()
}
