//! Guarded process pipes adapted to the ordinary VM MCP connection owner.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use harn_vm::mcp::{connect_mcp_stdio_transport, McpProcessOwner, VmMcpClientHandle};
use harn_vm::VmError;
use tokio::sync::oneshot;
use tokio_util::io::SyncIoBridge;

use super::{spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome};

const PIPE_CAPACITY: usize = 64 * 1024;

/// Host-selected launch configuration, independent of worker-supplied catalogs.
/// The current execution policy owns filesystem and network authority. The
/// environment replaces inheritance, and only declared path roots are used.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpProcessSpec {
    /// Host-selected executable.
    pub program: String,
    /// Literal argument vector; no shell expansion is performed.
    #[serde(default)]
    pub args: Vec<String>,
    /// Explicit working directory admitted by the current execution policy.
    pub cwd: PathBuf,
    /// Complete child environment, with no ambient inheritance.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

pub(crate) fn register(vm: &mut harn_vm::Vm) {
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_native_connect",
        |_ctx, args| async move {
            let refused = || VmError::TypeError("native MCP requires a closed process spec, valid Harn IR and positive concurrency".into());
            let json = |index: usize| harn_vm::tool_registry::result_to_json(args.get(index).ok_or_else(refused)?).map_err(|_| refused());
            let spec: McpProcessSpec = serde_json::from_value(json(0)?).map_err(|_| refused())?;
            let ir = harn_vm::codegen::validate_ir(&serde_json::to_vec(&json(1)?).map_err(|_| refused())?)
                .map_err(|_| refused())?;
            let Some(harn_vm::VmValue::Int(concurrency)) = args.get(2) else { return Err(refused()) };
            let concurrency = u32::try_from(*concurrency).ok().and_then(std::num::NonZeroU32::new).ok_or_else(refused)?;
            let client = connect_guarded_mcp_stdio("native Harn tool binding", spec, Some(harn_vm::mcp_protocol::PROTOCOL_VERSION)).await?;
            if let Err(error) = client.bind_native(ir, concurrency).await {
                let cleanup = client.disconnect().await;
                return Err(VmError::Runtime(if cleanup.is_ok() {
                    error.to_string()
                } else {
                    "native MCP profile refused; process cleanup did not complete".into()
                }));
            }
            Ok(harn_vm::VmValue::mcp_client(client))
        },
    );
}

struct GuardedMcpProcess {
    cancel: Arc<AtomicBool>,
    completion: oneshot::Receiver<io::Result<()>>,
}

impl Drop for GuardedMcpProcess {
    fn drop(&mut self) {
        // The existing ProcessHandle waiter owns termination and reaping. A
        // failed handshake or dropped connection must not strand that waiter.
        self.cancel.store(true, Ordering::Release);
    }
}

#[async_trait::async_trait]
impl McpProcessOwner for GuardedMcpProcess {
    async fn shutdown(&mut self, grace: Duration) -> io::Result<()> {
        let result = match tokio::time::timeout(grace, &mut self.completion).await {
            Ok(result) => result,
            Err(_) => {
                self.cancel.store(true, Ordering::Release);
                (&mut self.completion).await
            }
        };
        result.map_err(|_| io::Error::other("MCP process waiter did not report"))?
    }
}

/// Launch a contained worker and connect its pipes through the existing MCP
/// SDK lifecycle. This does not trust a worker profile; callers independently
/// bind a prepared Harn contract with `VmMcpClientHandle::bind_native`.
pub async fn connect_guarded_mcp_stdio(
    name: &str,
    spec: McpProcessSpec,
    protocol_version: Option<&str>,
) -> Result<VmMcpClientHandle, VmError> {
    let mut child = harn_vm::stdlib::sandbox::with_declared_path_roots(|| {
        spawn_process(SpawnSpec {
            builtin: "native_mcp_worker",
            program: spec.program,
            args: spec.args,
            cwd: Some(spec.cwd),
            env: spec.environment,
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: true,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        })
    })
    .map_err(|_| VmError::Runtime("native MCP worker spawn refused".into()))?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (completion_tx, completion) = oneshot::channel();
    let stdout = child.take_stdout();
    let stdin = child.take_stdin();
    let stderr = child.take_stderr();
    let waiter_cancel = cancel.clone();
    harn_parser::runtime_stack::spawn(move || {
        let outcome = child
            .wait_with_timeout(None, &|| waiter_cancel.load(Ordering::Acquire))
            .and_then(|outcome| match outcome {
                WaitOutcome::Exited(_) => Ok(()),
                WaitOutcome::Interrupted(report) | WaitOutcome::TimedOut(report)
                    if report.survivor_count() == Some(0) =>
                {
                    Ok(())
                }
                _ => Err(io::Error::other(
                    "MCP process cleanup remains unmeasured or incomplete",
                )),
            });
        let _ = completion_tx.send(outcome);
    });
    let owner = Box::new(GuardedMcpProcess { cancel, completion });
    let (Some(mut stdout), Some(mut stdin), Some(stderr)) = (stdout, stdin, stderr) else {
        return Err(VmError::Runtime(
            "native MCP worker pipes unavailable".into(),
        ));
    };
    let (reader, stdout_sink) = tokio::io::duplex(PIPE_CAPACITY);
    let (stdin_source, writer) = tokio::io::duplex(PIPE_CAPACITY);
    let runtime = tokio::runtime::Handle::current();
    let stdout_cancel = owner.cancel.clone();
    let stdout_runtime = runtime.clone();
    harn_parser::runtime_stack::spawn(move || {
        let mut sink = SyncIoBridge::new_with_handle(stdout_sink, stdout_runtime);
        if io::copy(&mut stdout, &mut sink).is_err() {
            stdout_cancel.store(true, Ordering::Release);
        }
    });
    let stdin_cancel = owner.cancel.clone();
    harn_parser::runtime_stack::spawn(move || {
        let mut source = SyncIoBridge::new_with_handle(stdin_source, runtime);
        if io::copy(&mut source, &mut stdin).is_err() {
            stdin_cancel.store(true, Ordering::Release);
        }
    });
    let stderr_cancel = owner.cancel.clone();
    harn_parser::runtime_stack::spawn(move || {
        // Do not retain or publish worker diagnostics that may contain input.
        drain_worker_stderr(stderr, &stderr_cancel);
    });
    connect_mcp_stdio_transport(name, reader, writer, owner, protocol_version).await
}

fn drain_worker_stderr(mut stderr: impl Read, cancel: &AtomicBool) {
    if io::copy(&mut stderr, &mut io::sink()).is_err() {
        cancel.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{install_spawner, MockProcessConfig, MockSpawner};

    #[test]
    fn discarded_diagnostics_have_no_cumulative_lifetime_quota() {
        let mut stderr = io::Cursor::new(vec![b'x'; 4 * PIPE_CAPACITY]);
        let cancel = AtomicBool::new(false);
        drain_worker_stderr(&mut stderr, &cancel);
        assert_eq!(stderr.position(), (4 * PIPE_CAPACITY) as u64);
        assert!(!cancel.load(Ordering::Acquire));
    }

    #[test]
    fn a_failed_diagnostic_pipe_still_retires_its_process_owner() {
        struct FailedPipe;
        impl Read for FailedPipe {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("fixture pipe failure"))
            }
        }
        let cancel = AtomicBool::new(false);
        drain_worker_stderr(FailedPipe, &cancel);
        assert!(cancel.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn failed_handshake_reaches_the_pipe_and_retires_the_guarded_process() {
        let mock = Arc::new(MockSpawner::new());
        let child = mock.enqueue(MockProcessConfig {
            stdout: vec![b'x'; harn_vm::mcp::MCP_MAX_STDIO_LINE_BYTES + 1],
            cleanup_report: Some(super::super::ProcessCleanupReport {
                guardian_custody_complete: true,
                ..Default::default()
            }),
            ..MockProcessConfig::running()
        });
        let _spawner = install_spawner(mock.clone());
        let result = connect_guarded_mcp_stdio(
            "guarded fixture",
            McpProcessSpec {
                program: "fixture-worker".into(),
                args: vec!["literal argument".into()],
                cwd: std::env::current_dir().unwrap(),
                environment: BTreeMap::from([("ONLY_EXPLICIT".into(), "fixture".into())]),
            },
            Some(harn_vm::mcp_protocol::PROTOCOL_VERSION),
        )
        .await;
        assert!(
            result.is_err(),
            "malformed worker must not produce a client"
        );
        assert!(child.was_killed(), "failed handshake must await retirement");
        assert!(
            !child.stdin_written().is_empty(),
            "ordinary MCP handshake was not reached"
        );
        let captured = mock.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].env_mode, EnvMode::Replace);
        assert_eq!(captured[0].env.len(), 1);
        assert_eq!(captured[0].owner_death, OwnerDeathPolicy::KillContainment);
        assert!(captured[0].configure_process_group && captured[0].use_stdin);
        assert!(matches!(captured[0].output_capture, OutputCapture::Pipe));
    }
}
