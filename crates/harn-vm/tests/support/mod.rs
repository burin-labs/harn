#![allow(dead_code)]

pub mod sandbox_host {
    use harn_vm as vm;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/sandbox_host.rs"
    ));
}

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use harn_vm::orchestration::RunExecutionRecord;
use harn_vm::security::session_environment::SessionEnvironment;
use harn_vm::value::VmError;

/// Path to the process helper binary.
///
/// Prefer the runtime `CARGO_BIN_EXE_*` / `NEXTEST_BIN_EXE_*` values that
/// nextest rewrites when executing from an archive; fall back to the
/// compile-time `env!` path for plain `cargo test`.
pub fn process_helper() -> String {
    std::env::var("CARGO_BIN_EXE_harn-test-echo-env")
        .or_else(|_| std::env::var("NEXTEST_BIN_EXE_harn-test-echo-env"))
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_harn-test-echo-env").to_string())
}

// Environment variables are process-global, while tests in one integration
// binary run concurrently. Hold the lock for the guard's lifetime so a child
// never observes another test's temporary environment.
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub struct EnvironmentGuard {
    name: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvironmentGuard {
    pub fn set(name: &'static str, value: &str) -> Self {
        let lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self {
            name,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

pub fn harn_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

pub fn helper_command(args: &[&str]) -> String {
    let mut values = Vec::with_capacity(args.len() + 1);
    values.push(harn_quote(&process_helper()));
    values.extend(args.iter().map(|arg| harn_quote(arg)));
    format!("[{}]", values.join(", "))
}

pub fn run(source: &str) -> Result<String, String> {
    run_with_environment(source, None)
}

pub fn run_isolated(source: &str) -> Result<String, String> {
    run_with_environment(source, Some(SessionEnvironment::isolated()))
}

pub fn logged(source: &str) -> Result<Vec<String>, String> {
    run(source).map(log_lines)
}

pub fn logged_with_execution_context(
    source: &str,
    context: RunExecutionRecord,
) -> Result<Vec<String>, String> {
    run_with_execution_context(source, context).map(log_lines)
}

pub fn logged_isolated(source: &str) -> Result<Vec<String>, String> {
    run_isolated(source).map(log_lines)
}

pub fn log_lines(output: String) -> Vec<String> {
    output_lines(&output)
}

pub fn output_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect()
}

fn run_with_environment(
    source: &str,
    environment: Option<SessionEnvironment>,
) -> Result<String, String> {
    run_with_environment_and_context(source, environment, None, false)
}

fn run_with_execution_context(source: &str, context: RunExecutionRecord) -> Result<String, String> {
    run_with_environment_and_context(source, None, Some(context), false)
}

/// Execute an agent fixture with the same host bridge and VM stack as the native runtime.
pub fn run_with_host_bridge(source: &str) -> Result<String, String> {
    run_with_environment_and_context(source, None, None, true)
}

fn run_with_environment_and_context(
    source: &str,
    environment: Option<SessionEnvironment>,
    context: Option<RunExecutionRecord>,
    with_bridge: bool,
) -> Result<String, String> {
    harn_vm::on_vm_stack(|| run_vm(source, environment, context, with_bridge))
}

fn run_vm(
    source: &str,
    environment: Option<SessionEnvironment>,
    context: Option<RunExecutionRecord>,
    with_bridge: bool,
) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    rt.block_on(harn_clock::test_support::with_local_tasks(async move {
        if with_bridge {
            harn_vm::llm::install_current_host_bridge(std::sync::Arc::new(
                harn_vm::bridge::HostBridge::from_parts(
                    std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(())),
                    1,
                ),
            ));
        }
        harn_vm::stdlib::process::set_session_environment(environment);
        harn_vm::stdlib::process::set_thread_execution_context(context);
        let mut vm = harn_vm::new_vm();
        harn_vm::register_vm_stdlib(&mut vm);
        let result = vm
            .execute(&chunk)
            .await
            .map_err(|error: VmError| format!("{error:?}"));
        harn_vm::stdlib::process::set_session_environment(None);
        harn_vm::stdlib::process::set_thread_execution_context(None);
        if with_bridge {
            harn_vm::llm::clear_current_host_bridge();
        }
        result.map(|_| vm.output().to_string())
    }))
}
