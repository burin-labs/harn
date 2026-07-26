//! Process execution for `host_call("process", …)`.
//!
//! The seam with [`super::process_dispatch`] is policy vs. execution: that
//! module decides *whether* a command may run (deny-patterns, approval gating,
//! sandbox selection) and this one *runs* it — spawning, pumping pipes,
//! enforcing timeouts, and shaping the response value.
//!
//! Split out of `host.rs`, which had grown past the source-length ratchet's
//! cap; the code is unchanged by the move.

use std::time::Instant;

use serde_json::Value as JsonValue;
use tokio::io::AsyncReadExt;

use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::{
    async_builtin_cancel_token, audited_utc_now_rfc3339, optional_i64, optional_string,
    optional_string_dict, optional_string_list, require_param,
};

/// Apply the command-policy preflight (deny-patterns, approval gating,
/// sandbox decisions) and then spawn the process non-blocking. Mirrors
/// [`dispatch_process_exec_with_policy`] so spawn is gated identically to
/// exec. There is no postflight here: spawn returns a handle immediately,
/// not a completed command result; completion is observed later via
/// poll/wait, which are not themselves command executions.
pub(super) async fn dispatch_process_spawn_with_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    caller: serde_json::Value,
) -> Result<VmValue, VmError> {
    let params =
        match crate::orchestration::run_command_policy_preflight_with_ctx(ctx, params, caller)
            .await?
        {
            crate::orchestration::CommandPolicyPreflight::Proceed { params, .. } => params,
            crate::orchestration::CommandPolicyPreflight::Blocked {
                status,
                message,
                context,
                decisions,
            } => {
                return Ok(crate::orchestration::blocked_command_response(
                    params, status, &message, context, decisions,
                ));
            }
        };

    match crate::stdlib::process_spawn::dispatch("spawn", &params, async_builtin_cancel_token(ctx))
        .await
    {
        Some(result) => result,
        None => Err(VmError::Runtime(
            "host_call process.spawn: dispatch returned None".to_string(),
        )),
    }
}

pub(super) async fn dispatch_process_exec_after_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    command_policy_context: JsonValue,
    command_policy_decisions: Vec<crate::orchestration::CommandPolicyDecision>,
) -> Result<VmValue, VmError> {
    let timeout_ms = optional_i64(params, "timeout")
        .or_else(|| optional_i64(params, "timeout_ms"))
        .filter(|value| *value > 0)
        .map(|value| value as u64);
    // Optional per-call profile override. Pipelines that want to
    // promote a single spawn to `os_hardened` (e.g. running
    // attacker-controlled code) pass `sandbox_profile: "os_hardened"`
    // without having to rewrite the surrounding policy. The override
    // is scoped to this call and pops with the guard at end-of-scope.
    let profile_guard = match optional_string(params, "sandbox_profile") {
        Some(value) => Some(push_sandbox_profile_override(&value)?),
        None => None,
    };
    let mut cmd = build_sandboxed_command(params, "process.exec")?;
    crate::op_interrupt::configure_tokio_kill_group(&mut cmd);
    let cleanup_token = crate::op_interrupt::new_process_cleanup_token();
    cmd.env(
        crate::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let started_at = audited_utc_now_rfc3339("host_call/process.exec.started_at");
    let started = crate::clock_mock::leak_audit::instant_now("host_call/process.exec.started");
    let mut child = cmd
        .spawn()
        .map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
    drop(profile_guard);
    let pid = child.id();
    let cleanup_registration = crate::op_interrupt::register_active_process_cleanup(
        pid,
        &cleanup_token,
        async_builtin_cancel_token(ctx),
    );
    let stdout_pipe = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "missing_stdout_pipe")
                .await;
            drop(cleanup_registration);
            return Err(VmError::Runtime(
                "host_call process.exec stdout pipe was not captured".to_string(),
            ));
        }
    };
    let stderr_pipe = match child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "missing_stderr_pipe")
                .await;
            drop(cleanup_registration);
            return Err(VmError::Runtime(
                "host_call process.exec stderr pipe was not captured".to_string(),
            ));
        }
    };
    let stdout_task = tokio::spawn(read_process_exec_pipe(stdout_pipe));
    let stderr_task = tokio::spawn(read_process_exec_pipe(stderr_pipe));

    enum ProcessExecWait {
        Exited(std::io::Result<std::process::ExitStatus>),
        TimedOut,
    }

    let exec_deadline = timeout_ms.map(|timeout_ms| {
        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
    });
    let wait_result = {
        let wait = child.wait();
        tokio::pin!(wait);
        if let Some(deadline) = exec_deadline {
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);
            tokio::select! {
                result = &mut wait => ProcessExecWait::Exited(result),
                _ = &mut sleep => ProcessExecWait::TimedOut,
            }
        } else {
            ProcessExecWait::Exited(wait.await)
        }
    };

    let (mut status, mut success, mut timed_out, mut exit_code) = match wait_result {
        ProcessExecWait::Exited(result) => {
            let status =
                result.map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
            let exit_code = status.code().unwrap_or(-1);
            ("completed", status.success(), false, exit_code)
        }
        ProcessExecWait::TimedOut => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "timeout").await;
            ("timed_out", false, true, -1)
        }
    };

    let drain_pipes = async {
        let stdout = collect_process_exec_pipe(stdout_task, "stdout").await?;
        let stderr = collect_process_exec_pipe(stderr_task, "stderr").await?;
        Ok::<_, VmError>((stdout, stderr))
    };
    tokio::pin!(drain_pipes);
    let (stdout, stderr) = if !timed_out {
        if let Some(deadline) = exec_deadline {
            tokio::select! {
                result = &mut drain_pipes => result?,
                _ = tokio::time::sleep_until(deadline) => {
                    terminate_process_exec_child(
                        &mut child,
                        pid,
                        &cleanup_token,
                        "pipe_drain_timeout",
                    )
                    .await;
                    status = "timed_out";
                    success = false;
                    timed_out = true;
                    exit_code = -1;
                    drain_pipes.await?
                }
            }
        } else {
            drain_pipes.await?
        }
    } else {
        drain_pipes.await?
    };
    drop(cleanup_registration);

    let stdout_utf8_valid = std::str::from_utf8(&stdout).is_ok();
    let stderr_utf8_valid = std::str::from_utf8(&stderr).is_ok();
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    let response = process_exec_response(ProcessExecResponse {
        pid,
        started_at,
        started,
        stdout: &stdout,
        stderr: &stderr,
        exit_code,
        status,
        success,
        timed_out,
        stdout_utf8_valid,
        stderr_utf8_valid,
    });
    crate::orchestration::run_command_policy_postflight_with_ctx(
        ctx,
        params,
        response,
        command_policy_context,
        command_policy_decisions,
    )
    .await
}

async fn read_process_exec_pipe<R>(mut pipe: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn collect_process_exec_pipe(
    task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, VmError> {
    match task.await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(VmError::Runtime(format!(
            "host_call process.exec read {name}: {error}"
        ))),
        Err(error) => Err(VmError::Runtime(format!(
            "host_call process.exec join {name} reader: {error}"
        ))),
    }
}

async fn terminate_process_exec_child(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
    cleanup_token: &str,
    reason: &'static str,
) {
    if let Some(pid) = pid {
        let mut report = crate::op_interrupt::signal_pid_tree_group_and_token_with_report(
            pid,
            Some(cleanup_token),
            9,
        );
        report.refresh_survivor_status();
        tracing::warn!(
            pid,
            children = report.children.len(),
            reason,
            "host_call process.exec signalled child process tree"
        );
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Build a sandboxed `tokio::process::Command` from process-call params,
/// applying argv/shell resolution, the active sandbox policy via
/// [`crate::process_sandbox::tokio_command_for`], cwd enforcement, and
/// env/env_mode/env_remove handling.
///
/// Shared by `process.exec` (synchronous) and `process.spawn`
/// (non-blocking) so both go through the identical sandbox-gated build
/// path. The caller is responsible for any `sandbox_profile` override
/// guard (it must be live across this call) and for setting stdio/kill
/// behaviour on the returned command. `label` ("process.exec" or
/// "process.spawn") is woven into error messages.
pub(crate) fn build_sandboxed_command(
    params: &crate::value::DictMap,
    label: &str,
) -> Result<tokio::process::Command, VmError> {
    let (program, args) = process_exec_argv(params)?;
    let mut cmd = crate::process_sandbox::tokio_command_for(&program, &args)
        .map_err(|e| VmError::Runtime(format!("host_call {label} sandbox setup: {e}")))?;
    if let Some(cwd) = optional_string(params, "cwd") {
        let cwd = resolve_process_exec_cwd(&cwd);
        crate::process_sandbox::enforce_process_cwd(&cwd)
            .map_err(|e| VmError::Runtime(format!("host_call {label} cwd: {e}")))?;
        cmd.current_dir(cwd);
    }
    // Under a session profile the command from `tokio_command_for` already
    // carries the resolver's closed env (parent env cleared), applied once at
    // the sandbox funnel; everything below layers onto that closed base.
    //
    // Track keys the caller set explicitly so the sandbox-local TMPDIR overlay
    // below never clobbers an intentional per-call value.
    let mut caller_env_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(env) = optional_string_dict(params, "env")? {
        // `env_mode` controls how the provided `env` keys combine with the
        // parent environment:
        //   - "merge" (default): inherit the parent env and overlay the
        //     provided keys. This is the least-surprising behavior — a
        //     caller passing `env: {ONE_VAR: "x"}` keeps PATH/HOME/etc.
        //   - "replace": clear the parent env entirely, then set only the
        //     provided keys. This is the footgun shape and must be requested
        //     explicitly whenever `env` is supplied.
        let env_mode = optional_string(params, "env_mode");
        match env_mode.as_deref().unwrap_or("merge") {
            "replace" => {
                cmd.env_clear();
            }
            "merge" => {}
            other => {
                return Err(VmError::Runtime(format!(
                    "host_call {label}: unknown env_mode {other:?}; expected \"merge\" or \"replace\""
                )));
            }
        }
        for (key, value) in env {
            caller_env_keys.insert(key.clone());
            cmd.env(key, value);
        }
    }
    // env_remove: list of environment variable names to strip before
    // spawning. Applied after `env` so callers can both inherit and
    // selectively unset (e.g. the git stdlib strips `GIT_*` so its
    // operations are self-contained even when Harn is invoked from
    // inside a git hook that sets `GIT_DIR`).
    if let Some(env_remove) = optional_string_list(params, "env_remove") {
        for key in env_remove {
            caller_env_keys.insert(key.clone());
            cmd.env_remove(key);
        }
    }
    // Give the child workspace-local temp, home, and toolchain-cache paths. A
    // key the caller set (via `env`) or explicitly stripped (via `env_remove`)
    // is left as intended; only untouched keys receive the overlay.
    for (key, value) in crate::process_sandbox::active_workspace_process_env() {
        if caller_env_keys.contains(&key) {
            continue;
        }
        cmd.env(key, value);
    }
    // Pin tool *message* output to a deterministic English/UTF-8 locale so
    // downstream English-diagnostic matchers (deterministic syntax repair,
    // error-signature grounding, completion/pass-fail classification) do not
    // misfire for a non-Anglosphere user whose shell localizes compiler/test
    // output. A user-inherited `LC_ALL` overrides `LC_MESSAGES`, so strip it
    // first — unless the caller pinned it via `env`/`env_remove` — then apply
    // the overlay with the same caller-wins rule as the TMPDIR overlay above.
    if !caller_env_keys.contains(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV) {
        cmd.env_remove(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV);
    }
    for (key, value) in crate::process_sandbox::deterministic_message_locale_env() {
        if caller_env_keys.contains(&key) {
            continue;
        }
        cmd.env(key, value);
    }
    Ok(cmd)
}

struct ProcessExecResponse<'a> {
    pid: Option<u32>,
    started_at: String,
    started: Instant,
    stdout: &'a str,
    stderr: &'a str,
    exit_code: i32,
    status: &'a str,
    success: bool,
    timed_out: bool,
    stdout_utf8_valid: bool,
    stderr_utf8_valid: bool,
}

fn process_exec_response(response: ProcessExecResponse<'_>) -> VmValue {
    let combined = format!("{}{}", response.stdout, response.stderr);
    let mut result = crate::value::DictMap::new();
    result.put_str(
        "command_id",
        format!(
            "cmd_{}_{}",
            std::process::id(),
            response.started.elapsed().as_nanos()
        ),
    );
    result.put_str("status", response.status);
    result.insert(
        crate::value::intern_key("pid"),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        crate::value::intern_key("process_group_id"),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(crate::value::intern_key("handle_id"), VmValue::Nil);
    result.put_str("started_at", response.started_at);
    result.put_str(
        "ended_at",
        audited_utc_now_rfc3339("host_call/process.exec.ended_at"),
    );
    result.insert(
        crate::value::intern_key("duration_ms"),
        VmValue::Int(response.started.elapsed().as_millis() as i64),
    );
    result.insert(
        crate::value::intern_key("exit_code"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(crate::value::intern_key("signal"), VmValue::Nil);
    result.insert(
        crate::value::intern_key("timed_out"),
        VmValue::Bool(response.timed_out),
    );
    result.put_str("stdout", response.stdout);
    result.put_str("stderr", response.stderr);
    result.insert(
        crate::value::intern_key("stdout_utf8_valid"),
        VmValue::Bool(response.stdout_utf8_valid),
    );
    result.insert(
        crate::value::intern_key("stderr_utf8_valid"),
        VmValue::Bool(response.stderr_utf8_valid),
    );
    result.put_str("combined", combined);
    result.insert(
        crate::value::intern_key("exit_status"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(
        crate::value::intern_key("legacy_status"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(
        crate::value::intern_key("success"),
        VmValue::Bool(response.success),
    );
    VmValue::dict(result)
}

pub(super) fn resolve_process_exec_cwd(cwd: &str) -> std::path::PathBuf {
    crate::stdlib::process::resolve_source_relative_path(cwd)
}

pub(super) fn process_exec_argv(
    params: &crate::value::DictMap,
) -> Result<(String, Vec<String>), VmError> {
    match optional_string(params, "mode")
        .as_deref()
        .unwrap_or("shell")
    {
        "argv" => {
            let argv = optional_string_list(params, "argv").ok_or_else(|| {
                VmError::Runtime("host_call process.exec missing argv".to_string())
            })?;
            split_argv(argv)
        }
        "shell" => {
            let command = require_param(params, "command")?;
            let mut invocation_params = params.clone();
            invocation_params.put_str("command", command);
            let invocation =
                crate::shells::resolve_invocation_from_vm_params(&invocation_params)
                    .map_err(|err| VmError::Runtime(format!("host_call process.exec: {err}")))?;
            Ok((invocation.program, invocation.args))
        }
        other => Err(VmError::Runtime(format!(
            "host_call process.exec unsupported mode {other:?}"
        ))),
    }
}

fn split_argv(mut argv: Vec<String>) -> Result<(String, Vec<String>), VmError> {
    if argv.is_empty() {
        return Err(VmError::Runtime(
            "host_call process.exec argv must not be empty".to_string(),
        ));
    }
    let program = argv.remove(0);
    if program.is_empty() {
        return Err(VmError::Runtime(
            "host_call process.exec argv[0] must not be empty".to_string(),
        ));
    }
    Ok((program, argv))
}

/// Push a transient policy onto the execution stack with the
/// requested sandbox profile, returning a guard that pops on drop.
/// Used by `host_call("process", "exec", ...)` to honor a per-call
/// `sandbox_profile` override without rewriting the surrounding
/// orchestration policy.
pub(crate) fn push_sandbox_profile_override(value: &str) -> Result<SandboxProfileGuard, VmError> {
    let profile = crate::orchestration::SandboxProfile::parse(value).ok_or_else(|| {
        let expected = crate::orchestration::SandboxProfile::all()
            .iter()
            .map(|profile| format!("{:?}", profile.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "host_call process.exec: unknown sandbox_profile {value:?}; expected one of {expected}"
        ))))
    })?;
    let mut policy = crate::orchestration::current_execution_policy().unwrap_or_default();
    policy.sandbox_profile = profile;
    crate::orchestration::push_execution_policy(policy);
    Ok(SandboxProfileGuard {
        _private: std::marker::PhantomData,
    })
}

pub(crate) struct SandboxProfileGuard {
    _private: std::marker::PhantomData<*const ()>,
}

impl Drop for SandboxProfileGuard {
    fn drop(&mut self) {
        crate::orchestration::pop_execution_policy();
    }
}
