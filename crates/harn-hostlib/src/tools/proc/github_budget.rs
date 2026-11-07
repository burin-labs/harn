//! Token-free exchanges with an installed GitHub budget owner.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use base64::Engine;
use harn_vm::orchestration::{
    CapabilityPolicy, ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile,
};
use harn_vm::stdlib::process::{GitHubBudgetOwner, GrantedSpawn};
use serde::Deserialize;

use crate::error::HostlibError;
use crate::host_call::HostCallContext;
use crate::process::{self, EnvMode, OutputCapture, ProcessHandle, SpawnSpec, WaitOutcome};

const SCHEMA: &str = "gh-budget.native-read.v1";
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(untagged)]
enum Nullable<T> {
    Value(T),
    Null(()),
}

impl<T> Nullable<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Null(()) => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    BeforeRequest,
    AfterAdmission,
    AfterRequest,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeRequest => "before_request",
            Self::AfterAdmission => "after_admission",
            Self::AfterRequest => "after_request",
        }
    }

    fn executed(self) -> Option<bool> {
        match self {
            Self::BeforeRequest => Some(false),
            Self::AfterAdmission => None,
            Self::AfterRequest => Some(true),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Prepared {
        schema: String,
        if_none_match: Nullable<String>,
    },
    Run {
        schema: String,
        if_none_match: Nullable<String>,
    },
    Completed {
        schema: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        request_executed: bool,
        response_observed: bool,
    },
    Refused {
        schema: String,
        exit_code: i32,
        request_executed: bool,
        detail: String,
    },
    Unavailable {
        schema: String,
        exit_code: i32,
        phase: Phase,
        operation: String,
        request_executed: Nullable<bool>,
        response_observed: bool,
        detail: String,
    },
}

impl Reply {
    fn schema(&self) -> &str {
        match self {
            Self::Prepared { schema, .. }
            | Self::Run { schema, .. }
            | Self::Completed { schema, .. }
            | Self::Refused { schema, .. }
            | Self::Unavailable { schema, .. } => schema,
        }
    }
}

fn decode_reply(line: &[u8]) -> Result<Reply, &'static str> {
    if line.len() > MAX_MESSAGE || line.last() != Some(&b'\n') {
        return Err("the budget owner reply is missing, incomplete or oversized");
    }
    let reply: Reply =
        serde_json::from_slice(line).map_err(|_| "the budget owner returned an invalid reply")?;
    if reply.schema() != SCHEMA {
        return Err("the budget owner returned an unsupported schema");
    }
    Ok(reply)
}

pub(super) struct BudgetOutput {
    pub(super) exit_code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) fn cached_outcome(
    req: &super::SpawnRequest,
    filter: &harn_vm::session_inputs::OutputFilter,
    command_id: String,
    cwd: PathBuf,
    started_at: String,
    started: Instant,
    output: BudgetOutput,
) -> Result<super::SpawnOutcome, HostlibError> {
    let stdout = filter.redact_bytes(&output.stdout);
    let stderr = filter.redact_bytes(&output.stderr);
    let artifacts = super::persist_artifacts(&command_id, &stdout, &stderr, None)?;
    let inline = super::inline_output(&stdout, &stderr, req.capture);
    Ok(super::SpawnOutcome {
        verifier_receipt: None,
        command_id,
        cwd,
        status: super::CommandStatus::Completed,
        pid: None,
        process_group_id: None,
        started_at,
        ended_at: Some(super::now_rfc3339()),
        exit_code: output.exit_code,
        missing_program: None,
        signal: None,
        stdout: inline.stdout,
        stdout_truncated: inline.stdout_truncated,
        stderr: inline.stderr,
        stderr_truncated: inline.stderr_truncated,
        output_path: artifacts.output_path,
        stdout_path: artifacts.stdout_path,
        stderr_path: artifacts.stderr_path,
        line_count: artifacts.line_count,
        byte_count: artifacts.byte_count,
        output_sha256: artifacts.output_sha256,
        duration: started.elapsed(),
        timed_out: false,
        process_cleanup: None,
        sandbox_assessment: harn_vm::process_sandbox::ProcessSandboxAssessment {
            report: harn_vm::process_sandbox::ProcessSandboxReport {
                kind: "not_started".into(),
                enforced: false,
                denial_reporting:
                    harn_vm::process_sandbox::ProcessSandboxDenialReporting::NotEnforced,
                network: harn_vm::process_sandbox::ProcessSandboxNetwork::Unknown,
            },
            refusal: None,
        },
    })
}

/// A literal native invocation retained across assessment. No helper process
/// or ledger admission remains live while a person considers the command.
pub(super) struct BudgetPlan {
    pub(super) grant: Arc<GrantedSpawn>,
    pub(super) argv: Vec<String>,
    cwd: PathBuf,
    api_args: Vec<String>,
    etag: Option<String>,
    request_id: String,
    caller: String,
}

impl BudgetPlan {
    pub(super) fn validate_invocation(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<(), HostlibError> {
        if cwd != self.cwd
            || !std::iter::once(program)
                .chain(args.iter().map(String::as_str))
                .eq(self.argv.iter().map(String::as_str))
        {
            return Err(unavailable(
                Phase::BeforeRequest,
                "retained_invocation",
                Some(false),
                false,
                "the native GitHub invocation changed after assessment".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn prepare(
        grant: Arc<GrantedSpawn>,
        api_args: Vec<String>,
        cwd: &Path,
    ) -> Result<Self, HostlibError> {
        let owner = grant.github_budget.as_ref().expect("enrolled owner");
        let request_id = super::next_command_id();
        let caller = format!(
            "harn:{}",
            cwd.file_name().unwrap_or_default().to_string_lossy()
        );
        let mut rpc = Rpc::launch(owner, cwd, Some(Instant::now() + PREPARE_TIMEOUT))?;
        rpc.send(&request(owner, &request_id, &api_args, "prepare", &caller))?;
        let etag = match rpc.receive()? {
            Reply::Prepared { if_none_match, .. } => if_none_match.into_option(),
            other => return Err(rpc.failure_reply(other, "preparation")),
        };
        rpc.retire()?;
        let argv = owner
            .native_argv(&api_args, etag.as_deref())
            .map_err(|detail| {
                unavailable(
                    Phase::BeforeRequest,
                    "preparation",
                    Some(false),
                    false,
                    detail,
                )
            })?;
        Ok(Self {
            grant,
            argv,
            cwd: cwd.into(),
            api_args,
            etag,
            request_id,
            caller,
        })
    }

    pub(super) fn admit(&self, deadline: Option<Instant>) -> Result<Admission, HostlibError> {
        let owner = self.grant.github_budget.as_ref().expect("enrolled owner");
        let mut rpc = Rpc::launch(owner, &self.cwd, deadline)?;
        let mut begin = request(
            owner,
            &self.request_id,
            &self.api_args,
            "begin",
            &self.caller,
        );
        begin["if_none_match"] = serde_json::to_value(&self.etag).expect("optional string");
        rpc.send(&begin)?;
        match rpc.receive()? {
            Reply::Run { if_none_match, .. } => {
                if if_none_match.into_option() != self.etag {
                    return Err(
                        rpc.failure("admission", "the budget owner changed the retained ETag")
                    );
                }
                rpc.phase = Phase::AfterAdmission;
                Ok(Admission::Run(rpc))
            }
            reply @ (Reply::Completed {
                request_executed: false,
                response_observed: false,
                ..
            }
            | Reply::Refused {
                request_executed: false,
                ..
            }) => {
                let output = rpc.output(reply, false)?;
                rpc.retire()?;
                Ok(Admission::Completed(output))
            }
            other => Err(rpc.failure_reply(other, "admission")),
        }
    }

    pub(super) fn started(&self, rpc: &mut Rpc) -> Result<(), HostlibError> {
        rpc.phase = Phase::AfterRequest;
        rpc.send(&serde_json::json!({"schema": SCHEMA, "phase": "started", "request_id": self.request_id}))
    }

    pub(super) fn finish(
        &self,
        rpc: &mut Rpc,
        exit_code: i32,
        stdout: &[u8],
        stderr: &[u8],
        cancelled: bool,
    ) -> Result<BudgetOutput, HostlibError> {
        // Accounting is local and performs no HTTP. Give it a bounded cleanup
        // phase after the native request deadline, including a timed-out child.
        rpc.deadline = Some(Instant::now() + FINALIZE_TIMEOUT);
        rpc.finalizing = true;
        let base64 = base64::engine::general_purpose::STANDARD;
        rpc.send(&serde_json::json!({
            "schema": SCHEMA, "phase": "finish", "request_id": self.request_id,
            "exit_code": exit_code, "stdout": base64.encode(stdout), "stderr": base64.encode(stderr),
            "cancelled": cancelled,
        }))?;
        let reply = rpc.receive()?;
        let output = rpc.output(reply, true)?;
        rpc.retire()?;
        Ok(output)
    }
}

pub(super) enum Admission {
    Run(Rpc),
    Completed(BudgetOutput),
}

fn request(
    owner: &GitHubBudgetOwner,
    id: &str,
    args: &[String],
    phase: &str,
    caller: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema": SCHEMA, "phase": phase, "request_id": id, "argv": args,
        "credential_digest": owner.credential_digest, "account_config_digest": owner.account_config_digest,
        "caller": caller,
    })
}

fn unavailable(
    phase: Phase,
    operation: &str,
    executed: Option<bool>,
    response_observed: bool,
    message: String,
) -> HostlibError {
    HostlibError::GitHubBudgetUnavailable {
        builtin: crate::tools::run_command::NAME,
        phase: phase.name(),
        operation: operation.into(),
        request_executed: executed,
        response_observed,
        message,
    }
}

struct HelperPolicy;

impl HelperPolicy {
    fn enter(owner: &GitHubBudgetOwner) -> Result<Self, HostlibError> {
        let runtime = owner
            .interpreter
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                unavailable(
                    Phase::BeforeRequest,
                    "helper_launch",
                    Some(false),
                    false,
                    "the enrolled interpreter has no runtime root".into(),
                )
            })?;
        let denied = harn_vm::orchestration::current_execution_policy()
            .map(|policy| policy.process_sandbox.read_deny_roots.clone())
            .unwrap_or_default();
        harn_vm::orchestration::push_execution_policy(CapabilityPolicy {
            workspace_roots: vec![owner.state_directory.display().to_string()],
            read_only_roots: [
                owner.adapter.as_path(),
                owner.implementation.as_path(),
                owner.manifest_path(),
                runtime,
            ]
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
            sandbox_profile: SandboxProfile::OsHardened,
            process_sandbox: Box::new(ProcessSandboxPolicy {
                presets: Some(vec![ProcessSandboxPreset::SystemRuntime]),
                read_deny_roots: denied,
                allow_child_workspace_write: true,
                deny_network: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        Ok(Self)
    }
}

impl Drop for HelperPolicy {
    fn drop(&mut self) {
        harn_vm::orchestration::pop_execution_policy();
    }
}

pub(super) struct Rpc {
    handle: Box<dyn ProcessHandle>,
    input: Option<Box<dyn Write + Send>>,
    replies: mpsc::Receiver<Result<Reply, &'static str>>,
    deadline: Option<Instant>,
    phase: Phase,
    response_observed: bool,
    call: HostCallContext,
    _cleanup: Option<harn_vm::op_interrupt::ActiveProcessCleanupGuard>,
    retired: bool,
    finalizing: bool,
}

impl Rpc {
    pub(super) fn native_failure(&self, message: &str) -> HostlibError {
        self.failure("native_wait", message)
    }

    fn launch(
        owner: &GitHubBudgetOwner,
        cwd: &Path,
        deadline: Option<Instant>,
    ) -> Result<Self, HostlibError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || harn_vm::op_interrupt::requested()
        {
            return Err(unavailable(
                Phase::BeforeRequest,
                "helper_launch",
                Some(false),
                false,
                "the command was cancelled or its deadline elapsed before budget accounting".into(),
            ));
        }
        owner.revalidate(cwd).map_err(|message| {
            unavailable(
                Phase::BeforeRequest,
                "owner_identity",
                Some(false),
                false,
                message,
            )
        })?;
        let call = HostCallContext::current();
        let scope = call.process_scope();
        let permit = scope
            .map(|scope| {
                scope.enter_launch().ok_or_else(|| {
                    unavailable(
                        Phase::BeforeRequest,
                        "helper_launch",
                        Some(false),
                        false,
                        "command cancelled before budget preparation".into(),
                    )
                })
            })
            .transpose()?;
        let policy = HelperPolicy::enter(owner)?;
        let mut handle = process::spawn_process(SpawnSpec {
            builtin: crate::tools::run_command::NAME,
            program: owner.interpreter.display().to_string(),
            args: vec![
                "-I".into(),
                "-B".into(),
                owner.adapter.display().to_string(),
                "--serve-native".into(),
            ],
            cwd: Some(owner.state_directory.clone()),
            env: [
                ("GH_DEBUG".into(), String::new()),
                ("GODEBUG".into(), String::new()),
            ]
            .into(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: true,
            configure_process_group: true,
            owner_death: process::OwnerDeathPolicy::None,
            output_capture: OutputCapture::Pipe,
        })
        .map_err(|_| {
            unavailable(
                Phase::BeforeRequest,
                "helper_launch",
                Some(false),
                false,
                "the enrolled token-free budget helper could not start".into(),
            )
        })?;
        drop(policy);
        let cleanup = scope.and_then(|scope| scope.register(handle.as_ref()));
        drop(permit);
        let input = handle.take_stdin();
        let stdout = handle.take_stdout();
        let stderr = handle.take_stderr();
        let (sender, replies) = mpsc::sync_channel(1);
        harn_parser::runtime_stack::spawn(move || {
            let Some(stdout) = stdout else { return };
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result = (&mut reader)
                    .take((MAX_MESSAGE + 1) as u64)
                    .read_until(b'\n', &mut line);
                if matches!(result, Ok(0)) {
                    break;
                }
                let reply = match result {
                    Ok(_) => decode_reply(&line),
                    _ => Err("the budget owner reply is missing, incomplete or oversized"),
                };
                let failed = reply.is_err();
                if sender.send(reply).is_err() || failed {
                    break;
                }
            }
        });
        harn_parser::runtime_stack::spawn(move || {
            if let Some(stderr) = stderr {
                let _ = std::io::copy(&mut stderr.take(1024 * 1024), &mut std::io::sink());
            }
        });
        Ok(Self {
            handle,
            input,
            replies,
            deadline,
            phase: Phase::BeforeRequest,
            response_observed: false,
            call,
            _cleanup: cleanup,
            retired: false,
            finalizing: false,
        })
    }

    fn failure(&self, operation: &str, message: &str) -> HostlibError {
        unavailable(
            self.phase,
            operation,
            self.phase.executed(),
            self.response_observed,
            message.into(),
        )
    }

    fn send(&mut self, message: &serde_json::Value) -> Result<(), HostlibError> {
        let mut bytes = serde_json::to_vec(message).expect("native budget message");
        bytes.push(b'\n');
        if bytes.len() > MAX_MESSAGE {
            return Err(self.failure(
                "helper_exchange",
                "the budget message exceeds its bounded transport",
            ));
        }
        let Some(input) = self.input.as_mut() else {
            return Err(self.failure("helper_exchange", "the budget helper has no input"));
        };
        let result = input.write_all(&bytes).and_then(|()| input.flush());
        result.map_err(|_| {
            self.failure(
                "helper_exchange",
                "the budget helper input became unavailable",
            )
        })
    }

    fn receive(&mut self) -> Result<Reply, HostlibError> {
        loop {
            if !self.finalizing
                && (harn_vm::op_interrupt::requested()
                    || self
                        .call
                        .process_scope()
                        .is_some_and(|scope| scope.cancel_requested()))
            {
                return Err(self.failure(
                    "helper_exchange",
                    "command cancelled during budget accounting",
                ));
            }
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(self.failure(
                    "helper_exchange",
                    "command deadline elapsed during budget accounting",
                ));
            }
            let wait = self
                .deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_millis(20))
                .min(Duration::from_millis(20));
            match self.replies.recv_timeout(wait) {
                Ok(reply) => {
                    return reply.map_err(|detail| self.failure("helper_exchange", detail))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.failure(
                        "helper_exchange",
                        "the budget helper exited without a complete reply",
                    ))
                }
            }
        }
    }

    fn failure_reply(&self, reply: Reply, operation: &str) -> HostlibError {
        if let Reply::Unavailable {
            exit_code: 78,
            phase,
            operation,
            request_executed,
            response_observed,
            detail,
            ..
        } = reply
        {
            let executed = request_executed.into_option();
            if executed != phase.executed()
                || (response_observed && phase != Phase::AfterRequest)
                || (phase == Phase::AfterRequest && self.phase != Phase::AfterRequest)
            {
                return self.failure(
                    "helper_exchange",
                    "the budget owner returned inconsistent execution evidence",
                );
            }
            return unavailable(
                if self.phase == Phase::AfterRequest {
                    self.phase
                } else {
                    phase
                },
                &operation,
                if matches!(self.phase, Phase::AfterRequest) {
                    Some(true)
                } else {
                    executed
                },
                response_observed,
                detail,
            );
        }
        self.failure(
            operation,
            "the budget owner returned a decision incompatible with the retained command",
        )
    }

    fn output(&mut self, reply: Reply, executed: bool) -> Result<BudgetOutput, HostlibError> {
        match reply {
            Reply::Completed {
                exit_code,
                stdout,
                stderr,
                request_executed,
                response_observed,
                ..
            } if request_executed == executed => {
                self.response_observed = response_observed;
                if executed && exit_code == 0 && !response_observed {
                    return Err(self.failure(
                        "response_recording",
                        "the successful native read has no observed response",
                    ));
                }
                let decode = |value: &str| {
                    base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|_| {
                            self.failure(
                                "helper_exchange",
                                "the budget owner returned invalid output bytes",
                            )
                        })
                };
                Ok(BudgetOutput {
                    exit_code,
                    stdout: decode(&stdout)?,
                    stderr: decode(&stderr)?,
                })
            }
            Reply::Refused {
                exit_code: exit_code @ (75 | 77),
                request_executed: false,
                detail,
                ..
            } if !executed => Ok(BudgetOutput {
                exit_code,
                stdout: Vec::new(),
                stderr: detail.into_bytes(),
            }),
            other => Err(self.failure_reply(other, "response_recording")),
        }
    }

    fn retire(&mut self) -> Result<(), HostlibError> {
        self.input.take();
        let remaining = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let finalizing = self.finalizing;
        let result = self.handle.wait_with_timeout(remaining, &|| {
            !finalizing && harn_vm::op_interrupt::requested()
        });
        if let Ok(WaitOutcome::Exited(status)) = result {
            self.retired = true;
            if status.code == Some(0) && status.signal.is_none() {
                return Ok(());
            }
        }
        Err(self.failure(
            "helper_exit",
            "the budget owner did not exit successfully after its decision",
        ))
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.input.take();
        if !self.retired {
            let _ = self.handle.killer().kill();
            let _ = self
                .handle
                .wait_with_timeout(Some(Duration::from_secs(1)), &|| false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_reply_requires_explicit_nullable_plan_and_closed_versioned_fields() {
        let valid =
            serde_json::json!({"schema": SCHEMA, "decision": "prepared", "if_none_match": null});
        let encode = |value: &serde_json::Value| {
            let mut bytes = serde_json::to_vec(value).unwrap();
            bytes.push(b'\n');
            bytes
        };
        assert!(matches!(
            decode_reply(&encode(&valid)).unwrap(),
            Reply::Prepared {
                if_none_match: Nullable::Null(()),
                ..
            }
        ));
        for invalid in [
            serde_json::json!({"schema": SCHEMA, "decision": "prepared"}),
            serde_json::json!({"schema": SCHEMA, "decision": "prepared", "if_none_match": false}),
            serde_json::json!({"schema": "unknown", "decision": "prepared", "if_none_match": null}),
            serde_json::json!({"schema": SCHEMA, "decision": "run", "if_none_match": null, "argv": ["other"]}),
            serde_json::json!({"schema": SCHEMA, "decision": "completed", "exit_code": 0, "stdout": "", "stderr": "", "response_observed": false}),
        ] {
            assert!(decode_reply(&encode(&invalid)).is_err());
        }
        let mut incomplete = encode(&valid);
        incomplete.pop();
        assert!(decode_reply(&incomplete).is_err());
    }
}
