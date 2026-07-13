//! `tools/run_command` — canonical command runner v2 with argv/shell modes,
//! sandboxed spawn, timeout, artifacts, and background handles.
//!
//! Schema: `schemas/tools/run_command.{request,response}.json`.
//!
//! Behavior:
//! - `argv` remains the recommended default, with no shell parsing.
//! - Shell execution is available when callers explicitly set
//!   `mode: "shell"`; callers can provide a `shell` object or `shell_id`,
//!   otherwise the host default shell is used.
//! - `capture_stderr: false` collapses stderr into stdout instead of dropping
//!   it.
//! - There is no implicit cap of 300s on `timeout_ms`; the caller decides.
//!   Sandboxing limits the blast radius regardless.
//! - `background: true` (or legacy `long_running: true`) spawns without waiting
//!   and returns a handle dict
//!   immediately. The result arrives via `agent_inject_feedback` when the
//!   process exits. See `tools/long_running.rs`.

use harn_vm::process_sandbox::ProcessSandboxScope;
use harn_vm::VmDictExt;
use harn_vm::VmValue;
use std::time::Duration;

use crate::error::HostlibError;
use crate::tools::payload::{
    optional_bool, optional_dict, optional_env_mode, optional_string, optional_string_list,
    optional_string_map, optional_timeout, optional_u64, parse_argv_program, require_dict_arg,
};
use crate::tools::proc::{self, CaptureConfig, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_run_command";

/// Project the VM command-policy denial into the public run-command contract.
///
/// The VM envelope intentionally carries compatibility fields for
/// `host_call("process.exec", ...)`; exposing that envelope here would both
/// omit required hostlib fields and leak properties forbidden by the hostlib
/// response schema.
pub(crate) fn policy_blocked_response(response: VmValue) -> VmValue {
    let map = response.as_dict();
    let command_id = map
        .and_then(|value| dict_string(value, "command_id"))
        .unwrap_or_else(proc::next_command_id);
    let status = map
        .and_then(|value| dict_string(value, "status"))
        .unwrap_or_else(|| "blocked".to_string());
    let started_at = map
        .and_then(|value| dict_string(value, "started_at"))
        .unwrap_or_else(proc::now_rfc3339);
    let ended_at = map
        .and_then(|value| dict_string(value, "ended_at"))
        .unwrap_or_else(|| started_at.clone());
    let message = map
        .and_then(|value| dict_string(value, "stderr"))
        .unwrap_or_default();
    let audit_id = map
        .and_then(|value| dict_string(value, "audit_id"))
        .unwrap_or_else(|| format!("audit_{command_id}"));

    let mut sandbox = harn_vm::value::DictMap::new();
    sandbox.put_str("kind", proc::sandbox_kind());
    sandbox.insert(
        harn_vm::value::intern_key("enforced"),
        VmValue::Bool(proc::sandbox_enforced()),
    );

    let mut builder = ResponseBuilder::new()
        .str("command_id", command_id)
        .str("status", status)
        .nil("pid")
        .nil("process_group_id")
        .nil("handle_id")
        .str("started_at", started_at)
        .str("ended_at", ended_at)
        .int("duration_ms", 0)
        .int("exit_code", -1)
        .nil("signal")
        .bool("timed_out", false)
        .str("stdout", "")
        .str("stderr", message.clone())
        .str("output_path", "")
        .str("stdout_path", "")
        .str("stderr_path", "")
        .int("line_count", message.lines().count() as i64)
        .int("byte_count", message.len() as i64)
        .str("output_sha256", "")
        .dict("sandbox", sandbox)
        .str("audit_id", audit_id);
    if let Some(policy) = map.and_then(|value| value.get("command_policy")).cloned() {
        builder = builder.value("command_policy", policy);
    }
    builder.build()
}

fn dict_string(map: &harn_vm::value::DictMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn request_is_background(map: &harn_vm::value::DictMap) -> bool {
    matches!(map.get("background"), Some(VmValue::Bool(true)))
        || matches!(map.get("long_running"), Some(VmValue::Bool(true)))
        || map
            .get("background_after_ms")
            .is_some_and(|value| !matches!(value, VmValue::Nil))
}

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let (program, args_tail) = parse_command(&map)?;
    let cwd_raw = optional_string(NAME, &map, "cwd")?;
    let cwd = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let env = optional_string_map(NAME, &map, "env")?.unwrap_or_default();
    let env_remove = optional_string_list(NAME, &map, "env_remove")?.unwrap_or_default();
    let stdin = optional_string(NAME, &map, "stdin")?;
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;
    let capture = parse_capture(&map)?;
    let sandbox_scope = parse_sandbox_scope(&map)?;
    let env_mode = optional_env_mode(NAME, &map, !env.is_empty())?;
    let background = optional_bool(NAME, &map, "background")?
        .or(optional_bool(NAME, &map, "long_running")?)
        .unwrap_or(false);
    let background_after_ms = optional_u64(NAME, &map, "background_after_ms")?;
    let progress_interval_ms = optional_u64(NAME, &map, "progress_interval_ms")?;
    let progress_max_inline_bytes = optional_u64(NAME, &map, "progress_max_inline_bytes")?
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(capture.max_inline_bytes);
    let policy_context = optional_dict(NAME, &map, "policy_context")?;
    let snapshot_binding = optional_dict(NAME, &map, "snapshot_binding")?;

    let _sandbox_guard = harn_vm::process_sandbox::push_process_sandbox_scope(sandbox_scope)
        .map_err(sandbox_scope_error_to_hostlib)?;
    if background || background_after_ms.is_some() {
        let session_id = harn_vm::current_agent_session_id().unwrap_or_default();
        let info = super::long_running::spawn_long_running_with_options(
            NAME,
            program,
            args_tail,
            cwd,
            env,
            super::long_running::LongRunningSpawnOptions {
                env_mode,
                env_remove,
                capture,
                session_id: session_id.clone(),
                progress_interval: progress_interval_ms.map(Duration::from_millis),
                progress_max_inline_bytes,
                snapshot_binding,
            },
        )?;
        if let Some(wait_ms) = background_after_ms.filter(|wait_ms| *wait_ms > 0) {
            if let Some(progress) =
                wait_for_initial_background_feedback(&session_id, &info.handle_id, wait_ms)
            {
                return Ok(progress);
            }
            return Ok(initial_background_snapshot(
                &info,
                wait_ms,
                progress_max_inline_bytes,
            ));
        }
        return Ok(info.into_handle_response());
    }

    let outcome = proc::run(SpawnRequest {
        builtin: NAME,
        program,
        args: args_tail,
        cwd,
        env,
        env_remove,
        env_mode,
        stdin,
        timeout,
        capture,
    })?;

    Ok(proc::build_response(outcome, None, policy_context))
}

fn wait_for_initial_background_feedback(
    session_id: &str,
    handle_id: &str,
    wait_ms: u64,
) -> Option<VmValue> {
    let timeout = Duration::from_millis(wait_ms);
    if !harn_vm::orchestration::agent_inbox::wait_sync(session_id, timeout) {
        return None;
    }
    let mut kept = Vec::new();
    let mut selected = None;
    for entry in harn_vm::orchestration::agent_inbox::drain(session_id) {
        let parsed = serde_json::from_str::<serde_json::Value>(&entry.content).ok();
        let matches_handle = parsed
            .as_ref()
            .and_then(|value| value.get("handle_id"))
            .and_then(serde_json::Value::as_str)
            == Some(handle_id);
        if matches_handle && selected.is_none() {
            if let Some(mut payload) = parsed {
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "feedback_kind".to_string(),
                        serde_json::Value::String(entry.kind.clone()),
                    );
                }
                selected = Some(harn_vm::json_to_vm_value(&payload));
                continue;
            }
        }
        kept.push(entry);
    }
    for entry in kept.into_iter().rev() {
        harn_vm::orchestration::agent_inbox::requeue_front(entry);
    }
    selected
}

fn initial_background_snapshot(
    info: &super::long_running::LongRunningHandleInfo,
    wait_ms: u64,
    max_inline_bytes: usize,
) -> VmValue {
    let artifacts = super::proc::planned_artifact_paths(&info.command_id);
    let stdout = std::fs::read(&artifacts.stdout_path).unwrap_or_default();
    let stderr = std::fs::read(&artifacts.stderr_path).unwrap_or_default();
    let capture = super::proc::CaptureConfig {
        max_inline_bytes,
        ..super::proc::CaptureConfig::default()
    };
    let (inline_stdout, inline_stderr) = super::proc::inline_output(&stdout, &stderr, capture);
    let line_count = stdout
        .iter()
        .chain(stderr.iter())
        .filter(|byte| **byte == b'\n')
        .count();
    let byte_count = stdout.len().saturating_add(stderr.len());
    let mut response = match super::proc::running_response(
        info.command_id.clone(),
        info.handle_id.clone(),
        info.pid,
        info.process_group_id,
        info.started_at.clone(),
        info.command_display.clone(),
        info.snapshot_binding.as_ref(),
    ) {
        VmValue::Dict(map) => (*map).clone(),
        _ => harn_vm::value::DictMap::new(),
    };
    response.put_str("feedback_kind", "tool_progress");
    response.insert(
        harn_vm::value::intern_key("duration_ms"),
        VmValue::Int(wait_ms as i64),
    );
    response.put_str("stdout", inline_stdout);
    response.put_str("stderr", inline_stderr);
    response.insert(
        harn_vm::value::intern_key("byte_count"),
        VmValue::Int(byte_count as i64),
    );
    response.insert(
        harn_vm::value::intern_key("line_count"),
        VmValue::Int(line_count as i64),
    );
    VmValue::dict(response)
}

fn parse_sandbox_scope(map: &harn_vm::value::DictMap) -> Result<ProcessSandboxScope, HostlibError> {
    let Some(value) = map.get("sandbox") else {
        return Ok(ProcessSandboxScope::default());
    };
    match value {
        VmValue::Nil => Ok(ProcessSandboxScope::default()),
        VmValue::Dict(dict) => Ok(ProcessSandboxScope {
            workspace_roots: optional_nested_string_list(dict, "workspace_roots")?
                .unwrap_or_default(),
        }),
        other => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "sandbox",
            message: format!("expected dict, got {}", other.type_name()),
        }),
    }
}

fn optional_nested_string_list(
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<Vec<String>>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::List(values) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values.iter() {
                match value {
                    VmValue::String(string) => out.push(string.to_string()),
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin: NAME,
                            param: key,
                            message: format!(
                                "expected list of strings, got {} element",
                                other.type_name()
                            ),
                        });
                    }
                }
            }
            Ok(Some(out))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: key,
            message: format!("expected list of strings, got {}", other.type_name()),
        }),
    }
}

fn sandbox_scope_error_to_hostlib(error: harn_vm::VmError) -> HostlibError {
    match error {
        harn_vm::VmError::CategorizedError {
            message,
            category: harn_vm::value::ErrorCategory::ToolRejected,
        } => HostlibError::SandboxViolation {
            builtin: NAME,
            path: String::new(),
            message,
        },
        other => HostlibError::Backend {
            builtin: NAME,
            message: other.to_string(),
        },
    }
}

fn parse_command(map: &harn_vm::value::DictMap) -> Result<(String, Vec<String>), HostlibError> {
    match optional_string(NAME, map, "mode")?
        .as_deref()
        .unwrap_or("argv")
    {
        "argv" => {
            let argv =
                optional_string_list(NAME, map, "argv")?.ok_or(HostlibError::MissingParameter {
                    builtin: NAME,
                    param: "argv",
                })?;
            parse_argv_program(NAME, argv)
        }
        "shell" => {
            let command =
                optional_string(NAME, map, "command")?.ok_or(HostlibError::MissingParameter {
                    builtin: NAME,
                    param: "command",
                })?;
            let mut invocation = harn_vm::value::DictMap::new();
            invocation.put_str("command", command);
            if let Some(shell_id) = optional_string(NAME, map, "shell_id")? {
                invocation.put_str("shell_id", shell_id);
            }
            if let Some(shell) = map.get("shell") {
                match shell {
                    VmValue::Dict(_) => {
                        invocation.insert(harn_vm::value::intern_key("shell"), shell.clone());
                    }
                    VmValue::Nil => {}
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin: NAME,
                            param: "shell",
                            message: format!("expected dict, got {}", other.type_name()),
                        });
                    }
                }
            }
            if let Some(login) = optional_bool(NAME, map, "login")? {
                invocation.insert(harn_vm::value::intern_key("login"), VmValue::Bool(login));
            }
            if let Some(interactive) = optional_bool(NAME, map, "interactive")? {
                invocation.insert(
                    harn_vm::value::intern_key("interactive"),
                    VmValue::Bool(interactive),
                );
            }
            let resolved = harn_vm::shells::resolve_invocation_from_vm_params(&invocation)
                .map_err(|message| HostlibError::InvalidParameter {
                    builtin: NAME,
                    param: "shell",
                    message,
                })?;
            Ok((resolved.program, resolved.args))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "mode",
            message: format!("unsupported command mode {other:?}; expected argv or shell"),
        }),
    }
}

fn parse_capture(map: &harn_vm::value::DictMap) -> Result<CaptureConfig, HostlibError> {
    let mut capture = CaptureConfig::default();
    if let Some(capture_value) = map.get("capture") {
        match capture_value {
            VmValue::Dict(dict) => {
                capture.stdout = optional_bool(NAME, dict, "stdout")?.unwrap_or(true);
                capture.stderr = optional_bool(NAME, dict, "stderr")?.unwrap_or(true);
                capture.merge_stderr = optional_bool(NAME, dict, "merge_stderr")?.unwrap_or(false);
                if let Some(bytes) = optional_u64(NAME, dict, "max_inline_bytes")? {
                    capture.max_inline_bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
                }
            }
            VmValue::Nil => {}
            other => {
                return Err(HostlibError::InvalidParameter {
                    builtin: NAME,
                    param: "capture",
                    message: format!("expected dict, got {}", other.type_name()),
                });
            }
        }
    }
    if optional_bool(NAME, map, "capture_stderr")?.is_some_and(|capture_stderr| !capture_stderr) {
        capture.merge_stderr = true;
        capture.stderr = false;
    }
    if let Some(max) = optional_u64(NAME, map, "max_inline_bytes")? {
        capture.max_inline_bytes = usize::try_from(max).unwrap_or(usize::MAX);
    }
    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_blocked_response_matches_public_schema() {
        let params = harn_vm::value::DictMap::new();
        let generic = harn_vm::orchestration::blocked_command_response(
            &params,
            "consent_denied",
            "operator declined",
            serde_json::json!({"caller": {"surface": "hostlib"}}),
            Vec::new(),
        );
        let response = policy_blocked_response(generic);
        let schema =
            crate::schemas::lookup("tools", "run_command", crate::schemas::SchemaKind::Response)
                .expect("run_command response schema");
        let schema: serde_json::Value = serde_json::from_str(schema).expect("valid schema JSON");
        let schema = harn_vm::schema::json_to_vm_value(&schema);

        harn_vm::schema::validate_value_against_schema(&response, &schema, false)
            .expect("blocked response must satisfy the public hostlib schema");
        let response = response.as_dict().expect("dict response");
        assert_eq!(
            dict_string(response, "status").as_deref(),
            Some("consent_denied")
        );
        assert!(response.get("command_policy").is_some());
        assert!(response.get("request").is_none());
    }

    #[test]
    fn background_detection_matches_public_request_modes() {
        for key in ["background", "long_running"] {
            let mut request = harn_vm::value::DictMap::new();
            request.insert(harn_vm::value::intern_key(key), VmValue::Bool(true));
            assert!(request_is_background(&request));
        }
        let mut delayed = harn_vm::value::DictMap::new();
        delayed.insert(
            harn_vm::value::intern_key("background_after_ms"),
            VmValue::Int(0),
        );
        assert!(request_is_background(&delayed));
        assert!(!request_is_background(&harn_vm::value::DictMap::new()));
    }
}
