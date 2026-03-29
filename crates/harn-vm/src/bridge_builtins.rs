//! Bridge-mode builtins that delegate to the host process via JSON-RPC.
//!
//! When `harn run --bridge` is active, these replace the normal builtins
//! for LLM calls, file I/O, tool execution, and output. Language builtins
//! (string ops, math, json, list ops) are NOT replaced.

use std::rc::Rc;

use crate::bridge::{json_result_to_vm_value, HostBridge};
use crate::value::VmValue;
use crate::vm::Vm;

/// Register all bridge-mode builtins on the VM.
///
/// This replaces: llm_call, agent_loop, llm_stream, log, print, println,
/// read_file, write_file, file_exists, delete_file, exec,
/// host_call, progress, and emit_response.
pub fn register_bridge_builtins(vm: &mut Vm, bridge: Rc<HostBridge>) {
    // Remove sync builtins that we're overriding with async bridge versions.
    // The VM checks sync builtins before async ones, so we must remove
    // the sync versions to let the async bridge versions take effect.
    for name in ["read_file", "write_file", "delete_file", "exec"] {
        vm.unregister_builtin(name);
    }

    // =========================================================================
    // Output builtins — redirect through JSON-RPC notifications
    // =========================================================================

    let b = bridge.clone();
    vm.register_builtin("log", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_output(&format!("[harn] {msg}\n"));
        Ok(VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("print", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_output(&msg);
        Ok(VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("println", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_output(&format!("{msg}\n"));
        Ok(VmValue::Nil)
    });

    // =========================================================================
    // Progress reporting
    // =========================================================================

    let b = bridge.clone();
    vm.register_builtin("progress", move |args, _out| {
        let phase = args.first().map(|a| a.display()).unwrap_or_default();
        let message = args.get(1).map(|a| a.display()).unwrap_or_default();
        b.send_progress(&phase, &message);
        Ok(VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("emit_response", move |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_output(&text);
        Ok(VmValue::Nil)
    });

    // Override structured logging builtins to send via bridge
    for level in ["log_debug", "log_info", "log_warn", "log_error"] {
        let b = bridge.clone();
        let lvl = level.strip_prefix("log_").unwrap_or(level).to_string();
        vm.register_builtin(level, move |args, _out| {
            let msg = args.first().map(|a| a.display()).unwrap_or_default();
            let fields = args.get(1).map(|a| a.display()).unwrap_or_default();
            if fields.is_empty() {
                b.send_output(&format!("[{lvl}] {msg}\n"));
            } else {
                b.send_output(&format!("[{lvl}] {msg} {fields}\n"));
            }
            Ok(VmValue::Nil)
        });
    }

    // =========================================================================
    // LLM builtins — delegate to host
    // =========================================================================

    let b = bridge.clone();
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            let system = args.get(1).map(|a| a.display());
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();

            let mut params = serde_json::json!({
                "prompt": prompt,
            });
            if let Some(sys) = system {
                params["system"] = serde_json::json!(sys);
            }
            if let Some(opts) = options {
                let opts_json = vm_value_dict_to_json(&opts);
                params["options"] = opts_json;
            }

            let result = bridge.call("llm_call", params).await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("agent_loop", move |args| {
        let bridge = b.clone();
        async move {
            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            let system = args.get(1).map(|a| a.display());
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();

            let mut params = serde_json::json!({
                "prompt": prompt,
            });
            if let Some(sys) = system {
                params["system"] = serde_json::json!(sys);
            }
            if let Some(opts) = options {
                let opts_json = vm_value_dict_to_json(&opts);
                params["options"] = opts_json;
            }

            let result = bridge.call("agent_loop", params).await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    // llm_stream in bridge mode: delegate to host, return the text result
    // (streaming happens between host and LLM, not between host and VM)
    let b = bridge.clone();
    vm.register_async_builtin("llm_stream", move |args| {
        let bridge = b.clone();
        async move {
            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            let system = args.get(1).map(|a| a.display());

            let mut params = serde_json::json!({"prompt": prompt});
            if let Some(sys) = system {
                params["system"] = serde_json::json!(sys);
            }

            let result = bridge.call("llm_call", params).await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    // =========================================================================
    // File I/O builtins — delegate to host via tool_execute
    // =========================================================================

    let b = bridge.clone();
    vm.register_async_builtin("read_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            let result = bridge
                .call(
                    "tool_execute",
                    serde_json::json!({"name": "read_file", "arguments": {"path": path}}),
                )
                .await?;
            // Return the content string
            if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                Ok(VmValue::String(Rc::from(content)))
            } else {
                Ok(json_result_to_vm_value(&result))
            }
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("write_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            let content = args.get(1).map(|a| a.display()).unwrap_or_default();
            bridge
                .call(
                    "tool_execute",
                    serde_json::json!({
                        "name": "write_file",
                        "arguments": {"path": path, "content": content},
                    }),
                )
                .await?;
            Ok(VmValue::Nil)
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("apply_edit", move |args| {
        let bridge = b.clone();
        async move {
            let file = args.first().map(|a| a.display()).unwrap_or_default();
            let old_str = args.get(1).map(|a| a.display()).unwrap_or_default();
            let new_str = args.get(2).map(|a| a.display()).unwrap_or_default();
            bridge
                .call(
                    "tool_execute",
                    serde_json::json!({
                        "name": "apply_edit",
                        "arguments": {"file": file, "old_str": old_str, "new_str": new_str},
                    }),
                )
                .await?;
            Ok(VmValue::Nil)
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("delete_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            bridge
                .call(
                    "tool_execute",
                    serde_json::json!({"name": "delete_file", "arguments": {"path": path}}),
                )
                .await?;
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("file_exists", |args, _out| {
        // file_exists is sync — we can't easily make it async, so send a
        // notification and let the host handle it. For now, delegate to local fs.
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(std::path::Path::new(&path).exists()))
    });

    // =========================================================================
    // Shell execution — delegate to host
    // =========================================================================

    let b = bridge.clone();
    vm.register_async_builtin("exec", move |args| {
        let bridge = b.clone();
        async move {
            let cmd = args.first().map(|a| a.display()).unwrap_or_default();
            let timeout = args.get(1).and_then(|a| a.as_int());
            let mut arguments = serde_json::json!({"command": cmd});
            if let Some(t) = timeout {
                arguments["timeout"] = serde_json::json!(t);
            }
            let result = bridge
                .call(
                    "tool_execute",
                    serde_json::json!({"name": "exec", "arguments": arguments}),
                )
                .await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("run_command", move |args| {
        let bridge = b.clone();
        async move {
            let cmd = args.first().map(|a| a.display()).unwrap_or_default();
            let timeout = args.get(1).and_then(|a| a.as_int());
            let mut arguments = serde_json::json!({"command": cmd});
            if let Some(t) = timeout {
                arguments["timeout"] = serde_json::json!(t);
            }
            let result = bridge
                .call(
                    "tool_execute",
                    serde_json::json!({"name": "run_command", "arguments": arguments}),
                )
                .await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    // =========================================================================
    // Host callback — generic escape hatch
    // =========================================================================

    let b = bridge.clone();
    vm.register_async_builtin("host_call", move |args| {
        let bridge = b.clone();
        async move {
            let name = args.first().map(|a| a.display()).unwrap_or_default();
            let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
            let args_json = crate::llm::vm_value_to_json(&call_args);
            let result = bridge
                .call(
                    "host_call",
                    serde_json::json!({"name": name, "args": args_json}),
                )
                .await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    // =========================================================================
    // Render — template rendering delegated to host
    // =========================================================================

    let b = bridge.clone();
    vm.register_async_builtin("render", move |args| {
        let bridge = b.clone();
        async move {
            let template = args.first().map(|a| a.display()).unwrap_or_default();
            let bindings = args.get(1).cloned().unwrap_or(VmValue::Nil);
            let bindings_json = crate::llm::vm_value_to_json(&bindings);
            let result = bridge
                .call(
                    "host_call",
                    serde_json::json!({
                        "name": "render",
                        "args": {"template": template, "bindings": bindings_json},
                    }),
                )
                .await?;
            Ok(json_result_to_vm_value(&result))
        }
    });

    // =========================================================================
    // ask_user — delegate to host (IDE shows modal, CLI reads stdin)
    // =========================================================================

    let b = bridge;
    vm.register_async_builtin("ask_user", move |args| {
        let bridge = b.clone();
        async move {
            let question = args.first().map(|a| a.display()).unwrap_or_default();
            let question_type = args.get(1).map(|a| a.display());
            let mut params =
                serde_json::json!({"name": "ask_user", "args": {"question": question}});
            if let Some(qt) = question_type {
                params["args"]["type"] = serde_json::json!(qt);
            }
            let result = bridge.call("host_call", params).await?;
            Ok(json_result_to_vm_value(&result))
        }
    });
}

/// Convert a VmValue BTreeMap to serde_json::Value.
fn vm_value_dict_to_json(dict: &std::collections::BTreeMap<String, VmValue>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(k.clone(), crate::llm::vm_value_to_json(v));
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_value_dict_to_json() {
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("name".to_string(), VmValue::String(Rc::from("test")));
        dict.insert("count".to_string(), VmValue::Int(42));
        dict.insert("active".to_string(), VmValue::Bool(true));

        let json = vm_value_dict_to_json(&dict);
        assert_eq!(json["name"], "test");
        assert_eq!(json["count"], 42);
        assert_eq!(json["active"], true);
    }

    #[test]
    fn test_vm_value_dict_to_json_nested() {
        let mut inner = std::collections::BTreeMap::new();
        inner.insert("key".to_string(), VmValue::String(Rc::from("value")));

        let mut dict = std::collections::BTreeMap::new();
        dict.insert("nested".to_string(), VmValue::Dict(Rc::new(inner)));
        dict.insert(
            "list".to_string(),
            VmValue::List(Rc::new(vec![VmValue::Int(1), VmValue::Int(2)])),
        );

        let json = vm_value_dict_to_json(&dict);
        assert_eq!(json["nested"]["key"], "value");
        assert_eq!(json["list"][0], 1);
        assert_eq!(json["list"][1], 2);
    }
}
