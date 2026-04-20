//! ACP builtin registrations and terminal-exec glue.
//!
//! These builtins delegate VM-side capabilities (`log`, `print`, `exec`,
//! `host_call`, ...) to the ACP client via the `AcpBridge`.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::AcpBridge;

/// Register builtins that delegate to the ACP client (editor).
pub(super) async fn register_acp_builtins(vm: &mut harn_vm::Vm, bridge: Rc<AcpBridge>) {
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
        .unwrap_or_else(|_| harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::new())));

    let b = bridge.clone();
    vm.register_builtin("log", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&format!("[harn] {msg}\n"));
        Ok(harn_vm::VmValue::Nil)
    });

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

    let b = bridge.clone();
    vm.register_async_builtin("host_call", move |args| {
        let bridge = b.clone();
        async move {
            let name = args.first().map(|a| a.display()).unwrap_or_default();
            let call_args = args.get(1).cloned().unwrap_or(harn_vm::VmValue::Nil);
            let args_json = harn_vm::llm::vm_value_to_json(&call_args);
            let result = bridge
                .call_client(
                    "host/call",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "name": name,
                        "args": args_json,
                    }),
                )
                .await?;
            Ok(harn_vm::bridge::json_result_to_vm_value(&result))
        }
    });

    let host_capabilities_cache = host_capability_manifest.clone();
    vm.register_builtin("host_capabilities", move |_args, _out| {
        Ok(host_capabilities_cache.clone())
    });

    let host_has_cache = host_capability_manifest.clone();
    vm.register_builtin("host_has", move |args, _out| {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let op = args.get(1).map(|a| a.display());
        let valid = if let Some(manifest) = host_has_cache.as_dict() {
            if let Some(value) = manifest.get(&capability) {
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
    vm.register_async_builtin("run_command", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });

    for level in ["log_debug", "log_info", "log_warn", "log_error"] {
        let b = bridge.clone();
        let lvl = level.strip_prefix("log_").unwrap_or(level).to_string();
        vm.register_builtin(level, move |args, _out| {
            let msg = args.first().map(|a| a.display()).unwrap_or_default();
            let fields = args.get(1).and_then(|a| {
                if matches!(a, harn_vm::VmValue::Nil) {
                    None
                } else {
                    Some(harn_vm::llm::vm_value_to_json(a))
                }
            });
            b.send_log(&lvl, &msg, fields);
            Ok(harn_vm::VmValue::Nil)
        });
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

    let b = bridge.clone();
    vm.register_builtin("emit_response", move |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&text);
        Ok(harn_vm::VmValue::Nil)
    });

    // exec/shell route through terminal/create + wait + output + release.
    for name in ["exec", "shell"] {
        vm.unregister_builtin(name);
    }

    let b = bridge.clone();
    vm.register_async_builtin("exec", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });

    let b = bridge;
    vm.register_async_builtin("shell", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });
}

/// Execute a command through ACP terminal/create + wait_for_exit + output + release.
pub(super) async fn acp_terminal_exec(
    bridge: &AcpBridge,
    args: &[harn_vm::VmValue],
) -> Result<harn_vm::VmValue, harn_vm::VmError> {
    let cmd = args.first().map(|a| a.display()).unwrap_or_default();
    if cmd.is_empty() {
        return Err(harn_vm::VmError::Thrown(harn_vm::VmValue::String(
            Rc::from("exec: command is required"),
        )));
    }

    let create_result = bridge
        .call_client(
            "terminal/create",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "command": cmd,
            }),
        )
        .await?;

    let terminal_id = create_result
        .get("terminalId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if terminal_id.is_empty() {
        // Client doesn't support terminal — fall back to local exec.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .map_err(|e| {
                harn_vm::VmError::Thrown(harn_vm::VmValue::String(Rc::from(format!(
                    "exec failed: {e}"
                ))))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "stdout".to_string(),
            harn_vm::VmValue::String(Rc::from(stdout)),
        );
        map.insert(
            "stderr".to_string(),
            harn_vm::VmValue::String(Rc::from(stderr)),
        );
        map.insert(
            "combined".to_string(),
            harn_vm::VmValue::String(Rc::from(format!(
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
        return Ok(harn_vm::VmValue::Dict(Rc::new(map)));
    }

    // wait_for_exit returns the stdout/stderr/combined/exitCode payload.
    let wait_result = bridge
        .call_client(
            "terminal/wait_for_exit",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await
        .unwrap_or(serde_json::json!({}));

    // Usually empty since wait_for_exit already drained the pipes.
    let _output_result = bridge
        .call_client(
            "terminal/output",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await
        .unwrap_or(serde_json::json!({}));

    let output_result = wait_result;

    let _ = bridge
        .call_client(
            "terminal/release",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
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
                "combined".to_string(),
                harn_vm::VmValue::String(Rc::from(format!("{stdout}{stderr}"))),
            );
        }
        if !normalized.contains_key("status") {
            let status = normalized
                .get("exit_code")
                .or_else(|| normalized.get("exitCode"))
                .and_then(|v| v.as_int())
                .unwrap_or(-1);
            normalized.insert("status".to_string(), harn_vm::VmValue::Int(status));
        }
        if !normalized.contains_key("success") {
            let success = normalized
                .get("status")
                .and_then(|v| v.as_int())
                .is_some_and(|code| code == 0);
            normalized.insert("success".to_string(), harn_vm::VmValue::Bool(success));
        }
        return Ok(harn_vm::VmValue::Dict(Rc::new(normalized)));
    }
    Ok(output)
}
pub(super) fn normalize_host_capability_manifest(value: harn_vm::VmValue) -> harn_vm::VmValue {
    let Some(root) = value.as_dict() else {
        return harn_vm::VmValue::Dict(Rc::new(BTreeMap::new()));
    };

    let mut normalized = BTreeMap::new();
    for (capability, entry) in root.iter() {
        match entry {
            harn_vm::VmValue::Dict(_) => {
                normalized.insert(capability.clone(), entry.clone());
            }
            harn_vm::VmValue::List(list) => {
                let mut dict = BTreeMap::new();
                dict.insert("ops".to_string(), harn_vm::VmValue::List(list.clone()));
                normalized.insert(capability.clone(), harn_vm::VmValue::Dict(Rc::new(dict)));
            }
            _ => {}
        }
    }

    harn_vm::VmValue::Dict(Rc::new(normalized))
}
