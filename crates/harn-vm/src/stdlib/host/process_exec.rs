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
    audited_utc_now_rfc3339, optional_i64, optional_string, optional_string_dict,
    optional_string_list, require_param,
};

pub(super) fn async_builtin_cancel_token(
    ctx: Option<&AsyncBuiltinCtx>,
) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
    ctx.and_then(|ctx| ctx.child_vm().cancel_token.clone())
}

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
    // Workspace effects govern mutation and approval, not host resource cost.
    // Read-only compilers and formatters still contend for the same process,
    // CPU, and toolchain lanes as writers, so every spawned command participates.
    let _process_admission = super::process_admission::acquire_process_admission(ctx).await?;
    let (tape_program, tape_args) = process_exec_argv(params)?;
    let tape_cwd = optional_string(params, "cwd").map(|cwd| resolve_process_exec_cwd(&cwd));
    let started_at = audited_utc_now_rfc3339("host_call/process.exec.started_at");
    let started = crate::clock_mock::leak_audit::instant_now("host_call/process.exec.started");
    if let Some(intercepted) = crate::testbench::process_tape::intercept_spawn(
        &tape_program,
        &tape_args,
        tape_cwd.as_deref(),
    ) {
        let output = intercepted
            .map_err(|message| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message))))?;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout_utf8_valid = std::str::from_utf8(&output.stdout).is_ok();
        let stderr_utf8_valid = std::str::from_utf8(&output.stderr).is_ok();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let response = process_exec_response(ProcessExecResponse {
            pid: None,
            started_at,
            started,
            stdout: &stdout,
            stderr: &stderr,
            exit_code,
            status: "completed",
            success: output.status.success(),
            timed_out: false,
            stdout_utf8_valid,
            stderr_utf8_valid,
        });
        return crate::orchestration::run_command_policy_postflight_with_ctx(
            ctx,
            params,
            response,
            command_policy_context,
            command_policy_decisions,
        )
        .await;
    }
    let tape_recording = crate::testbench::process_tape::start_recording(
        &tape_program,
        &tape_args,
        tape_cwd.as_deref(),
    );
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
        Some(value) => Some(crate::process_sandbox::push_sandbox_profile_override(
            &value,
        )?),
        None => None,
    };
    #[cfg(target_os = "windows")]
    if let Some((policy, profile)) = crate::stdlib::sandbox::active_sandbox_policy() {
        let launch = ProcessExecLaunch::from_params(params, "process.exec")?;
        // The resolved policy/profile are owned snapshots. Drop the thread-
        // local, non-Send override guard before the blocking worker await; the
        // Windows backend receives the snapshot explicitly.
        drop(profile_guard);
        let output = crate::stdlib::sandbox::windows_command_output(
            tape_program,
            tape_args,
            launch.into_process_config(),
            policy,
            profile,
        )
        .await
        .map_err(|error| contextualize_process_error("process.exec", "sandbox", error))?;
        let exit_code = output.status.code().unwrap_or(-1);
        if let Some(recording) = tape_recording {
            recording.finish(&output);
        }
        let stdout_utf8_valid = std::str::from_utf8(&output.stdout).is_ok();
        let stderr_utf8_valid = std::str::from_utf8(&output.stderr).is_ok();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let response = process_exec_response(ProcessExecResponse {
            pid: None,
            started_at,
            started,
            stdout: &stdout,
            stderr: &stderr,
            exit_code,
            status: "completed",
            success: output.status.success(),
            timed_out: false,
            stdout_utf8_valid,
            stderr_utf8_valid,
        });
        return crate::orchestration::run_command_policy_postflight_with_ctx(
            ctx,
            params,
            response,
            command_policy_context,
            command_policy_decisions,
        )
        .await;
    }
    let mut cmd = build_sandboxed_command(params, "process.exec")?;
    crate::op_interrupt::configure_tokio_kill_group(&mut cmd);
    let cleanup_token = crate::op_interrupt::new_process_cleanup_token();
    cmd.env(
        crate::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    crate::op_interrupt::preserve_process_owner_token(cmd.as_std_mut());
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|error| crate::value::environment_io_error_thrown(&error, error.to_string()))?;
    drop(profile_guard);
    let pid = child.id();
    crate::op_interrupt::record_tokio_process_owner_group(&mut child, &cleanup_token)
        .await
        .map_err(|error| {
            VmError::Runtime(format!(
                "host_call process.exec record owner group: {error}"
            ))
        })?;
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
    if let Some(recording) = tape_recording {
        recording.finish_parts(exit_code, &stdout, &stderr);
    }

    if status == "completed" {
        if let Some(error) = crate::process_sandbox::wrapped_spawn_io_error(exit_code, &stderr) {
            return Err(crate::value::environment_io_error_thrown(
                &error,
                error.to_string(),
            ));
        }
    }

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
/// The platform-independent normalization lives in [`ProcessExecLaunch`].
/// This function projects it onto Tokio for `process.spawn` and for
/// `process.exec` on platforms whose sandbox can decorate a Tokio command.
/// Windows exec projects the same launch onto `ProcessCommandConfig` because
/// AppContainer requires the custom output backend.
pub(crate) fn build_sandboxed_command(
    params: &crate::value::DictMap,
    label: &str,
) -> Result<tokio::process::Command, VmError> {
    let launch = ProcessExecLaunch::from_params(params, label)?;
    let mut cmd = crate::process_sandbox::tokio_command_for(&launch.program, &launch.args)
        .map_err(|error| contextualize_process_error(label, "sandbox setup", error))?;
    if let Some(cwd) = launch.cwd {
        cmd.current_dir(cwd);
    }
    if launch.closed_env {
        cmd.env_clear();
    }
    for (key, value) in launch.env {
        cmd.env(key, value);
    }
    for key in launch.env_remove {
        cmd.env_remove(key);
    }
    Ok(cmd)
}

/// Normalized process authority at the host boundary.
///
/// Parsing, cwd confinement, execution-context overlays, caller overrides,
/// removals, workspace-local paths, and deterministic locale policy happen
/// once here. Platform launchers are deliberately boring projections of this
/// value so Windows AppContainer and Tokio-backed hosts cannot drift.
struct ProcessExecLaunch {
    program: String,
    args: Vec<String>,
    cwd: Option<std::path::PathBuf>,
    env: Vec<(String, String)>,
    env_remove: Vec<String>,
    closed_env: bool,
}

impl ProcessExecLaunch {
    fn from_params(params: &crate::value::DictMap, label: &str) -> Result<Self, VmError> {
        let (program, args) = process_exec_argv(params)?;
        let execution_context = crate::stdlib::process::current_execution_context();
        let cwd = match optional_string(params, "cwd") {
            Some(cwd) => resolve_process_exec_cwd(&cwd),
            None => crate::stdlib::process::inherited_process_cwd()
                .map_err(|error| contextualize_process_error(label, "cwd", error))?,
        };
        crate::process_sandbox::enforce_process_cwd(&cwd)
            .map_err(|error| contextualize_process_error(label, "cwd", error))?;

        let closed_env = match optional_string(params, "env_mode")
            .as_deref()
            .unwrap_or("merge")
        {
            "replace" => true,
            "merge" => false,
            other => {
                return Err(VmError::Runtime(format!(
                    "host_call {label}: unknown env_mode {other:?}; expected \"merge\" or \"replace\""
                )));
            }
        };
        let mut env = Vec::new();
        if !closed_env {
            if let Some(context) = &execution_context {
                env.extend(
                    context
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            }
        }
        env.extend(crate::stdlib::process::runtime_child_env_overlay());

        let mut caller_env_keys = std::collections::BTreeSet::new();
        if let Some(overrides) = optional_string_dict(params, "env")? {
            for (key, value) in overrides {
                caller_env_keys.insert(key.clone());
                env.push((key, value));
            }
        }
        let mut env_remove = optional_string_list(params, "env_remove").unwrap_or_default();
        caller_env_keys.extend(env_remove.iter().cloned());
        for (key, value) in crate::process_sandbox::active_workspace_process_env() {
            if !caller_env_keys.contains(&key) {
                env.push((key, value));
            }
        }
        if !caller_env_keys.contains(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV) {
            env_remove.push(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV.to_string());
        }
        for (key, value) in crate::process_sandbox::deterministic_message_locale_env() {
            if !caller_env_keys.contains(&key) {
                env.push((key, value));
            }
        }
        Ok(Self {
            program,
            args,
            cwd: Some(cwd),
            env,
            env_remove,
            closed_env,
        })
    }

    #[cfg(target_os = "windows")]
    fn into_process_config(self) -> crate::stdlib::sandbox::ProcessCommandConfig {
        crate::stdlib::sandbox::ProcessCommandConfig {
            cwd: self.cwd,
            env: self.env,
            env_remove: self.env_remove,
            stdin_null: true,
            closed_env: self.closed_env,
        }
    }
}

fn contextualize_process_error(label: &str, stage: &str, error: VmError) -> VmError {
    match error {
        VmError::CategorizedError { message, category } => VmError::CategorizedError {
            message: format!("host_call {label} {stage}: {message}"),
            category,
        },
        other => VmError::Runtime(format!("host_call {label} {stage}: {other}")),
    }
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
