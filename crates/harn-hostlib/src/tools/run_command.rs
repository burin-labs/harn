//! `tools/run_command` — canonical command runner v2 with argv/shell modes,
//! sandboxed spawn, timeout, artifacts, and background handles.
//!
//! Schema: `schemas/tools/run_command.{request,response}.json`.
//!
//! Behavior:
//! - `argv` remains the recommended default, with no shell parsing.
//! - Shell execution is only available when callers explicitly set
//!   `mode: "shell"` and provide a `shell` object.
//! - `capture_stderr: false` collapses stderr into stdout instead of dropping
//!   it.
//! - There is no implicit cap of 300s on `timeout_ms`; the caller decides.
//!   Sandboxing limits the blast radius regardless.
//! - `background: true` (or legacy `long_running: true`) spawns without waiting
//!   and returns a handle dict
//!   immediately. The result arrives via `agent_inject_feedback` when the
//!   process exits. See `tools/long_running.rs`.

use harn_vm::VmValue;

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
    let cwd = proc::parse_cwd(NAME, payload_str(&map, "cwd").as_deref())?;
    let env = optional_string_map(NAME, &map, "env")?.unwrap_or_default();
    let stdin = payload_str(&map, "stdin");
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;
    let capture = parse_capture(&map)?;
    let env_mode = parse_env_mode(&map, !env.is_empty())?;
    let background = optional_bool(NAME, &map, "background")?
        .or(optional_bool(NAME, &map, "long_running")?)
        .unwrap_or(false);
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

    if background {
        let session_id = harn_vm::current_agent_session_id().unwrap_or_default();
        let info = super::long_running::spawn_long_running_with_options(
            NAME, program, args_tail, cwd, env, env_mode, capture, session_id,
        )?;
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

fn payload_str(map: &std::collections::BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match map.get(key)? {
        VmValue::String(s) => Some(s.to_string()),
        VmValue::Nil => None,
        _ => None,
    }
}

fn parse_command(
    map: &std::collections::BTreeMap<String, VmValue>,
) -> Result<(String, Vec<String>), HostlibError> {
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
            let shell = match map.get("shell") {
                Some(VmValue::Dict(shell)) => shell,
                Some(other) => {
                    return Err(HostlibError::InvalidParameter {
                        builtin: NAME,
                        param: "shell",
                        message: format!("expected dict, got {}", other.type_name()),
                    });
                }
                None => {
                    return Err(HostlibError::MissingParameter {
                        builtin: NAME,
                        param: "shell",
                    });
                }
            };
            let path = shell_string(shell, "path")?
                .or_else(|| shell_string(shell, "id").ok().flatten())
                .ok_or(HostlibError::MissingParameter {
                    builtin: NAME,
                    param: "shell.path",
                })?;
            let login = shell_bool(shell, "login")?.unwrap_or(false);
            let interactive = shell_bool(shell, "interactive")?.unwrap_or(false);
            Ok(shell_argv(path, command, login, interactive))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "mode",
            message: format!("unsupported command mode {other:?}; expected argv or shell"),
        }),
    }
}

fn parse_env_mode(
    map: &std::collections::BTreeMap<String, VmValue>,
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

fn parse_capture(
    map: &std::collections::BTreeMap<String, VmValue>,
) -> Result<CaptureConfig, HostlibError> {
    let mut capture = CaptureConfig::default();
    if let Some(capture_value) = map.get("capture") {
        match capture_value {
            VmValue::Dict(dict) => {
                capture.stdout = dict_bool(dict, "stdout")?.unwrap_or(true);
                capture.stderr = dict_bool(dict, "stderr")?.unwrap_or(true);
                capture.merge_stderr = dict_bool(dict, "merge_stderr")?.unwrap_or(false);
                if let Some(bytes) = dict_u64(dict, "max_inline_bytes")? {
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

fn shell_argv(
    shell_path_or_id: String,
    command: String,
    login: bool,
    interactive: bool,
) -> (String, Vec<String>) {
    let program = match shell_path_or_id.as_str() {
        "sh" if cfg!(windows) => "cmd".to_string(),
        "sh" => "/bin/sh".to_string(),
        "bash" => "/bin/bash".to_string(),
        "zsh" => "/bin/zsh".to_string(),
        "cmd" => "cmd".to_string(),
        "powershell" => "powershell".to_string(),
        other => other.to_string(),
    };
    if cfg!(windows) && program.eq_ignore_ascii_case("cmd") {
        return (program, vec!["/C".to_string(), command]);
    }
    let mut args = Vec::new();
    if interactive {
        args.push("-i".to_string());
    }
    args.push(if login { "-lc" } else { "-c" }.to_string());
    args.push(command);
    (program, args)
}

fn shell_string(
    dict: &std::rc::Rc<std::collections::BTreeMap<String, VmValue>>,
    key: &'static str,
) -> Result<Option<String>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: key,
            message: format!("expected string, got {}", other.type_name()),
        }),
    }
}

fn shell_bool(
    dict: &std::rc::Rc<std::collections::BTreeMap<String, VmValue>>,
    key: &'static str,
) -> Result<Option<bool>, HostlibError> {
    dict_bool(dict, key)
}

fn dict_bool(
    dict: &std::collections::BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<bool>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: key,
            message: format!("expected bool, got {}", other.type_name()),
        }),
    }
}

fn dict_u64(
    dict: &std::collections::BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(i)) if *i >= 0 => Ok(Some(*i as u64)),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: key,
            message: format!("expected non-negative integer, got {}", other.type_name()),
        }),
    }
}
