//! Windows: the process tools' children, launched inside the AppContainer.
//!
//! A `Command` cannot carry an AppContainer, so on Windows a spawn the active
//! policy confines is prepared exactly as any other (program, arguments,
//! session environment, working directory) and then launched by
//! `harn_vm::process_sandbox::spawn_confined` instead of `Command::spawn`.
//! The returned child is contained in a kill-on-close Job Object from its
//! first instruction, which is what the ordinary path's owner job provides.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::process_sandbox::{
    self, ChildInput, ChildOutput, ChildStdio, ConfinedChild, ConfinedTerminator,
    ProcessCommandConfig,
};

use super::handle::{
    ExitStatus, OutputCapture, ProcessCleanupReport, ProcessError, ProcessHandle, ProcessKiller,
    SpawnSpec, WaitOutcome,
};
use super::real::{decode_status, open_capture, PreparedSpawn};

pub(super) fn spawn(
    spec: &SpawnSpec,
    prepared: PreparedSpawn,
) -> Result<Box<dyn ProcessHandle>, ProcessError> {
    let PreparedSpawn {
        command,
        cleanup_token,
        env_cleared,
        ..
    } = prepared;
    let program = command.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut env = Vec::new();
    let mut env_remove = Vec::new();
    for (key, value) in command.get_envs() {
        let key = key.to_string_lossy().into_owned();
        match value {
            Some(value) => env.push((key, value.to_string_lossy().into_owned())),
            None => env_remove.push(key),
        }
    }
    let config = ProcessCommandConfig {
        cwd: command.get_current_dir().map(PathBuf::from),
        env,
        env_remove,
        closed_env: env_cleared,
        ..ProcessCommandConfig::default()
    };
    let stdio = child_stdio(spec)?;
    let child = process_sandbox::spawn_confined(&program, &args, &config, stdio)
        .map_err(|error| ProcessError::SandboxSetup(format!("{error:?}")))?
        .ok_or_else(|| {
            ProcessError::SandboxSetup(
                "the policy stopped confining this spawn between preparing and launching it"
                    .to_string(),
            )
        })?;
    if let Err(error) = harn_vm::op_interrupt::record_current_process_owner_group(child.id()) {
        child.terminator().terminate();
        return Err(ProcessError::Spawn(format!(
            "record process owner group: {error}"
        )));
    }
    Ok(Box::new(ConfinedProcess::new(child, cleanup_token)))
}

/// The same stream wiring `prepare_command` gives a `Command`.
fn child_stdio(spec: &SpawnSpec) -> Result<ChildStdio, ProcessError> {
    let (stdout, stderr) = match &spec.output_capture {
        OutputCapture::Inherit => (ChildOutput::Inherit, ChildOutput::Inherit),
        OutputCapture::Pipe => (ChildOutput::Pipe, ChildOutput::Pipe),
        OutputCapture::File {
            stdout_path,
            stderr_path,
        } => (
            ChildOutput::File(open_capture(stdout_path, "stdout")?),
            ChildOutput::File(open_capture(stderr_path, "stderr")?),
        ),
    };
    let stdin = match (&spec.output_capture, spec.use_stdin) {
        (OutputCapture::Inherit, true) => ChildInput::Inherit,
        (_, true) => ChildInput::Pipe,
        (_, false) => ChildInput::Null,
    };
    Ok(ChildStdio {
        stdin,
        stdout,
        stderr,
    })
}

struct ConfinedProcess {
    pid: u32,
    child: ConfinedChild,
    killer: Arc<dyn ProcessKiller>,
}

impl ConfinedProcess {
    fn new(child: ConfinedChild, cleanup_token: String) -> Self {
        let pid = child.id();
        let killer = Arc::new(ConfinedKiller {
            pid,
            cleanup_token,
            terminator: child.terminator(),
        });
        Self { pid, child, killer }
    }
}

struct ConfinedKiller {
    pid: u32,
    cleanup_token: String,
    terminator: ConfinedTerminator,
}

impl ProcessKiller for ConfinedKiller {
    fn kill(&self) -> ProcessCleanupReport {
        let report = harn_vm::op_interrupt::terminate_pid_tree_group_and_token_with_report(
            self.pid,
            Some(&self.cleanup_token),
        );
        // The Job Object holds the whole tree, including anything the
        // process-tree walk above could not see.
        self.terminator.terminate();
        report
    }
}

impl ProcessHandle for ConfinedProcess {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn process_group_id(&self) -> Option<u32> {
        None
    }

    fn killer(&self) -> Arc<dyn ProcessKiller> {
        Arc::clone(&self.killer)
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.child
            .take_stdin()
            .map(|pipe| Box::new(pipe) as Box<dyn Write + Send>)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.child
            .take_stdout()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.child
            .take_stderr()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
    }

    fn wait_with_timeout(
        &mut self,
        timeout: Option<Duration>,
        interrupt: &dyn Fn() -> bool,
    ) -> io::Result<WaitOutcome> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(WaitOutcome::Exited(decode_status(status)));
            }
            let interrupted = interrupt();
            let timed_out = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if interrupted || timed_out {
                let mut report = self.killer.kill();
                let _ = self.child.wait();
                report.refresh_survivor_status();
                return Ok(if interrupted {
                    WaitOutcome::Interrupted(report)
                } else {
                    WaitOutcome::TimedOut(report)
                });
            }
            let sleep = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::MAX)
                .min(Duration::from_millis(20));
            thread::sleep(sleep);
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().map(decode_status)
    }
}
