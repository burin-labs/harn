//! `tools/run_command` — spawn an arbitrary argv with sandbox + timeout +
//! stdout/stderr capture.
//!
//! Schema: `schemas/tools/run_command.{request,response}.json`.
//!
//! Divergence vs. Swift `CoreToolExecutor.runCommand`:
//! - Input is `argv: [String]` (no shell parsing). The Swift implementation
//!   accepted a shell `command: String` and routed through `ShellPathResolver`.
//!   We require argv to eliminate the shell-injection surface; callers that
//!   genuinely need a shell can pass `["sh", "-c", ...]` themselves.
//! - `capture_stderr: false` collapses stderr into stdout instead of dropping
//!   it (matches what Swift returned to LLMs).
//! - There is no implicit cap of 300s on `timeout_ms`; the caller decides.
//!   Sandboxing limits the blast radius regardless.
//! - `long_running: true` spawns without waiting and returns a handle dict
//!   immediately. The result arrives via `agent_inject_feedback` when the
//!   process exits. See `tools/long_running.rs`.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::payload::{
    optional_bool, optional_string_map, optional_timeout, parse_argv_program, require_argv,
    require_dict_arg,
};
use crate::tools::proc::{self, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_run_command";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let argv = require_argv(NAME, &map)?;
    let (program, args_tail) = parse_argv_program(NAME, argv)?;
    let cwd = proc::parse_cwd(NAME, payload_str(&map, "cwd").as_deref())?;
    let env = optional_string_map(NAME, &map, "env")?.unwrap_or_default();
    let stdin = payload_str(&map, "stdin");
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;
    let capture_stderr = optional_bool(NAME, &map, "capture_stderr")?.unwrap_or(true);
    let long_running = optional_bool(NAME, &map, "long_running")?.unwrap_or(false);

    if long_running {
        let session_id = harn_vm::current_agent_session_id().unwrap_or_default();
        let info = super::long_running::spawn_long_running(
            NAME, program, args_tail, cwd, env, session_id,
        )?;
        return Ok(info.into_handle_response());
    }

    let outcome = proc::run(SpawnRequest {
        builtin: NAME,
        program,
        args: args_tail,
        cwd,
        env,
        stdin,
        timeout,
    })?;

    let (stdout, stderr) = if capture_stderr {
        (outcome.stdout, outcome.stderr)
    } else {
        let mut merged = outcome.stdout;
        if !outcome.stderr.is_empty() {
            if !merged.is_empty() && !merged.ends_with('\n') {
                merged.push('\n');
            }
            merged.push_str(&outcome.stderr);
        }
        (merged, String::new())
    };

    Ok(ResponseBuilder::new()
        .int("exit_code", outcome.exit_code as i64)
        .opt_str("signal", outcome.signal)
        .str("stdout", stdout)
        .str("stderr", stderr)
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .bool("timed_out", outcome.timed_out)
        .build())
}

fn payload_str(map: &std::collections::BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match map.get(key)? {
        VmValue::String(s) => Some(s.to_string()),
        VmValue::Nil => None,
        _ => None,
    }
}
