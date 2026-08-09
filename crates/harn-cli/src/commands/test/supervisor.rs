//! Native lifetime supervision for the conformance CLI.
//!
//! The public `harn test conformance` process remains the user-visible runner,
//! while an owner-death guardian executes the actual suite in a contained
//! process tree. Kernel EOF on the guardian's liveness pipe survives SIGKILL,
//! so detached helpers cannot outlive the public runner.

use std::collections::BTreeMap;
use std::io;

use harn_hostlib::process::{
    self, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome,
};

use crate::cli::TestArgs;

const PAYLOAD_ENV: &str = "HARN_INTERNAL_CONFORMANCE_SUPERVISED_PAYLOAD";

pub(super) fn requires_supervision(args: &TestArgs) -> bool {
    args.target.as_deref() == Some("conformance")
}

pub(super) fn is_payload() -> bool {
    std::env::var_os(PAYLOAD_ENV).is_some()
}

pub(super) async fn run_current_invocation() -> Result<i32, String> {
    tokio::task::spawn_blocking(run_current_invocation_blocking)
        .await
        .map_err(|error| format!("conformance supervisor task failed: {error}"))?
}

fn run_current_invocation_blocking() -> Result<i32, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve conformance runner executable: {error}"))?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut env = std::env::vars().collect::<BTreeMap<_, _>>();
    env.insert(PAYLOAD_ENV.to_string(), "1".to_string());
    let spec = SpawnSpec {
        builtin: "harn test conformance supervisor",
        program: executable.to_string_lossy().into_owned(),
        args,
        cwd: std::env::current_dir().ok(),
        env,
        env_remove: Vec::new(),
        env_mode: EnvMode::Patch,
        use_stdin: false,
        configure_process_group: true,
        owner_death: OwnerDeathPolicy::KillContainment,
        output_capture: OutputCapture::Pipe,
    };
    let mut child = process::spawn_process(spec)
        .map_err(|error| format!("start supervised conformance payload: {error}"))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "supervised conformance stdout pipe is missing".to_string())?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| "supervised conformance stderr pipe is missing".to_string())?;
    let stdout_task = std::thread::spawn(move || {
        let mut stdout = stdout;
        io::copy(&mut stdout, &mut io::stdout())
    });
    let stderr_task = std::thread::spawn(move || {
        let mut stderr = stderr;
        io::copy(&mut stderr, &mut io::stderr())
    });

    let outcome = child
        .wait_with_timeout(None, &|| false)
        .map_err(|error| format!("wait for supervised conformance payload: {error}"))?;
    let stdout_result = stdout_task
        .join()
        .map_err(|_| "supervised conformance stdout forwarder panicked".to_string())?;
    let stderr_result = stderr_task
        .join()
        .map_err(|_| "supervised conformance stderr forwarder panicked".to_string())?;
    stdout_result.map_err(|error| format!("forward supervised conformance stdout: {error}"))?;
    stderr_result.map_err(|error| format!("forward supervised conformance stderr: {error}"))?;

    match outcome {
        WaitOutcome::Exited(status) => Ok(status.code.unwrap_or_else(|| {
            status
                .signal
                .map_or(1, |signal| 128_i32.saturating_add(signal))
        })),
        WaitOutcome::TimedOut(_) => Err("supervised conformance payload timed out".to_string()),
        WaitOutcome::Interrupted(_) => {
            Err("supervised conformance payload interrupted".to_string())
        }
    }
}
