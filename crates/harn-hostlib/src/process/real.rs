//! Production [`ProcessSpawner`] implementation backed by
//! `std::process::Command` + `harn_vm::process_sandbox`.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::process_sandbox;

use super::handle::{
    EnvMode, ExitStatus, OutputCapture, ProcessCleanupReport, ProcessError, ProcessHandle,
    ProcessKiller, ProcessSpawner, SpawnSpec, WaitOutcome,
};

/// Spawner that produces real OS processes via `std::process::Command`.
pub struct RealSpawner;

static REAL_SPAWNER: LazyLock<Arc<dyn ProcessSpawner>> =
    LazyLock::new(|| Arc::new(RealSpawner) as Arc<dyn ProcessSpawner>);

/// Returns the singleton real spawner used as the default.
pub fn default_spawner() -> Arc<dyn ProcessSpawner> {
    Arc::clone(&REAL_SPAWNER)
}

impl ProcessSpawner for RealSpawner {
    fn spawn(&self, spec: SpawnSpec) -> Result<Box<dyn ProcessHandle>, ProcessError> {
        let (mut command, cleanup_token) = prepare_command(&spec, None)?;
        let child = command.spawn().map_err(map_spawn_error)?;

        let pid = child.id();
        let pgid = child_process_group_id(pid);
        let killer: Arc<dyn ProcessKiller> = Arc::new(RealKiller {
            pid,
            cleanup_token: cleanup_token.clone(),
        });

        Ok(Box::new(RealProcess {
            pid,
            pgid,
            cleanup_token,
            killer,
            child: Some(child),
            stdin: None,
            stdout: None,
            stderr: None,
            stdin_taken: false,
            stdout_taken: false,
            stderr_taken: false,
        }))
    }
}

fn prepare_command(
    spec: &SpawnSpec,
    cleanup_token: Option<String>,
) -> Result<(Command, String), ProcessError> {
    if spec.program.is_empty() {
        return Err(ProcessError::InvalidArgv(
            "first element of argv must be a non-empty program name".to_string(),
        ));
    }

    let mut command = process_sandbox::std_command_for(&spec.program, &spec.args)
        .map_err(|e| ProcessError::SandboxSetup(format!("{e:?}")))?;

    if let Some(cwd) = spec.cwd.as_ref() {
        process_sandbox::enforce_process_cwd(cwd)
            .map_err(|e| ProcessError::SandboxCwd(format!("{e:?}")))?;
        command.current_dir(cwd);
    }

    match spec.env_mode {
        // `Replace` starts from an empty environment, so nothing to strip.
        EnvMode::Replace => {
            command.env_clear();
        }
        // `InheritClean`/`Patch` inherit the full parent environment. Strip
        // secret-bearing variables (provider `*_API_KEY`s, `GITHUB_TOKEN`,
        // `HARN_CLOUD_API_KEY`, etc.) so build/test commands — and the model
        // that reads their stdout as the tool result — never see them.
        // Caller-supplied `env` below is applied afterward and is an
        // explicit opt-in, so it is intentionally not filtered here.
        EnvMode::InheritClean | EnvMode::Patch => {
            for (key, _) in std::env::vars_os() {
                if let Some(name) = key.to_str() {
                    if super::handle::is_sensitive_env_name(name) {
                        command.env_remove(&key);
                    }
                }
            }
        }
    }
    // Caller-requested inherited-env strips (e.g. a harness spawning a
    // child harn/burin process that must not write into the parent's
    // event-log or transcript dirs). Applied before `spec.env`, so an
    // explicitly supplied override still wins.
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    // Point the child's temp dir at a sandbox-writable, workspace-local
    // location so compiler linkers (rustc/cc/ld, Go, Swift, …) and other
    // toolchains that honor TMPDIR/TMP/TEMP don't false-fail trying to write
    // intermediates to the unwritable system /tmp under a restricted
    // sandbox profile. Applied after the caller's `spec.env` so an explicit
    // caller-set TMPDIR wins; only keys the caller did not set receive the
    // overlay. No-op when the active profile is unrestricted or no writable
    // workspace root is available. TMPDIR/TMP/TEMP are workspace paths, not
    // secrets, so this does not widen the env-secret-scrub surface above.
    for (key, value) in process_sandbox::active_workspace_tmpdir_env() {
        if spec.env.contains_key(&key) {
            continue;
        }
        command.env(key, value);
    }

    // Pin tool *message* output to a deterministic English/UTF-8 locale so
    // downstream English-diagnostic matchers (deterministic syntax repair,
    // error-signature grounding, completion/pass-fail classification) do not
    // misfire for a non-Anglosphere user whose shell localizes compiler/test
    // output. A user-inherited `LC_ALL` overrides `LC_MESSAGES`, so strip it
    // first — unless the caller pinned it. Then apply the overlay with the
    // same caller-wins rule as the TMPDIR overlay above.
    if !spec
        .env
        .contains_key(process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV)
    {
        command.env_remove(process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV);
    }
    for (key, value) in process_sandbox::deterministic_message_locale_env() {
        if spec.env.contains_key(&key) {
            continue;
        }
        command.env(key, value);
    }

    log_spawn_context(&command, spec.env_mode);

    if spec.configure_process_group {
        configure_background_process_group(&mut command);
    }
    let cleanup_token =
        cleanup_token.unwrap_or_else(harn_vm::op_interrupt::new_process_cleanup_token);
    command.env(
        harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );

    match &spec.output_capture {
        OutputCapture::Inherit => {
            command.stdout(Stdio::inherit());
            command.stderr(Stdio::inherit());
        }
        OutputCapture::Pipe => {
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        }
        OutputCapture::File {
            stdout_path,
            stderr_path,
        } => {
            let stdout = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(stdout_path)
                .map_err(|error| ProcessError::Spawn(format!("open stdout capture: {error}")))?;
            let stderr = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(stderr_path)
                .map_err(|error| ProcessError::Spawn(format!("open stderr capture: {error}")))?;
            command.stdout(Stdio::from(stdout));
            command.stderr(Stdio::from(stderr));
        }
    }
    command.stdin(match (&spec.output_capture, spec.use_stdin) {
        (OutputCapture::Inherit, true) => Stdio::inherit(),
        (_, true) => Stdio::piped(),
        (_, false) => Stdio::null(),
    });

    Ok((command, cleanup_token))
}

/// Record only the non-secret facts needed to diagnose command-resolution
/// failures. Arguments and the rest of the environment may contain credentials
/// or user data, so this boundary intentionally logs neither.
fn log_spawn_context(command: &Command, env_mode: EnvMode) {
    let program = command.get_program().to_string_lossy();
    let cwd = command
        .get_current_dir()
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let path = resolved_env_value(command, "PATH", env_mode)
        .map(|value| value.to_string_lossy().into_owned());
    tracing::debug!(
        target: "harn_hostlib::process",
        shell_or_program = %program,
        cwd = %cwd.as_deref().map_or_else(|| "<unresolved>".into(), std::path::Path::to_string_lossy),
        path = %path.as_deref().unwrap_or("<unset>"),
        env_mode = ?env_mode,
        "resolved command spawn context"
    );
}

fn resolved_env_value(
    command: &Command,
    name: &str,
    env_mode: EnvMode,
) -> Option<std::ffi::OsString> {
    for (key, value) in command.get_envs() {
        if env_key_eq(key, name) {
            return value.map(std::ffi::OsStr::to_os_string);
        }
    }
    if env_mode == EnvMode::Replace {
        None
    } else {
        std::env::var_os(name)
    }
}

fn env_key_eq(key: &std::ffi::OsStr, expected: &str) -> bool {
    #[cfg(windows)]
    {
        key.to_string_lossy().eq_ignore_ascii_case(expected)
    }
    #[cfg(not(windows))]
    {
        key == expected
    }
}

fn map_spawn_error(error: io::Error) -> ProcessError {
    if let Some(violation) = process_sandbox::process_spawn_error(&error) {
        return ProcessError::SandboxSpawn(format!("{violation:?}"));
    }
    ProcessError::Spawn(error.to_string())
}

/// Replace the current Unix process through the same prepared-command path as
/// normal hostlib spawns. A successful call never returns.
#[cfg(unix)]
pub fn replace_current_process(spec: SpawnSpec) -> Result<std::convert::Infallible, ProcessError> {
    use std::os::unix::process::CommandExt;

    super::handle::validate_process_spec(&spec)?;
    let inherited_cleanup_token = std::env::var(harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV)
        .ok()
        .filter(|token| !token.is_empty());
    let (mut command, _cleanup_token) = prepare_command(&spec, inherited_cleanup_token)?;
    Err(map_spawn_error(command.exec()))
}

struct RealProcess {
    pid: u32,
    pgid: Option<u32>,
    cleanup_token: String,
    killer: Arc<dyn ProcessKiller>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdin_taken: bool,
    stdout_taken: bool,
    stderr_taken: bool,
}

impl RealProcess {
    fn ensure_pipes_taken(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if self.stdin.is_none() && !self.stdin_taken {
                self.stdin = child.stdin.take();
            }
            if self.stdout.is_none() && !self.stdout_taken {
                self.stdout = child.stdout.take();
            }
            if self.stderr.is_none() && !self.stderr_taken {
                self.stderr = child.stderr.take();
            }
        }
    }
}

impl ProcessHandle for RealProcess {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn process_group_id(&self) -> Option<u32> {
        self.pgid
    }

    fn killer(&self) -> Arc<dyn ProcessKiller> {
        Arc::clone(&self.killer)
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.ensure_pipes_taken();
        self.stdin_taken = true;
        self.stdin
            .take()
            .map(|s| Box::new(s) as Box<dyn Write + Send>)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.ensure_pipes_taken();
        self.stdout_taken = true;
        self.stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.ensure_pipes_taken();
        self.stderr_taken = true;
        self.stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>)
    }

    fn wait_with_timeout(
        &mut self,
        timeout: Option<Duration>,
        interrupt: &dyn Fn() -> bool,
    ) -> io::Result<WaitOutcome> {
        let killer = Arc::clone(&self.killer);
        let Some(child) = self.child.as_mut() else {
            return Err(io::Error::other("child already reaped"));
        };
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            match child.try_wait()? {
                Some(status) => return Ok(WaitOutcome::Exited(decode_status(status))),
                None => {
                    if interrupt() {
                        // Scope cancellation / deadline expiry: graceful
                        // group termination (SIGTERM, grace, SIGKILL) shared
                        // with the VM-side `process.*` builtins.
                        let (_, report) =
                            harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                                child,
                                Some(&self.cleanup_token),
                            );
                        return Ok(WaitOutcome::Interrupted(report));
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        // `killer.kill()` kills the process tree/group on
                        // Unix. That path is a no-op on non-Unix targets, so
                        // also kill the child handle directly
                        // (TerminateProcess on Windows) to guarantee the
                        // subsequent `child.wait()` cannot block forever on a
                        // timed-out process.
                        let mut report = killer.kill();
                        let _ = child.kill();
                        let _ = child.wait();
                        report.refresh_survivor_status();
                        return Ok(WaitOutcome::TimedOut(report));
                    }
                    let sleep = deadline
                        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::MAX)
                        .min(Duration::from_millis(20));
                    thread::sleep(sleep);
                }
            }
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("child already reaped"))?;
        let status = child.wait()?;
        Ok(decode_status(status))
    }
}

struct RealKiller {
    pid: u32,
    cleanup_token: String,
}

impl ProcessKiller for RealKiller {
    fn kill(&self) -> ProcessCleanupReport {
        let report = harn_vm::op_interrupt::signal_pid_tree_group_and_token_with_report(
            self.pid,
            Some(&self.cleanup_token),
            9,
        );
        #[cfg(target_os = "windows")]
        terminate_process(self.pid);
        report
    }
}

#[cfg(target_os = "windows")]
fn terminate_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return;
    }
    unsafe {
        TerminateProcess(handle, 1);
        CloseHandle(handle);
    }
}

#[cfg(unix)]
fn decode_status(status: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        ExitStatus::from_code(code)
    } else if let Some(sig) = status.signal() {
        ExitStatus::from_signal(sig)
    } else {
        ExitStatus {
            code: None,
            signal: None,
        }
    }
}

#[cfg(not(unix))]
fn decode_status(status: std::process::ExitStatus) -> ExitStatus {
    ExitStatus::from_code(status.code().unwrap_or(-1))
}

pub(crate) fn child_process_group_id(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        extern "C" {
            fn getpgid(pid: i32) -> i32;
        }
        let pgid = unsafe { getpgid(pid as i32) };
        if pgid > 0 {
            Some(pgid as u32)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        Some(pid)
    }
}

pub(crate) fn configure_background_process_group(command: &mut std::process::Command) {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            extern "C" {
                fn setpgid(pid: i32, pgid: i32) -> i32;
            }
            if setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_path_prefers_the_child_override() {
        let mut command = Command::new("shell");
        command.env("PATH", "/resolved/toolchain/bin");

        assert_eq!(
            resolved_env_value(&command, "PATH", EnvMode::Patch),
            Some(std::ffi::OsString::from("/resolved/toolchain/bin"))
        );
    }

    #[test]
    fn resolved_path_honors_an_explicit_removal() {
        let mut command = Command::new("shell");
        command.env_remove("PATH");

        assert_eq!(resolved_env_value(&command, "PATH", EnvMode::Patch), None);
    }

    #[test]
    fn replace_mode_does_not_report_an_inherited_path() {
        let command = Command::new("shell");

        assert_eq!(resolved_env_value(&command, "PATH", EnvMode::Replace), None);
    }
}
