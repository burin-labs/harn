use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::orchestration::RunExecutionRecord;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HARN_REPLAY_ENV: &str = "HARN_REPLAY";

thread_local! {
    pub(crate) static VM_SOURCE_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static VM_EXECUTION_CONTEXT: RefCell<Option<RunExecutionRecord>> = const { RefCell::new(None) };
}

/// Set the source directory for the current thread (called by VM on file execution).
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = Some(normalize_context_path(dir)));
}

pub(crate) fn normalize_context_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

pub fn set_thread_execution_context(context: Option<RunExecutionRecord>) {
    VM_EXECUTION_CONTEXT.with(|current| *current.borrow_mut() = context);
}

pub(crate) fn current_execution_context() -> Option<RunExecutionRecord> {
    VM_EXECUTION_CONTEXT.with(|current| current.borrow().clone())
}

/// Reset thread-local process state (for test isolation).
pub(crate) fn reset_process_state() {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = None);
    VM_EXECUTION_CONTEXT.with(|current| *current.borrow_mut() = None);
}

pub fn execution_root_path() -> PathBuf {
    current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn source_root_path() -> PathBuf {
    VM_SOURCE_DIR
        .with(|sd| sd.borrow().clone())
        .or_else(|| {
            current_execution_context().and_then(|context| context.source_dir.map(PathBuf::from))
        })
        .or_else(|| current_execution_context().and_then(|context| context.cwd.map(PathBuf::from)))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn asset_root_path() -> PathBuf {
    source_root_path()
}

fn env_override(name: &str) -> Option<String> {
    (name == HARN_REPLAY_ENV && crate::triggers::dispatcher::current_dispatch_is_replay())
        .then(|| "1".to_string())
}

pub(crate) fn read_env_value(name: &str) -> Option<String> {
    env_override(name)
        .or_else(|| current_execution_context().and_then(|context| context.env.get(name).cloned()))
        .or_else(|| std::env::var(name).ok())
}

pub fn runtime_root_base() -> PathBuf {
    find_project_root(&execution_root_path())
        .or_else(|| find_project_root(&source_root_path()))
        .unwrap_or_else(source_root_path)
}

/// Lexically collapse `..` components in `path`. Returns `None` if a
/// `..` would pop a non-Normal component (i.e. the path tries to walk
/// above its root anchor). This is a pure-string canonicalization that
/// does NOT hit the filesystem — symlinks are not followed.
fn lexically_collapse(path: &std::path::Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let popped = out.pop();
                if !matches!(popped, Some(Component::Normal(_))) {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

pub fn resolve_source_relative_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    let root = execution_root_path();
    let joined = root.join(&candidate);
    // Defense-in-depth path-traversal check (paired with the deferred
    // F3 sandbox-by-default fix): refuse to resolve a path that
    // escapes the project root via `..` components. We anchor against
    // `runtime_root_base()` (the project root), which is broader than
    // `execution_root_path()` and lets benign sibling-dir walks like
    // `read_file("../fixtures/payload.json")` from `tests/` succeed.
    if path_escapes_project_root(&joined) {
        return root.join("__harn_rejected_parent_dir_traversal__");
    }
    joined
}

pub fn resolve_source_asset_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    let root = asset_root_path();
    let joined = root.join(&candidate);
    if path_escapes_project_root(&joined) {
        return root.join("__harn_rejected_parent_dir_traversal__");
    }
    joined
}

/// Returns `true` when `joined` (which may contain raw `..`
/// components) cannot be lexically collapsed without popping past its
/// root component — i.e. the relative input had more `..` than the
/// joined depth allows, escaping the filesystem root.
///
/// This is intentionally a narrow check: it doesn't try to enforce
/// that the path stays inside a logical "project root", because the
/// project root isn't always reliably resolvable (and benign uses
/// like `../fixtures/x.json` from a `tests/` subdir are legitimate).
/// The sandbox layer remains the authoritative defense for arbitrary
/// `..` traversal; this guard plugs the most egregious escapes
/// (`../../../../etc/passwd`) for the no-sandbox-by-default
/// `harn run` path.
fn path_escapes_project_root(joined: &std::path::Path) -> bool {
    lexically_collapse(joined).is_none()
}

pub(crate) fn register_process_builtins(vm: &mut Vm) {
    for def in PROCESS_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "env(name: string) -> string?", category = "process")]
fn env_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(value) = read_env_value(&name) {
        return Ok(VmValue::String(std::sync::Arc::from(value)));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "env_or(name: string, default: any) -> any",
    category = "process"
)]
fn env_or_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
    if let Some(value) = read_env_value(&name) {
        return Ok(VmValue::String(std::sync::Arc::from(value)));
    }
    Ok(default)
}

#[harn_builtin(sig = "exit(code?: int) -> never", category = "process")]
fn exit_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
    std::process::exit(code as i32);
}

#[harn_builtin(sig = "exec(...command: string) -> dict", category = "process")]
fn exec_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "exec: command is required",
        ))));
    }
    let cmd = args[0].display();
    let cmd_args: Vec<String> = args[1..].iter().map(|a| a.display()).collect();
    let output = exec_command(None, &cmd, &cmd_args)?;
    Ok(vm_output_to_value(output))
}

#[harn_builtin(sig = "shell(command: string) -> dict", category = "process")]
fn shell_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let cmd = args.first().map(|a| a.display()).unwrap_or_default();
    if cmd.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "shell: command string is required",
        ))));
    }
    let invocation = crate::shells::default_shell_invocation(&cmd)
        .map_err(|error| VmError::Runtime(format!("shell: {error}")))?;
    let output = exec_shell_args(None, &invocation.program, &invocation.args)?;
    Ok(vm_output_to_value(output))
}

#[harn_builtin(
    sig = "exec_at(dir: string, ...command: string) -> dict",
    category = "process"
)]
fn exec_at_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "exec_at: directory and command are required",
        ))));
    }
    let dir = args[0].display();
    let cmd = args[1].display();
    let cmd_args: Vec<String> = args[2..].iter().map(|a| a.display()).collect();
    let output = exec_command(Some(dir.as_str()), &cmd, &cmd_args)?;
    Ok(vm_output_to_value(output))
}

#[harn_builtin(
    sig = "shell_at(dir: string, command: string) -> dict",
    category = "process"
)]
fn shell_at_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "shell_at: directory and command string are required",
        ))));
    }
    let dir = args[0].display();
    let cmd = args[1].display();
    if cmd.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "shell_at: command string is required",
        ))));
    }
    let invocation = crate::shells::default_shell_invocation(&cmd)
        .map_err(|error| VmError::Runtime(format!("shell_at: {error}")))?;
    let output = exec_shell_args(Some(dir.as_str()), &invocation.program, &invocation.args)?;
    Ok(vm_output_to_value(output))
}

#[harn_builtin(sig = "username(...args: any) -> string", category = "process")]
fn username_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(user)))
}

#[harn_builtin(sig = "hostname() -> string", category = "process")]
fn hostname_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .ok_or(std::env::VarError::NotPresent)
        })
        .unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(name)))
}

#[harn_builtin(sig = "platform(...args: any) -> string", category = "process")]
fn platform_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        std::env::consts::OS
    };
    Ok(VmValue::String(std::sync::Arc::from(os)))
}

#[harn_builtin(sig = "arch() -> string", category = "process")]
fn arch_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(std::sync::Arc::from(
        std::env::consts::ARCH,
    )))
}

#[harn_builtin(sig = "home_dir() -> string", category = "process")]
fn home_dir_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let home = crate::user_dirs::home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(home)))
}

#[harn_builtin(sig = "pid(...args: any) -> int", category = "process")]
fn pid_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Int(std::process::id() as i64))
}

#[harn_builtin(sig = "date_iso() -> string", category = "process")]
fn date_iso_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // `date_iso` reads the OS wall clock directly (it predates the
    // unified `clock_mock`). Routing through `leak_audit::wall_now`
    // keeps the production behavior unchanged but surfaces the call
    // in `testbench_clock_leaks()` whenever a script invokes it
    // under a paused testbench session, so fidelity hazards are
    // visible instead of silently corrupting tapes.
    let now = crate::clock_mock::leak_audit::wall_now("stdlib/date_iso");
    let dt: chrono::DateTime<chrono::Utc> = now.into();
    Ok(VmValue::String(std::sync::Arc::from(
        dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )))
}

#[harn_builtin(sig = "cwd() -> string", category = "process")]
fn cwd_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = current_execution_context()
        .and_then(|context| context.cwd)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(dir)))
}

#[harn_builtin(sig = "execution_root() -> string", category = "process")]
fn execution_root_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(std::sync::Arc::from(
        execution_root_path().to_string_lossy().into_owned(),
    )))
}

#[harn_builtin(sig = "asset_root() -> string", category = "process")]
fn asset_root_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(std::sync::Arc::from(
        asset_root_path().to_string_lossy().into_owned(),
    )))
}

#[harn_builtin(sig = "runtime_paths() -> dict", category = "process")]
fn runtime_paths_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let runtime_base = runtime_root_base();
    let mut paths = BTreeMap::new();
    paths.put_str("execution_root", execution_root_path().to_string_lossy());
    paths.put_str("asset_root", asset_root_path().to_string_lossy());
    paths.put_str(
        "state_root",
        crate::runtime_paths::state_root(&runtime_base).to_string_lossy(),
    );
    paths.put_str(
        "run_root",
        crate::runtime_paths::run_root(&runtime_base).to_string_lossy(),
    );
    paths.put_str(
        "worktree_root",
        crate::runtime_paths::worktree_root(&runtime_base).to_string_lossy(),
    );
    Ok(VmValue::Dict(std::sync::Arc::new(paths)))
}

#[harn_builtin(sig = "spawn_captured(opts: dict) -> dict", category = "process")]
fn spawn_captured_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    spawn_captured_value(args)
}

// `term_width()` / `term_height()` return the current terminal
// dimensions in columns and rows. Reads `COLUMNS` / `LINES` env vars
// first (so test harnesses can pin a value), falls back to the
// platform `ioctl` size, and finally defaults to 80x24 when neither
// is available (e.g. when stdout is not a TTY). These are the
// free-builtin aliases for `harness.term.width()` /
// `harness.term.height()`. `std/tui` already exposes
// `__tui_terminal_width` for its renderer; these aliases keep
// ported subcommands working without importing the tui module.
#[harn_builtin(sig = "term_width() -> int", category = "process")]
fn term_width_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Int(crate::term::width() as i64))
}

#[harn_builtin(sig = "term_height() -> int", category = "process")]
fn term_height_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Int(crate::term::height() as i64))
}

const PROCESS_BUILTINS: &[&VmBuiltinDef] = &[
    &ENV_IMPL_DEF,
    &ENV_OR_IMPL_DEF,
    &EXIT_IMPL_DEF,
    &EXEC_IMPL_DEF,
    &EXEC_OPTS_IMPL_DEF,
    &SHELL_IMPL_DEF,
    &EXEC_AT_IMPL_DEF,
    &EXEC_AT_OPTS_IMPL_DEF,
    &SHELL_AT_IMPL_DEF,
    &USERNAME_IMPL_DEF,
    &HOSTNAME_IMPL_DEF,
    &PLATFORM_IMPL_DEF,
    &ARCH_IMPL_DEF,
    &HOME_DIR_IMPL_DEF,
    &PID_IMPL_DEF,
    &DATE_ISO_IMPL_DEF,
    &CWD_IMPL_DEF,
    &EXECUTION_ROOT_IMPL_DEF,
    &ASSET_ROOT_IMPL_DEF,
    &RUNTIME_PATHS_IMPL_DEF,
    &SPAWN_CAPTURED_IMPL_DEF,
    &TERM_WIDTH_IMPL_DEF,
    &TERM_HEIGHT_IMPL_DEF,
];

/// Run an external command synchronously and return captured output.
///
/// Shared by the legacy free builtin and `harness.process.spawn_captured` so
/// subprocess capture has one implementation and one result shape.
pub(crate) fn spawn_captured_value(args: &[VmValue]) -> Result<VmValue, VmError> {
    let opts = match args.first() {
        Some(VmValue::Dict(opts)) => opts.clone(),
        _ => {
            return Err(VmError::Runtime(
                "spawn_captured: options dict is required".to_string(),
            ));
        }
    };
    let cmd = match opts.get("cmd").map(|v| v.display()).unwrap_or_default() {
        s if s.is_empty() => {
            return Err(VmError::Runtime(
                "spawn_captured: opts.cmd is required".to_string(),
            ));
        }
        s => s,
    };
    let cmd_args: Vec<String> = match opts.get("args") {
        Some(VmValue::List(items)) => items.iter().map(|v| v.display()).collect(),
        None | Some(VmValue::Nil) => Vec::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "spawn_captured: opts.args must be a list of strings, got {}",
                other.type_name()
            )));
        }
    };
    let cwd = opts
        .get("cwd")
        .map(|v| v.display())
        .filter(|s| !s.is_empty());
    let env_overrides: Vec<(String, String)> = match opts.get("env") {
        Some(VmValue::Dict(env)) => env.iter().map(|(k, v)| (k.clone(), v.display())).collect(),
        None | Some(VmValue::Nil) => Vec::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "spawn_captured: opts.env must be a dict, got {}",
                other.type_name()
            )));
        }
    };
    let stdin_bytes: Option<Vec<u8>> = match opts.get("stdin") {
        Some(VmValue::Bytes(bytes)) => Some(bytes.as_slice().to_vec()),
        Some(VmValue::String(s)) => Some(s.as_bytes().to_vec()),
        None | Some(VmValue::Nil) => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "spawn_captured: opts.stdin must be string or bytes, got {}",
                other.type_name()
            )));
        }
    };
    let timeout = opts
        .get("timeout_ms")
        .and_then(|v| v.as_int())
        .filter(|n| *n > 0)
        .map(|n| Duration::from_millis(n as u64));

    let spawn = CapturedSpawn {
        label: "spawn_captured",
        cmd: &cmd,
        args: &cmd_args,
        cwd: cwd.as_deref(),
        env: &env_overrides,
        // `spawn_captured` has always layered `env` over the inherited
        // parent environment, so keep that merge behavior.
        env_clear: false,
        stdin: stdin_bytes,
        timeout,
    };
    let CapturedRun {
        output,
        timed_out,
        duration_ms,
    } = run_captured_spawn(spawn)?;

    let exit_code = if timed_out {
        -1
    } else {
        output.status.code().unwrap_or(-1) as i64
    };
    let success = if timed_out {
        false
    } else {
        output.status.success()
    };
    let mut result = BTreeMap::new();
    result.insert("exit_code".to_string(), VmValue::Int(exit_code));
    result.put_str("stdout", String::from_utf8_lossy(&output.stdout).as_ref());
    result.put_str("stderr", String::from_utf8_lossy(&output.stderr).as_ref());
    result.insert("duration_ms".to_string(), VmValue::Int(duration_ms));
    result.insert("success".to_string(), VmValue::Bool(success));
    result.insert("timed_out".to_string(), VmValue::Bool(timed_out));
    Ok(VmValue::Dict(std::sync::Arc::new(result)))
}

/// Parameters for [`run_captured_spawn`]: a single synchronous subprocess
/// spawn that captures stdout/stderr, optionally feeds stdin, optionally
/// enforces a wall-clock timeout, and either merges (`env_clear == false`)
/// or replaces (`env_clear == true`) the parent environment with `env`.
struct CapturedSpawn<'a> {
    label: &'static str,
    cmd: &'a str,
    args: &'a [String],
    cwd: Option<&'a str>,
    env: &'a [(String, String)],
    env_clear: bool,
    stdin: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

/// Result of [`run_captured_spawn`].
struct CapturedRun {
    output: std::process::Output,
    timed_out: bool,
    duration_ms: i64,
}

/// Shared synchronous spawn-and-capture core used by `spawn_captured` and the
/// `exec_opts`/`exec_at_opts` convenience builtins. Honors cwd, an env
/// overlay (merge or replace via `env_clear`), optional stdin, and an optional
/// wall-clock timeout (after which the child is killed and `timed_out` is set).
fn run_captured_spawn(spec: CapturedSpawn<'_>) -> Result<CapturedRun, VmError> {
    let label = spec.label;
    let mut command = std::process::Command::new(spec.cmd);
    command.args(spec.args);
    if let Some(cwd) = spec.cwd {
        command.current_dir(cwd);
    }
    if spec.env_clear {
        command.env_clear();
    }
    for (key, value) in spec.env {
        command.env(key, value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if spec.stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }

    let started = Instant::now();
    let cmd = spec.cmd;
    let mut child = command.spawn().map_err(|error| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "{label}: failed to spawn '{cmd}': {error}"
        ))))
    })?;

    if let (Some(payload), Some(mut stdin)) = (spec.stdin, child.stdin.take()) {
        // Children may close stdin early while still producing useful output.
        let _ = stdin.write_all(&payload);
    }

    let (output, timed_out) = match spec.timeout {
        None => match child.wait_with_output() {
            Ok(output) => (output, false),
            Err(error) => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!("{label}: wait failed: {error}"),
                ))));
            }
        },
        Some(limit) => {
            let deadline = started + limit;
            let mut timed_out = false;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            timed_out = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                            format!("{label}: poll failed: {error}"),
                        ))));
                    }
                }
            }
            if timed_out {
                let stdout_handle = child.stdout.take();
                let stderr_handle = child.stderr.take();
                let (tx_out, rx_out) = mpsc::channel::<Vec<u8>>();
                let (tx_err, rx_err) = mpsc::channel::<Vec<u8>>();
                if let Some(mut s) = stdout_handle {
                    std::thread::spawn(move || {
                        use std::io::Read as _;
                        let mut buf = Vec::new();
                        let _ = s.read_to_end(&mut buf);
                        let _ = tx_out.send(buf);
                    });
                }
                if let Some(mut s) = stderr_handle {
                    std::thread::spawn(move || {
                        use std::io::Read as _;
                        let mut buf = Vec::new();
                        let _ = s.read_to_end(&mut buf);
                        let _ = tx_err.send(buf);
                    });
                }
                let stdout = rx_out
                    .recv_timeout(Duration::from_millis(100))
                    .unwrap_or_default();
                let stderr = rx_err
                    .recv_timeout(Duration::from_millis(100))
                    .unwrap_or_default();
                (
                    std::process::Output {
                        status: std::process::ExitStatus::default(),
                        stdout,
                        stderr,
                    },
                    true,
                )
            } else {
                match child.wait_with_output() {
                    Ok(output) => (output, false),
                    Err(error) => {
                        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                            format!("{label}: wait failed: {error}"),
                        ))));
                    }
                }
            }
        }
    };

    Ok(CapturedRun {
        output,
        timed_out,
        duration_ms: started.elapsed().as_millis() as i64,
    })
}

/// Parsed `exec_opts` / `exec_at_opts` options, ready to populate a
/// [`CapturedSpawn`].
#[derive(Default)]
struct ExecOptions {
    env: Vec<(String, String)>,
    env_clear: bool,
    cwd: Option<String>,
    timeout: Option<Duration>,
}

/// Extract `exec_opts` / `exec_at_opts` options into an [`ExecOptions`].
///
/// `env_mode` mirrors the `process.exec` host op (and the env-clear footgun
/// fix): the default is `"merge"` (overlay `env` keys on the inherited parent
/// environment, keeping PATH/HOME/etc.); `"replace"` clears the parent
/// environment first so only the provided keys remain.
fn exec_options(label: &str, options: Option<&VmValue>) -> Result<ExecOptions, VmError> {
    let opts = match options {
        None | Some(VmValue::Nil) => return Ok(ExecOptions::default()),
        Some(VmValue::Dict(opts)) => opts.clone(),
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!("{label}: options must be a dict, got {}", other.type_name()),
            ))));
        }
    };
    let env: Vec<(String, String)> = match opts.get("env") {
        Some(VmValue::Dict(env)) => env.iter().map(|(k, v)| (k.clone(), v.display())).collect(),
        None | Some(VmValue::Nil) => Vec::new(),
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "{label}: options.env must be a dict, got {}",
                    other.type_name()
                ),
            ))));
        }
    };
    let env_clear = match opts.get("env_mode").map(|v| v.display()).as_deref() {
        None | Some("merge") => false,
        Some("replace") => true,
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "{label}: options.env_mode must be \"merge\" or \"replace\", got {other:?}"
                ),
            ))));
        }
    };
    let cwd = opts
        .get("cwd")
        .map(|v| v.display())
        .filter(|s| !s.is_empty());
    // Accept both `timeout` and `timeout_ms` (millis), matching the
    // `process.exec` host op's tolerance.
    let timeout = opts
        .get("timeout")
        .or_else(|| opts.get("timeout_ms"))
        .and_then(|v| v.as_int())
        .filter(|n| *n > 0)
        .map(|n| Duration::from_millis(n as u64));
    Ok(ExecOptions {
        env,
        env_clear,
        cwd,
        timeout,
    })
}

/// Build the `exec`-shaped result dict (`stdout`/`stderr`/`status`/`success`)
/// and additionally surface `timed_out` so options-form callers can detect a
/// timeout kill without inspecting the exit status.
fn captured_run_to_value(run: &CapturedRun) -> VmValue {
    let status = if run.timed_out {
        -1
    } else {
        run.output.status.code().unwrap_or(-1) as i64
    };
    let success = !run.timed_out && run.output.status.success();
    let mut result = BTreeMap::new();
    result.put_str(
        "stdout",
        String::from_utf8_lossy(&run.output.stdout).as_ref(),
    );
    result.put_str(
        "stderr",
        String::from_utf8_lossy(&run.output.stderr).as_ref(),
    );
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("success".to_string(), VmValue::Bool(success));
    result.insert("timed_out".to_string(), VmValue::Bool(run.timed_out));
    result.insert("duration_ms".to_string(), VmValue::Int(run.duration_ms));
    VmValue::Dict(std::sync::Arc::new(result))
}

#[harn_builtin(
    sig = "exec_opts(command: list, options: dict?) -> dict",
    category = "process"
)]
fn exec_opts_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let command = exec_opts_command("exec_opts", args.first())?;
    let opts = exec_options("exec_opts", args.get(1))?;
    let run = run_captured_spawn(CapturedSpawn {
        label: "exec_opts",
        cmd: &command[0],
        args: &command[1..],
        cwd: opts.cwd.as_deref(),
        env: &opts.env,
        env_clear: opts.env_clear,
        stdin: None,
        timeout: opts.timeout,
    })?;
    Ok(captured_run_to_value(&run))
}

#[harn_builtin(
    sig = "exec_at_opts(dir: string, command: list, options: dict?) -> dict",
    category = "process"
)]
fn exec_at_opts_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = match args.first() {
        Some(value) if !value.display().is_empty() => value.display(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "exec_at_opts: directory is required",
            ))));
        }
    };
    let command = exec_opts_command("exec_at_opts", args.get(1))?;
    let opts = exec_options("exec_at_opts", args.get(2))?;
    // The positional `dir` argument is the working directory; an explicit
    // `options.cwd` (rare) overrides it so callers retain full control.
    let resolved_cwd = opts.cwd.unwrap_or(dir);
    let run = run_captured_spawn(CapturedSpawn {
        label: "exec_at_opts",
        cmd: &command[0],
        args: &command[1..],
        cwd: Some(resolved_cwd.as_str()),
        env: &opts.env,
        env_clear: opts.env_clear,
        stdin: None,
        timeout: opts.timeout,
    })?;
    Ok(captured_run_to_value(&run))
}

/// Validate the `command` argument shared by `exec_opts`/`exec_at_opts`: a
/// non-empty list whose first element is a non-empty program name.
fn exec_opts_command(label: &str, value: Option<&VmValue>) -> Result<Vec<String>, VmError> {
    let items = match value {
        Some(VmValue::List(items)) => items,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!("{label}: command must be a non-empty list of strings"),
            ))));
        }
    };
    let command: Vec<String> = items.iter().map(|v| v.display()).collect();
    if command.is_empty() || command[0].is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{label}: command must be a non-empty list of strings"),
        ))));
    }
    Ok(command)
}

/// Find the project root by walking up from a base directory looking for harn.toml.
pub fn find_project_root(base: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = base.to_path_buf();
    loop {
        if dir.join("harn.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Register builtins that depend on source directory context.
pub(crate) fn register_path_builtins(vm: &mut Vm) {
    for def in PATH_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "source_dir(...args: any) -> string", category = "process")]
fn source_dir_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
    match dir {
        Some(d) => Ok(VmValue::String(std::sync::Arc::from(
            d.to_string_lossy().into_owned(),
        ))),
        None => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(VmValue::String(std::sync::Arc::from(cwd)))
        }
    }
}

#[harn_builtin(sig = "project_root() -> string?", category = "process")]
fn project_root_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let base = current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| VM_SOURCE_DIR.with(|sd| sd.borrow().clone()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    match find_project_root(&base) {
        Some(root) => Ok(VmValue::String(std::sync::Arc::from(
            root.to_string_lossy().into_owned(),
        ))),
        None => Ok(VmValue::Nil),
    }
}

const PATH_BUILTINS: &[&VmBuiltinDef] = &[&SOURCE_DIR_IMPL_DEF, &PROJECT_ROOT_IMPL_DEF];

fn vm_output_to_value(output: std::process::Output) -> VmValue {
    let mut result = BTreeMap::new();
    result.put_str("stdout", String::from_utf8_lossy(&output.stdout).as_ref());
    result.put_str("stderr", String::from_utf8_lossy(&output.stderr).as_ref());
    result.insert(
        "status".to_string(),
        VmValue::Int(output.status.code().unwrap_or(-1) as i64),
    );
    result.insert(
        "success".to_string(),
        VmValue::Bool(output.status.success()),
    );
    VmValue::Dict(std::sync::Arc::new(result))
}

fn exec_command(
    dir: Option<&str>,
    cmd: &str,
    args: &[String],
) -> Result<std::process::Output, VmError> {
    let config = process_command_config(dir)?;
    crate::stdlib::sandbox::command_output(cmd, args, &config)
        .map_err(|error| prefix_process_error(error, "exec"))
}

#[cfg(test)]
fn exec_shell(
    dir: Option<&str>,
    shell: &str,
    flag: &str,
    script: &str,
) -> Result<std::process::Output, VmError> {
    let args = vec![flag.to_string(), script.to_string()];
    exec_shell_args(dir, shell, &args)
}

fn exec_shell_args(
    dir: Option<&str>,
    shell: &str,
    args: &[String],
) -> Result<std::process::Output, VmError> {
    let config = process_command_config(dir)?;
    crate::stdlib::sandbox::command_output(shell, args, &config)
        .map_err(|error| prefix_process_error(error, "shell"))
}

fn process_command_config(
    dir: Option<&str>,
) -> Result<crate::stdlib::sandbox::ProcessCommandConfig, VmError> {
    let mut config = crate::stdlib::sandbox::ProcessCommandConfig {
        stdin_null: true,
        ..Default::default()
    };
    if let Some(dir) = dir {
        let resolved = resolve_command_dir(dir);
        crate::stdlib::sandbox::enforce_process_cwd(&resolved)?;
        config.cwd = Some(resolved);
    } else if let Some(context) = current_execution_context() {
        if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
            crate::stdlib::sandbox::enforce_process_cwd(std::path::Path::new(&cwd))?;
            config.cwd = Some(std::path::PathBuf::from(cwd));
        }
        if !context.env.is_empty() {
            config.env.extend(context.env);
        }
    }
    if let Some(value) = env_override(HARN_REPLAY_ENV) {
        config.env.push((HARN_REPLAY_ENV.to_string(), value));
    }
    Ok(config)
}

fn prefix_process_error(error: VmError, prefix: &str) -> VmError {
    match error {
        VmError::Thrown(VmValue::String(message)) => VmError::Thrown(VmValue::String(
            std::sync::Arc::from(format!("{prefix} failed: {message}")),
        )),
        other => other,
    }
}

fn resolve_command_dir(dir: &str) -> PathBuf {
    let candidate = PathBuf::from(dir);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(cwd) = current_execution_context().and_then(|context| context.cwd) {
        return PathBuf::from(cwd).join(candidate);
    }
    if let Some(source_dir) = VM_SOURCE_DIR.with(|sd| sd.borrow().clone()) {
        return source_dir.join(candidate);
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RuntimePathsEnvGuard {
        state: Option<String>,
        run: Option<String>,
        worktree: Option<String>,
    }

    impl RuntimePathsEnvGuard {
        fn capture() -> Self {
            Self {
                state: std::env::var(crate::runtime_paths::HARN_STATE_DIR_ENV).ok(),
                run: std::env::var(crate::runtime_paths::HARN_RUN_DIR_ENV).ok(),
                worktree: std::env::var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV).ok(),
            }
        }
    }

    impl Drop for RuntimePathsEnvGuard {
        fn drop(&mut self) {
            match self.state.as_deref() {
                Some(value) => std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, value),
                None => std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV),
            }
            match self.run.as_deref() {
                Some(value) => std::env::set_var(crate::runtime_paths::HARN_RUN_DIR_ENV, value),
                None => std::env::remove_var(crate::runtime_paths::HARN_RUN_DIR_ENV),
            }
            match self.worktree.as_deref() {
                Some(value) => {
                    std::env::set_var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV, value);
                }
                None => std::env::remove_var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV),
            }
        }
    }

    #[test]
    fn lexically_collapse_resolves_sibling_walk() {
        let path = PathBuf::from("/tmp/project/tests/../fixtures/x.json");
        let collapsed = lexically_collapse(&path).expect("sibling walk");
        assert_eq!(collapsed, PathBuf::from("/tmp/project/fixtures/x.json"));
    }

    #[test]
    fn lexically_collapse_blocks_escape_past_root() {
        // `/app/../etc/passwd` would lexically resolve to `/etc/passwd`,
        // but the pop hits a RootDir which is not Normal — refuse.
        let path = PathBuf::from("/app/../../etc/passwd");
        assert!(lexically_collapse(&path).is_none());
    }

    #[test]
    fn lexically_collapse_strips_curdir() {
        let path = PathBuf::from("/app/./logs/today.txt");
        let collapsed = lexically_collapse(&path).expect("curdir is benign");
        assert_eq!(collapsed, PathBuf::from("/app/logs/today.txt"));
    }

    #[test]
    fn resolve_source_relative_path_blocks_obvious_escape() {
        let dir =
            std::env::temp_dir().join(format!("harn-process-escape-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        set_thread_source_dir(&dir);
        set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
            cwd: Some(dir.to_string_lossy().into_owned()),
            source_dir: Some(dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }));
        // A long string of `..` should escape the temp-root and trip
        // the rejection sentinel, so the file read fails NotFound
        // instead of escaping to a different filesystem location.
        let resolved = resolve_source_relative_path("../../../../../../../../etc/passwd");
        assert!(
            resolved
                .to_string_lossy()
                .contains("__harn_rejected_parent_dir_traversal__"),
            "expected rejection sentinel, got {resolved:?}"
        );
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_source_relative_path_ignores_thread_source_dir_without_execution_context() {
        let dir = std::env::temp_dir().join(format!("harn-process-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let current_dir = std::env::current_dir().unwrap();
        set_thread_source_dir(&dir);
        let resolved = resolve_source_relative_path("templates/prompt.txt");
        assert_eq!(resolved, current_dir.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_source_relative_path_prefers_execution_cwd_over_source_dir() {
        let cwd = std::env::temp_dir().join(format!("harn-process-cwd-{}", uuid::Uuid::now_v7()));
        let source_dir =
            std::env::temp_dir().join(format!("harn-process-source-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        set_thread_source_dir(&source_dir);
        set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            source_dir: Some(source_dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }));
        let resolved = resolve_source_relative_path("templates/prompt.txt");
        assert_eq!(resolved, cwd.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn resolve_source_asset_path_prefers_execution_source_dir_over_cwd() {
        let cwd = std::env::temp_dir().join(format!("harn-asset-cwd-{}", uuid::Uuid::now_v7()));
        let source_dir =
            std::env::temp_dir().join(format!("harn-asset-source-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        set_thread_source_dir(&source_dir);
        set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            source_dir: Some(source_dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }));
        let resolved = resolve_source_asset_path("templates/prompt.txt");
        assert_eq!(resolved, source_dir.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn set_thread_source_dir_absolutizes_relative_paths() {
        reset_process_state();
        let current_dir = std::env::current_dir().unwrap();
        set_thread_source_dir(std::path::Path::new("scripts"));
        assert_eq!(source_root_path(), current_dir.join("scripts"));
        reset_process_state();
    }

    #[test]
    fn exec_context_sets_default_cwd_and_env() {
        let dir = std::env::temp_dir().join(format!("harn-process-ctx-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "ok").unwrap();
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: BTreeMap::from([("HARN_PROCESS_TEST".to_string(), "present".to_string())]),
            ..Default::default()
        }));
        let output = exec_shell(
            None,
            "sh",
            "-c",
            "printf '%s:' \"$HARN_PROCESS_TEST\" && test -f marker.txt",
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "present:");
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_at_resolves_relative_to_execution_cwd() {
        let dir = std::env::temp_dir().join(format!("harn-process-rel-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("marker.txt"), "ok").unwrap();
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        }));
        let output = exec_shell(Some("nested"), "sh", "-c", "test -f marker.txt").unwrap();
        assert!(output.status.success());
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_paths_uses_configurable_state_roots() {
        let _runtime_paths_env_lock = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env_guard = RuntimePathsEnvGuard::capture();
        let base =
            std::env::temp_dir().join(format!("harn-process-runtime-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, ".custom-harn");
        std::env::set_var(crate::runtime_paths::HARN_RUN_DIR_ENV, ".custom-runs");
        std::env::set_var(
            crate::runtime_paths::HARN_WORKTREE_DIR_ENV,
            ".custom-worktrees",
        );
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(base.to_string_lossy().into_owned()),
            ..Default::default()
        }));

        let mut vm = crate::vm::Vm::new();
        register_process_builtins(&mut vm);
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get("runtime_paths")
            .expect("runtime_paths builtin");
        let paths = match builtin(&[], &mut out).unwrap() {
            VmValue::Dict(map) => map,
            other => panic!("expected dict, got {other:?}"),
        };
        assert_eq!(
            paths.get("state_root").unwrap().display(),
            base.join(".custom-harn").display().to_string()
        );
        assert_eq!(
            paths.get("run_root").unwrap().display(),
            base.join(".custom-runs").display().to_string()
        );
        assert_eq!(
            paths.get("worktree_root").unwrap().display(),
            base.join(".custom-worktrees").display().to_string()
        );

        reset_process_state();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    fn exec_opts_list(items: &[&str]) -> VmValue {
        VmValue::List(std::sync::Arc::new(
            items
                .iter()
                .map(|s| VmValue::String(std::sync::Arc::from(*s)))
                .collect(),
        ))
    }

    #[cfg(unix)]
    fn exec_opts_dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(std::sync::Arc::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn exec_opts_merges_env_with_parent_by_default() {
        std::env::set_var("HARN_EXEC_OPTS_PARENT", "from-parent");
        let env = exec_opts_dict(&[("CHILD", VmValue::String(std::sync::Arc::from("from-child")))]);
        let args = vec![
            exec_opts_list(&[
                "/bin/sh",
                "-c",
                "printf '%s|%s' \"$HARN_EXEC_OPTS_PARENT\" \"$CHILD\"",
            ]),
            exec_opts_dict(&[("env", env)]),
        ];
        let mut out = String::new();
        let result = exec_opts_impl(&args, &mut out).expect("exec_opts result");
        let dict = result.as_dict().expect("dict");
        assert_eq!(
            dict.get("stdout").unwrap().display(),
            "from-parent|from-child"
        );
        assert!(matches!(dict.get("success"), Some(VmValue::Bool(true))));
        std::env::remove_var("HARN_EXEC_OPTS_PARENT");
    }

    #[cfg(unix)]
    #[test]
    fn exec_opts_replace_env_clears_parent() {
        std::env::set_var("HARN_EXEC_OPTS_PARENT2", "from-parent");
        let env = exec_opts_dict(&[("CHILD", VmValue::String(std::sync::Arc::from("from-child")))]);
        let args = vec![
            exec_opts_list(&[
                "/bin/sh",
                "-c",
                "printf '%s|%s' \"$HARN_EXEC_OPTS_PARENT2\" \"$CHILD\"",
            ]),
            exec_opts_dict(&[
                ("env", env),
                ("env_mode", VmValue::String(std::sync::Arc::from("replace"))),
            ]),
        ];
        let mut out = String::new();
        let result = exec_opts_impl(&args, &mut out).expect("exec_opts result");
        let dict = result.as_dict().expect("dict");
        assert_eq!(dict.get("stdout").unwrap().display(), "|from-child");
        std::env::remove_var("HARN_EXEC_OPTS_PARENT2");
    }

    #[cfg(unix)]
    #[test]
    fn exec_at_opts_honors_directory() {
        let dir = std::env::temp_dir().join(format!("harn-exec-opts-cwd-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let args = vec![
            VmValue::String(std::sync::Arc::from(dir.to_string_lossy().into_owned())),
            exec_opts_list(&["/bin/sh", "-c", "pwd -P"]),
        ];
        let mut out = String::new();
        let result = exec_at_opts_impl(&args, &mut out).expect("exec_at_opts result");
        let dict = result.as_dict().expect("dict");
        // macOS /tmp is a symlink to /private/tmp; canonicalize for comparison.
        let want = std::fs::canonicalize(&dir).unwrap();
        let got = dict.get("stdout").unwrap().display();
        assert_eq!(got.trim(), want.to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn exec_opts_enforces_timeout() {
        let args = vec![
            exec_opts_list(&["/bin/sh", "-c", "sleep 5"]),
            exec_opts_dict(&[("timeout", VmValue::Int(50))]),
        ];
        let mut out = String::new();
        let result = exec_opts_impl(&args, &mut out).expect("exec_opts result");
        let dict = result.as_dict().expect("dict");
        assert!(
            matches!(dict.get("timed_out"), Some(VmValue::Bool(true))),
            "command exceeding timeout must report timed_out"
        );
        assert!(matches!(dict.get("success"), Some(VmValue::Bool(false))));
    }

    #[cfg(unix)]
    #[test]
    fn exec_opts_rejects_empty_command() {
        let args = vec![exec_opts_list(&[])];
        let mut out = String::new();
        assert!(exec_opts_impl(&args, &mut out).is_err());
        let bad = vec![VmValue::String(std::sync::Arc::from("not-a-list"))];
        assert!(exec_opts_impl(&bad, &mut out).is_err());
    }
}
