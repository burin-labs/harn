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

use harn_vm::VmDictExt;
use harn_vm::VmValue;
use std::time::Duration;

use crate::error::HostlibError;
use crate::tools::payload::{
    optional_bool, optional_string, optional_string_list, optional_string_map, optional_timeout,
    optional_u64, parse_argv_program, require_dict_arg,
};
use crate::tools::proc::{self, CaptureConfig, EnvMode, SpawnRequest};

pub(crate) const NAME: &str = "hostlib_tools_run_command";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let (program, args_tail) = parse_command(&map)?;
    let cwd_raw = optional_string(NAME, &map, "cwd")?;
    let cwd = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let env = optional_string_map(NAME, &map, "env")?.unwrap_or_default();
    let stdin = optional_string(NAME, &map, "stdin")?;
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;
    let capture = parse_capture(&map)?;
    let env_mode = parse_env_mode(&map, !env.is_empty())?;
    let background = optional_bool(NAME, &map, "background")?
        .or(optional_bool(NAME, &map, "long_running")?)
        .unwrap_or(false);
    let background_after_ms = optional_u64(NAME, &map, "background_after_ms")?;
    let progress_interval_ms = optional_u64(NAME, &map, "progress_interval_ms")?;
    let progress_max_inline_bytes = optional_u64(NAME, &map, "progress_max_inline_bytes")?
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(capture.max_inline_bytes);
    let policy_context = match map.get("policy_context") {
        Some(VmValue::Dict(dict)) => Some((**dict).clone()),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: NAME,
                param: "policy_context",
                message: format!("expected dict, got {}", other.type_name()),
            });
        }
    };

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
                capture,
                session_id: session_id.clone(),
                progress_interval: progress_interval_ms.map(Duration::from_millis),
                progress_max_inline_bytes,
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
    ) {
        VmValue::Dict(map) => (*map).clone(),
        _ => harn_vm::value::DictMap::new(),
    };
    response.put_str("feedback_kind", "tool_progress");
    response.insert("duration_ms".to_string(), VmValue::Int(wait_ms as i64));
    response.put_str("stdout", inline_stdout);
    response.put_str("stderr", inline_stderr);
    response.insert("byte_count".to_string(), VmValue::Int(byte_count as i64));
    response.insert("line_count".to_string(), VmValue::Int(line_count as i64));
    VmValue::dict(response)
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
                        invocation.insert("shell".to_string(), shell.clone());
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
                invocation.insert("login".to_string(), VmValue::Bool(login));
            }
            if let Some(interactive) = optional_bool(NAME, map, "interactive")? {
                invocation.insert("interactive".to_string(), VmValue::Bool(interactive));
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

fn parse_env_mode(
    map: &harn_vm::value::DictMap,
    env_supplied: bool,
) -> Result<EnvMode, HostlibError> {
    match optional_string(NAME, map, "env_mode")?.as_deref() {
        Some("inherit_clean") => Ok(EnvMode::InheritClean),
        Some("replace") => Ok(EnvMode::Replace),
        Some("patch") => Ok(EnvMode::Patch),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "env_mode",
            message: format!(
                "unsupported env_mode {other:?}; expected inherit_clean, replace, or patch"
            ),
        }),
        None if env_supplied => Ok(EnvMode::Replace),
        None => Ok(EnvMode::InheritClean),
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
