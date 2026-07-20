#![allow(dead_code)]

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use harn_vm::orchestration::RunExecutionRecord;
use harn_vm::security::session_grants::SessionProfile;
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
    run_with_profile(source, None)
}

pub fn run_hermetic(source: &str) -> Result<String, String> {
    run_with_profile(source, Some(SessionProfile::hermetic()))
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

pub fn logged_hermetic(source: &str) -> Result<Vec<String>, String> {
    run_hermetic(source).map(log_lines)
}

pub fn log_lines(output: String) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect()
}

fn run_with_profile(source: &str, profile: Option<SessionProfile>) -> Result<String, String> {
    run_with_profile_and_context(source, profile, None)
}

fn run_with_execution_context(source: &str, context: RunExecutionRecord) -> Result<String, String> {
    run_with_profile_and_context(source, None, Some(context))
}

fn run_with_profile_and_context(
    source: &str,
    profile: Option<SessionProfile>,
    context: Option<RunExecutionRecord>,
) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    rt.block_on(async move {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                harn_vm::stdlib::process::set_session_profile(profile);
                harn_vm::stdlib::process::set_thread_execution_context(context);
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|error: VmError| format!("{error:?}"));
                harn_vm::stdlib::process::set_session_profile(None);
                harn_vm::stdlib::process::set_thread_execution_context(None);
                result.map(|_| vm.output().to_string())
            })
            .await
    })
}
