use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use harn_parser::DiagnosticSeverity;
use harn_vm::event_log::EventLog;

use crate::commands::mcp::{self, AuthResolution};
use crate::commands::time::RunTiming;
use crate::package;
use crate::parse_source_file;
use crate::skill_loader::{
    canonicalize_cli_dirs, emit_loader_warnings, install_skills_global, load_skills,
    SkillLoaderInputs,
};

mod explain_cost;
pub mod harnpack;
pub mod json_events;

use self::harnpack::{HarnpackError, HarnpackRunOptions, PreparedHarnpack};
use self::json_events::NdjsonEmitter;

/// JSON event-stream configuration for `--json` runs.
#[derive(Clone, Default)]
pub struct RunJsonOptions {
    /// Suppress `stdout` / `stderr` events. Transcript, tool, hook,
    /// persona, and the terminal result/error events still flow.
    pub quiet: bool,
}

pub(crate) enum RunFileMcpServeMode {
    Stdio,
    Http {
        options: harn_serve::McpHttpServeOptions,
        auth_policy: harn_serve::AuthPolicy,
    },
}

/// Core builtins that are never denied, even when using `--allow`.
const CORE_BUILTINS: &[&str] = &[
    "println",
    "print",
    "log",
    "type_of",
    "to_string",
    "to_int",
    "to_float",
    "len",
    "assert",
    "assert_eq",
    "assert_ne",
    "json_parse",
    "json_stringify",
    "runtime_context",
    "task_current",
    "runtime_context_values",
    "runtime_context_get",
    "runtime_context_set",
    "runtime_context_clear",
];

/// Build the set of denied builtin names from `--deny` or `--allow` flags.
///
/// - `--deny a,b,c` denies exactly those names.
/// - `--allow a,b,c` denies everything *except* the listed names and the core builtins.
pub(crate) fn build_denied_builtins(
    deny_csv: Option<&str>,
    allow_csv: Option<&str>,
) -> HashSet<String> {
    if let Some(csv) = deny_csv {
        csv.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(csv) = allow_csv {
        // With --allow, we mark every registered stdlib builtin as denied
        // *except* those in the allow list and the core builtins.
        let allowed: HashSet<String> = csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let core: HashSet<&str> = CORE_BUILTINS.iter().copied().collect();

        // Create a temporary VM with stdlib registered to enumerate all builtin names.
        let mut tmp = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut tmp);
        harn_vm::register_store_builtins(&mut tmp, std::path::Path::new("."));
        harn_vm::register_metadata_builtins(&mut tmp, std::path::Path::new("."));

        tmp.builtin_names()
            .into_iter()
            .filter(|name| !allowed.contains(name) && !core.contains(name.as_str()))
            .collect()
    } else {
        HashSet::new()
    }
}

/// Result of [`compile_or_load_chunk_for_run`]. Failures propagate as
/// diagnostic text on the run path so callers map them straight to a
/// non-zero exit code without bespoke error types.
pub(crate) struct LoadedChunk {
    pub(crate) source: String,
    pub(crate) chunk: harn_vm::Chunk,
}

/// Load the entry pipeline as a runnable [`harn_vm::Chunk`], using the
/// content-addressed bytecode cache when its key matches. On a cache miss
/// we read, parse, type-check, and compile, then persist the chunk.
/// On a hit we skip parse/typecheck/compile entirely — the cache invariant
/// is that a stored chunk passed those phases on the writer's harn build,
/// and the key includes every transitively-imported user file so any
/// change re-runs the full path.
///
/// `stderr` receives any diagnostic output. Returns `None` when a fatal
/// type or compile error blocks execution; the caller maps that to
/// exit-code 1.
pub(crate) fn compile_or_load_chunk_for_run(
    path: &str,
    stderr: &mut String,
) -> Option<LoadedChunk> {
    compile_or_load_chunk_with_timing(path, stderr, None)
}

/// Like [`compile_or_load_chunk_for_run`] but lets the caller observe
/// per-phase wall-clock timings (parse, typecheck, bytecode compile +
/// cache hit/miss). Used by `harn time run` to drive the same code
/// path as `harn run` while reporting phase-level timing.
//
// The `as_deref_mut` calls reborrow the inner `&mut RunTiming` so each
// phase can mutate it independently. Clippy's `needless_option_as_deref`
// is correct that the surface types match — that's exactly the
// reborrow we want.
#[allow(clippy::needless_option_as_deref)]
pub(crate) fn compile_or_load_chunk_with_timing(
    path: &str,
    stderr: &mut String,
    mut timing: Option<&mut RunTiming>,
) -> Option<LoadedChunk> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            stderr.push_str(&format!("Error reading {path}: {e}\n"));
            return None;
        }
    };
    if let Some(t) = timing.as_deref_mut() {
        t.input_bytes = source.len() as u64;
    }

    let compile_phase_start = Instant::now();
    let lookup = harn_vm::bytecode_cache::load(Path::new(path), &source);
    if let Some(chunk) = lookup.chunk {
        if let Some(t) = timing.as_deref_mut() {
            t.cache_hit = true;
            t.bytecode_compile = compile_phase_start.elapsed();
        }
        return Some(LoadedChunk { source, chunk });
    }
    if let Some(t) = timing.as_deref_mut() {
        t.cache_hit = false;
    }

    let parse_start = Instant::now();
    let (parsed_source, program) = parse_source_file(path);
    debug_assert_eq!(parsed_source, source, "parse_source_file re-read drifted");
    if let Some(t) = timing.as_deref_mut() {
        t.parse = parse_start.elapsed();
    }

    let typecheck_start = Instant::now();
    let mut had_type_error = false;
    let type_diagnostics = typecheck_with_imports(&program, Path::new(path), &source);
    for diag in &type_diagnostics {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_type_error = true;
        }
        stderr.push_str(&rendered);
    }
    if let Some(t) = timing.as_deref_mut() {
        t.typecheck = typecheck_start.elapsed();
    }
    if had_type_error {
        return None;
    }

    let compile_step_start = Instant::now();
    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            stderr.push_str(&format!("error: compile error: {e}\n"));
            return None;
        }
    };

    // Cache misses are best-effort — read-only homedirs, full disks, and
    // sandboxes are common in CI environments. Surface the failure as a
    // single-line warning when explicitly requested via the audit hook;
    // otherwise stay quiet to avoid bloating happy-path output.
    if let Err(err) = harn_vm::bytecode_cache::store(&lookup.key, &chunk) {
        if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
            eprintln!("[harn] bytecode cache write skipped: {err}");
        }
    }
    if let Some(t) = timing.as_deref_mut() {
        t.bytecode_compile = compile_step_start.elapsed();
    }

    Some(LoadedChunk { source, chunk })
}

/// Run the static type checker against `program` with cross-module
/// import-aware call resolution when the file's imports all resolve. Used
/// by `run_file` and the MCP server entry so `harn run` catches undefined
/// cross-module calls before the VM starts.
fn typecheck_with_imports(
    program: &[harn_parser::SNode],
    path: &Path,
    source: &str,
) -> Vec<harn_parser::TypeDiagnostic> {
    if let Err(error) = package::ensure_dependencies_materialized(path) {
        eprintln!("error: {error}");
        process::exit(1);
    }
    let graph = harn_modules::build(&[path.to_path_buf()]);
    let mut checker = harn_parser::TypeChecker::new();
    if let Some(imported) = graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker.check_with_source(program, source)
}

/// Build the wrapped source and temp file backing a `harn run -e` invocation.
///
/// `import` is a top-level declaration in Harn, so the leading prefix of
/// import lines (with surrounding blanks/comments) is hoisted out of the
/// `pipeline main(task) { ... }` wrapper. The temp file is created in the
/// current working directory so relative imports (`import "./lib"`) and
/// `harn.toml` discovery resolve against the user's project, not the
/// system temp dir. If the CWD is unwritable we fall back to the system
/// temp dir with a stderr warning — pure-expression `-e` still works,
/// but relative imports will fail to resolve.
pub(crate) fn prepare_eval_temp_file(
    code: &str,
) -> Result<(String, tempfile::NamedTempFile), String> {
    let (header, body) = split_eval_header(code);
    let wrapped = if header.is_empty() {
        format!("pipeline main(task) {{\n{body}\n}}")
    } else {
        format!("{header}\npipeline main(task) {{\n{body}\n}}")
    };

    let tmp = create_eval_temp_file()?;
    Ok((wrapped, tmp))
}

/// Try to place the `-e` temp file in the current working directory so
/// relative imports and `harn.toml` discovery resolve against the user's
/// project. Fall back to the system temp dir on failure (with a warning),
/// so pure-expression `-e` keeps working in read-only contexts.
fn create_eval_temp_file() -> Result<tempfile::NamedTempFile, String> {
    if let Some(dir) = std::env::current_dir().ok().as_deref() {
        // Hidden prefix on Unix so editors / tree-walkers are less likely
        // to pick the file up during its short lifetime.
        match tempfile::Builder::new()
            .prefix(".harn-eval-")
            .suffix(".harn")
            .tempfile_in(dir)
        {
            Ok(tmp) => return Ok(tmp),
            Err(error) => eprintln!(
                "warning: harn run -e: could not create temp file in {}: {error}; \
                 relative imports will not resolve",
                dir.display()
            ),
        }
    }
    tempfile::Builder::new()
        .prefix("harn-eval-")
        .suffix(".harn")
        .tempfile()
        .map_err(|e| format!("failed to create temp file for -e: {e}"))
}

/// Split the `-e` input into a header (top-level imports + leading
/// blanks/comments) and a body (everything else, to be wrapped in
/// `pipeline main(task)`). The header may be empty.
///
/// Lines whose first non-whitespace token is `import` or `pub import`
/// are treated as imports. Scanning stops at the first non-blank,
/// non-comment, non-import line.
fn split_eval_header(code: &str) -> (String, String) {
    let mut header_end = 0usize;
    let mut last_kept = 0usize;
    for (idx, line) in code.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            header_end = idx + 1;
            continue;
        }
        let is_import = trimmed.starts_with("import ")
            || trimmed.starts_with("import\t")
            || trimmed.starts_with("import\"")
            || trimmed.starts_with("pub import ")
            || trimmed.starts_with("pub import\t");
        if is_import {
            header_end = idx + 1;
            last_kept = idx + 1;
        } else {
            break;
        }
    }
    if last_kept == 0 {
        return (String::new(), code.to_string());
    }
    let mut header_lines: Vec<&str> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    for (idx, line) in code.lines().enumerate() {
        if idx < header_end {
            header_lines.push(line);
        } else {
            body_lines.push(line);
        }
    }
    (header_lines.join("\n"), body_lines.join("\n"))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CliLlmMockMode {
    #[default]
    Off,
    Replay {
        fixture_path: PathBuf,
    },
    Record {
        fixture_path: PathBuf,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunAttestationOptions {
    pub receipt_out: Option<PathBuf>,
    pub agent_id: Option<String>,
}

/// Opt-in profiling. When `text` is true the run prints a categorical
/// breakdown to stderr after execution; when `json_path` is set the same
/// rollup is serialized to that path. Either flag enables span tracing
/// (i.e. `harn_vm::tracing::set_tracing_enabled(true)`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunProfileOptions {
    pub text: bool,
    pub json_path: Option<PathBuf>,
}

impl RunProfileOptions {
    pub fn is_enabled(&self) -> bool {
        self.text || self.json_path.is_some()
    }
}

#[derive(Clone)]
pub struct RunInterruptTokens {
    pub cancel_token: Arc<AtomicBool>,
    pub signal_token: Arc<Mutex<Option<String>>>,
}

struct ExecuteRunInputs<'a> {
    path: &'a str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    interrupt_tokens: Option<RunInterruptTokens>,
    json: Option<(RunJsonOptions, Box<dyn io::Write + Send>)>,
    timing: Option<&'a mut RunTiming>,
    harnpack: HarnpackRunOptions,
}

/// Captured outcome of an in-process `execute_run` invocation. Tests use this
/// instead of spawning the `harn` binary; the binary entry point translates
/// it into real stdout/stderr writes + `process::exit`.
#[derive(Clone, Debug, Default)]
pub struct RunOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn install_cli_llm_mock_mode(mode: &CliLlmMockMode) -> Result<(), String> {
    harn_vm::llm::clear_cli_llm_mock_mode();
    match mode {
        CliLlmMockMode::Off => Ok(()),
        CliLlmMockMode::Replay { fixture_path } => {
            let mocks = harn_vm::llm::load_llm_mocks_jsonl(fixture_path)?;
            harn_vm::llm::install_cli_llm_mocks(mocks);
            Ok(())
        }
        CliLlmMockMode::Record { .. } => {
            harn_vm::llm::enable_cli_llm_mock_recording();
            Ok(())
        }
    }
}

pub fn persist_cli_llm_mock_recording(mode: &CliLlmMockMode) -> Result<(), String> {
    let CliLlmMockMode::Record { fixture_path } = mode else {
        return Ok(());
    };
    if let Some(parent) = fixture_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create fixture directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }

    let lines = harn_vm::llm::take_cli_llm_recordings()
        .into_iter()
        .map(harn_vm::llm::serialize_llm_mock)
        .collect::<Result<Vec<_>, _>>()?;
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(fixture_path, body)
        .map_err(|error| format!("failed to write {}: {error}", fixture_path.display()))
}

pub(crate) async fn run_file(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
) {
    run_file_with_skill_dirs(
        path,
        trace,
        denied_builtins,
        script_argv,
        Vec::new(),
        llm_mock_mode,
        attestation,
        profile,
        None,
        HarnpackRunOptions::default(),
    )
    .await;
}

pub(crate) fn run_explain_cost_file_with_skill_dirs(path: &str) {
    let outcome = execute_explain_cost(path);
    if !outcome.stderr.is_empty() {
        io::stderr().write_all(outcome.stderr.as_bytes()).ok();
    }
    if !outcome.stdout.is_empty() {
        io::stdout().write_all(outcome.stdout.as_bytes()).ok();
    }
    if outcome.exit_code != 0 {
        process::exit(outcome.exit_code);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_file_with_skill_dirs(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    json: Option<RunJsonOptions>,
    harnpack: HarnpackRunOptions,
) {
    // Graceful shutdown: flush run records before exit on SIGINT/SIGTERM.
    let interrupt_tokens = install_signal_shutdown_handler();

    let _stdout_passthrough = StdoutPassthroughGuard::enable();
    let json_with_stdout =
        json.map(|opts| (opts, Box::new(io::stdout()) as Box<dyn io::Write + Send>));
    let outcome = execute_run_inner(ExecuteRunInputs {
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        interrupt_tokens: Some(interrupt_tokens.clone()),
        json: json_with_stdout,
        timing: None,
        harnpack,
    })
    .await;

    // `harn run` streams normal program stdout during execution. Any stdout
    // left here came from older capture paths, so flush it after diagnostics.
    if !outcome.stderr.is_empty() {
        io::stderr().write_all(outcome.stderr.as_bytes()).ok();
    }
    if !outcome.stdout.is_empty() {
        io::stdout().write_all(outcome.stdout.as_bytes()).ok();
    }

    let mut exit_code = outcome.exit_code;
    if exit_code != 0 && interrupt_tokens.cancel_token.load(Ordering::SeqCst) {
        exit_code = 124;
    }
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_resume_with_skill_dirs(
    target: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    resume_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    json: Option<RunJsonOptions>,
) {
    let source = r#"import { resume_agent, wait_agent } from "std/agent/workers"

pipeline main(task) {
  let input = if len(argv) > 1 {
    argv[1]
  } else {
    nil
  }
  let handle = resume_agent(argv[0], input, true)
  return wait_agent(handle)
}
"#;
    let tmp = create_eval_temp_file().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });
    let tmp_path = tmp.path().to_path_buf();
    if let Err(error) = fs::write(&tmp_path, source) {
        eprintln!("error: failed to write temp file for --resume: {error}");
        process::exit(1);
    }
    let mut argv = Vec::with_capacity(resume_argv.len() + 1);
    argv.push(target.to_string());
    argv.extend(resume_argv);
    let tmp_str = tmp_path.to_string_lossy().into_owned();
    run_file_with_skill_dirs(
        &tmp_str,
        trace,
        denied_builtins,
        argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        json,
        HarnpackRunOptions::default(),
    )
    .await;
}

pub fn execute_explain_cost(path: &str) -> RunOutcome {
    let stdout = String::new();
    let mut stderr = String::new();

    let (source, program) = parse_source_file(path);

    let mut had_type_error = false;
    let type_diagnostics = typecheck_with_imports(&program, Path::new(path), &source);
    for diag in &type_diagnostics {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_type_error = true;
        }
        stderr.push_str(&rendered);
    }
    if had_type_error {
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    let extensions = package::load_runtime_extensions(Path::new(path));
    package::install_runtime_extensions(&extensions);
    RunOutcome {
        stdout: explain_cost::render_explain_cost(path, &program),
        stderr,
        exit_code: 0,
    }
}

pub(crate) struct StdoutPassthroughGuard {
    previous: bool,
}

impl StdoutPassthroughGuard {
    pub(crate) fn enable() -> Self {
        Self {
            previous: harn_vm::set_stdout_passthrough(true),
        }
    }
}

impl Drop for StdoutPassthroughGuard {
    fn drop(&mut self) {
        harn_vm::set_stdout_passthrough(self.previous);
    }
}

// User-facing copy on Ctrl-C. We want the operator to know that a brief
// pause after the first signal is expected (the VM rewinds the active
// instruction, drops in-flight async ops like a hanging Ollama request,
// and unwinds frames before the runtime exits) so they don't reflexively
// reach for a second Ctrl-C and force-kill the process. The "Ctrl-C
// again to force-exit" hint is load-bearing — earlier runs of harn
// released to the fleet showed operators routinely double-tapping the
// shortcut and losing the chance to inspect the error trace.
const FIRST_SIGNAL_MESSAGE: &str =
    "[harn] signal received, interrupting VM (give it a moment to unwind in-flight async ops; Ctrl-C again to force-exit)...";

fn install_signal_shutdown_handler() -> RunInterruptTokens {
    let tokens = RunInterruptTokens {
        cancel_token: Arc::new(AtomicBool::new(false)),
        signal_token: Arc::new(Mutex::new(None)),
    };
    let tokens_clone = tokens.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
            let mut sighup = signal(SignalKind::hangup()).expect("SIGHUP handler");
            let mut seen_signal = false;
            loop {
                let signal_name = tokio::select! {
                    _ = sigterm.recv() => "SIGTERM",
                    _ = sigint.recv() => "SIGINT",
                    _ = sighup.recv() => "SIGHUP",
                };
                if seen_signal {
                    eprintln!("[harn] second signal received, terminating");
                    process::exit(124);
                }
                seen_signal = true;
                request_vm_interrupt(&tokens_clone, signal_name);
                eprintln!("{FIRST_SIGNAL_MESSAGE}");
            }
        }
        #[cfg(not(unix))]
        {
            let mut seen_signal = false;
            loop {
                let _ = tokio::signal::ctrl_c().await;
                if seen_signal {
                    eprintln!("[harn] second signal received, terminating");
                    process::exit(124);
                }
                seen_signal = true;
                request_vm_interrupt(&tokens_clone, "SIGINT");
                eprintln!("{FIRST_SIGNAL_MESSAGE}");
            }
        }
    });
    tokens
}

fn request_vm_interrupt(tokens: &RunInterruptTokens, signal_name: &str) {
    if let Ok(mut signal) = tokens.signal_token.lock() {
        *signal = Some(signal_name.to_string());
    }
    tokens.cancel_token.store(true, Ordering::SeqCst);
}

/// In-process equivalent of `run_file_with_skill_dirs`. Returns the captured
/// stdout, stderr, and what exit code the binary entry would have used,
/// instead of writing to real stdout/stderr or calling `process::exit`.
///
/// Tests should call this directly. The `harn run` binary path wraps it.
pub async fn execute_run(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
) -> RunOutcome {
    execute_run_with_harnpack_options(
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        HarnpackRunOptions::default(),
    )
    .await
}

/// [`execute_run`] for callers that want to opt-in to the `.harnpack`
/// verify-replay-execute path. Used by `harn run <bundle.harnpack>`
/// integration tests and by the binary entry once it has parsed the
/// `--allow-unsigned` / `--dry-run-verify` flags.
#[allow(clippy::too_many_arguments)]
pub async fn execute_run_with_harnpack_options(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    harnpack: HarnpackRunOptions,
) -> RunOutcome {
    execute_run_inner(ExecuteRunInputs {
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        interrupt_tokens: None,
        json: None,
        timing: None,
        harnpack,
    })
    .await
}

/// `execute_run` variant for `--json` mode. Returns once the run is
/// complete; the NDJSON event stream — including the terminal `result`
/// or `error` event — has already been written to `out` and flushed.
/// `out` must be `Send` because the run-event sink may be called from
/// any worker thread the VM spawns.
#[allow(clippy::too_many_arguments)]
pub async fn execute_run_json(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    out: Box<dyn io::Write + Send>,
    options: RunJsonOptions,
) -> RunOutcome {
    execute_run_inner(ExecuteRunInputs {
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        interrupt_tokens: None,
        json: Some((options, out)),
        timing: None,
        harnpack: HarnpackRunOptions::default(),
    })
    .await
}

/// Run a `.harn` file with the default builtin/argv set and record
/// phase timings into `timing`. Used by `harn time run` so the
/// instrumented run shares the exact code path as plain `harn run`.
pub(crate) async fn execute_run_with_timing(
    path: &str,
    script_argv: Vec<String>,
    timing: Option<&mut RunTiming>,
) -> RunOutcome {
    execute_run_inner(ExecuteRunInputs {
        path,
        trace: false,
        denied_builtins: HashSet::new(),
        script_argv,
        skill_dirs_raw: Vec::new(),
        llm_mock_mode: CliLlmMockMode::Off,
        attestation: None,
        profile: RunProfileOptions::default(),
        interrupt_tokens: None,
        json: None,
        timing,
        harnpack: HarnpackRunOptions::default(),
    })
    .await
}

// See [`compile_or_load_chunk_with_timing`] for why `as_deref_mut` is
// the intentional reborrow pattern here.
#[allow(clippy::needless_option_as_deref)]
async fn execute_run_inner(inputs: ExecuteRunInputs<'_>) -> RunOutcome {
    let ExecuteRunInputs {
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        interrupt_tokens,
        json,
        mut timing,
        harnpack,
    } = inputs;

    // `--json` installs an in-process sink that diverts every
    // observable VM event (stdout, stderr, transcript, tool, hook,
    // persona) into a single NDJSON stream on `out`. The sink stays
    // active until we drop the guard below — fatal errors emit a
    // terminal `error` event on the same stream before bailing.
    let json_session = json.map(|(options, out)| JsonRunSession::install(options, out));

    let mut stderr = String::new();
    let mut stdout = String::new();

    // `.harnpack` preflight: verify signature + replay archive into the
    // content-addressed cache before we touch the chunk loader. The
    // outcome path (entrypoint inside the unpacked tree) replaces the
    // CLI-supplied `path` for everything below.
    let owned_run_path: String;
    let resolved_path: &str = if harnpack::looks_like_harnpack(Path::new(path)) {
        let outcome = match harnpack::prepare_harnpack(Path::new(path), &harnpack, &mut stderr) {
            Ok(prepared) => prepared,
            Err(err) => return finalize_harnpack_error(stderr, json_session, err),
        };
        harn_vm::run_events::emit(harn_vm::run_events::RunEvent::PackRun {
            bundle_hash: outcome.bundle_hash.clone(),
            signature_verified: outcome.signature_verified,
            key_id: outcome.key_id.clone(),
            cache_hit: outcome.cache_hit,
            dry_run_verify: harnpack.dry_run_verify,
        });
        if harnpack.dry_run_verify {
            return finalize_harnpack_dry_run(stderr, json_session, &outcome);
        }
        owned_run_path = outcome.entrypoint_path.to_string_lossy().into_owned();
        owned_run_path.as_str()
    } else {
        path
    };

    let Some(LoadedChunk { source, chunk }) =
        compile_or_load_chunk_with_timing(resolved_path, &mut stderr, timing.as_deref_mut())
    else {
        if let Some(session) = json_session {
            return session.finalize_error("compile_error", stderr, 1);
        }
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    };
    let path = resolved_path;

    // Bracket the VM-setup phase explicitly. `run_setup` covers
    // everything between the bytecode compile and the first VM
    // instruction; `run_main` covers `vm.execute` proper.
    let setup_start = Instant::now();

    if trace {
        harn_vm::llm::enable_tracing();
    }
    if profile.is_enabled() {
        harn_vm::tracing::set_tracing_enabled(true);
    }
    if let Err(error) = install_cli_llm_mock_mode(&llm_mock_mode) {
        stderr.push_str(&format!("error: {error}\n"));
        if let Some(session) = json_session {
            return session.finalize_error("llm_mock_install", error, 1);
        }
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    let mut vm = harn_vm::Vm::new();
    if let Some(interrupt_tokens) = interrupt_tokens {
        vm.install_interrupt_signal_token(interrupt_tokens.signal_token);
        vm.install_cancel_token(interrupt_tokens.cancel_token);
    }
    harn_vm::register_vm_stdlib(&mut vm);
    crate::install_default_hostlib(&mut vm);
    let source_parent = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    // Metadata/store rooted at harn.toml when present; source dir otherwise.
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    let attestation_started_at_ms = now_ms();
    let attestation_log = if attestation.is_some() {
        Some(harn_vm::event_log::install_memory_for_current_thread(256))
    } else {
        None
    };
    if let Some(log) = attestation_log.as_ref() {
        append_run_provenance_event(
            log,
            "started",
            serde_json::json!({
                "pipeline": path,
                "argv": &script_argv,
                "project_root": store_base.display().to_string(),
            }),
        )
        .await;
    }
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if !denied_builtins.is_empty() {
        vm.set_denied_builtins(denied_builtins);
    }
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            vm.set_source_dir(p);
        }
    }

    // Load filesystem + manifest skills before the pipeline runs so
    // `skills` is populated with a pre-discovered registry (see #73).
    let cli_dirs = canonicalize_cli_dirs(&skill_dirs_raw, None);
    let loaded = load_skills(&SkillLoaderInputs {
        cli_dirs,
        source_path: Some(std::path::PathBuf::from(path)),
    });
    emit_loader_warnings(&loaded.loader_warnings);
    install_skills_global(&mut vm, &loaded);

    // `harn run script.harn -- a b c` yields `argv == ["a", "b", "c"]`.
    // Always set so scripts can rely on `len(argv)`.
    let argv_values: Vec<harn_vm::VmValue> = script_argv
        .iter()
        .map(|s| harn_vm::VmValue::String(std::rc::Rc::from(s.as_str())))
        .collect();
    vm.set_global(
        "argv",
        harn_vm::VmValue::List(std::rc::Rc::new(argv_values)),
    );

    // Install the script's `Harness` capability handle so the auto-call
    // emitted by `Compiler::compile()` for `fn main(harness: Harness)`
    // entrypoints can read it.
    vm.set_harness(harn_vm::Harness::real());

    let extensions = package::load_runtime_extensions(Path::new(path));
    package::install_runtime_extensions(&extensions);
    if let Some(manifest) = extensions.root_manifest.as_ref() {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }
    if let Err(error) = package::install_manifest_triggers(&mut vm, &extensions).await {
        stderr.push_str(&format!(
            "error: failed to install manifest triggers: {error}\n"
        ));
        if let Some(session) = json_session {
            return session.finalize_error("manifest_triggers", error.to_string(), 1);
        }
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }
    if let Err(error) = package::install_manifest_hooks(&mut vm, &extensions).await {
        stderr.push_str(&format!(
            "error: failed to install manifest hooks: {error}\n"
        ));
        if let Some(session) = json_session {
            return session.finalize_error("manifest_hooks", error.to_string(), 1);
        }
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    // Run inside a LocalSet so spawn_local works for concurrency builtins.
    let local = tokio::task::LocalSet::new();
    if let Some(t) = timing.as_deref_mut() {
        t.run_setup = setup_start.elapsed();
    }
    let main_start = Instant::now();
    let execution = local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(value) => Ok((vm.output(), value)),
                Err(e) => Err(vm.format_runtime_error(&e)),
            }
        })
        .await;
    if let Some(t) = timing.as_deref_mut() {
        t.run_main = main_start.elapsed();
    }
    if let Err(error) = persist_cli_llm_mock_recording(&llm_mock_mode) {
        stderr.push_str(&format!("error: {error}\n"));
        if let Some(session) = json_session {
            return session.finalize_error("llm_mock_record", error, 1);
        }
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    // Always drain any captured stderr accumulated during execution.
    let buffered_stderr = harn_vm::take_stderr_buffer();
    stderr.push_str(&buffered_stderr);

    let exit_code = match &execution {
        Ok((_, return_value)) => exit_code_from_return_value(return_value),
        Err(_) => 1,
    };

    if let (Some(options), Some(log)) = (attestation.as_ref(), attestation_log.as_ref()) {
        if let Err(error) = emit_run_attestation(
            log,
            path,
            store_base,
            attestation_started_at_ms,
            exit_code,
            options,
            &mut stderr,
        )
        .await
        {
            stderr.push_str(&format!(
                "error: failed to emit provenance receipt: {error}\n"
            ));
            if let Some(session) = json_session {
                return session.finalize_error("attestation", error.to_string(), 1);
            }
            return RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            };
        }
        harn_vm::event_log::reset_active_event_log();
    }

    match execution {
        Ok((output, return_value)) => {
            stdout.push_str(output);
            if trace {
                stderr.push_str(&render_trace_summary());
            }
            if profile.is_enabled() {
                if let Err(error) = render_and_persist_profile(&profile, &mut stderr) {
                    stderr.push_str(&format!("warning: failed to write profile: {error}\n"));
                }
            }
            if exit_code != 0 {
                stderr.push_str(&render_return_value_error(&return_value));
            }
            if let Some(session) = json_session {
                let value = harn_vm::llm::vm_value_to_json(&return_value);
                return session.finalize_result(value, exit_code);
            }
            RunOutcome {
                stdout,
                stderr,
                exit_code,
            }
        }
        Err(rendered_error) => {
            stderr.push_str(&rendered_error);
            if profile.is_enabled() {
                if let Err(error) = render_and_persist_profile(&profile, &mut stderr) {
                    stderr.push_str(&format!("warning: failed to write profile: {error}\n"));
                }
            }
            if let Some(session) = json_session {
                return session.finalize_error("runtime", rendered_error, 1);
            }
            RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            }
        }
    }
}

fn render_and_persist_profile(
    options: &RunProfileOptions,
    stderr: &mut String,
) -> Result<(), String> {
    let spans = harn_vm::tracing::peek_spans();
    let profile = harn_vm::profile::build(&spans);
    if options.text {
        stderr.push_str(&harn_vm::profile::render(&profile));
    }
    if let Some(path) = options.json_path.as_ref() {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {error}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(&profile)
            .map_err(|error| format!("serialize profile: {error}"))?;
        fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

async fn append_run_provenance_event(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    kind: &str,
    payload: serde_json::Value,
) {
    let Ok(topic) = harn_vm::event_log::Topic::new("run.provenance") else {
        return;
    };
    let _ = log
        .append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await;
}

async fn emit_run_attestation(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    path: &str,
    store_base: &Path,
    started_at_ms: i64,
    exit_code: i32,
    options: &RunAttestationOptions,
    stderr: &mut String,
) -> Result<(), String> {
    let finished_at_ms = now_ms();
    let status = if exit_code == 0 { "success" } else { "failure" };
    append_run_provenance_event(
        log,
        "finished",
        serde_json::json!({
            "pipeline": path,
            "status": status,
            "exit_code": exit_code,
        }),
    )
    .await;
    log.flush()
        .await
        .map_err(|error| format!("failed to flush attestation event log: {error}"))?;
    let secret_provider = harn_vm::secrets::configured_default_chain("harn.provenance")
        .map_err(|error| format!("failed to configure provenance secrets: {error}"))?;
    let (signing_key, key_id) =
        harn_vm::load_or_generate_agent_signing_key(&secret_provider, options.agent_id.as_deref())
            .await
            .map_err(|error| format!("failed to load provenance signing key: {error}"))?;
    let receipt = harn_vm::build_signed_receipt(
        log,
        harn_vm::ReceiptBuildOptions {
            pipeline: path.to_string(),
            status: status.to_string(),
            started_at_ms,
            finished_at_ms,
            exit_code,
            producer_name: "harn-cli".to_string(),
            producer_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        &signing_key,
        key_id,
    )
    .await
    .map_err(|error| format!("failed to build provenance receipt: {error}"))?;
    let receipt_path = receipt_output_path(store_base, options, &receipt.receipt_id);
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("failed to encode provenance receipt: {error}"))?;
    fs::write(&receipt_path, encoded)
        .map_err(|error| format!("failed to write {}: {error}", receipt_path.display()))?;
    stderr.push_str(&format!("provenance receipt: {}\n", receipt_path.display()));
    Ok(())
}

fn receipt_output_path(
    store_base: &Path,
    options: &RunAttestationOptions,
    receipt_id: &str,
) -> PathBuf {
    if let Some(path) = options.receipt_out.as_ref() {
        return path.clone();
    }
    harn_vm::runtime_paths::state_root(store_base)
        .join("receipts")
        .join(format!("{receipt_id}.json"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Map a script's top-level return value to a process exit code.
///
/// - `int n`             → exit n (clamped to 0..=255)
/// - `Result::Ok(_)`     → exit 0
/// - `Result::Err(_)`    → exit 1
/// - anything else       → exit 0
fn exit_code_from_return_value(value: &harn_vm::VmValue) -> i32 {
    use harn_vm::VmValue;
    match value {
        VmValue::Int(n) => (*n).clamp(0, 255) as i32,
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => 1,
        _ => 0,
    }
}

/// State for a single `harn run --json` invocation. Installs the
/// run-event sink in [`Self::install`] and removes it in [`Drop`], so
/// every exit path through `execute_run_inner` cleans up correctly
/// even if a panic unwinds out of the VM. Save-and-restore of any
/// previously installed sink keeps the helper safe to nest (rare, but
/// in-process embeddings can call into `harn run` from a host that
/// already had a sink wired).
///
/// `finalize_result` / `finalize_error` emit the terminal event and
/// build a [`RunOutcome`] whose stdout/stderr captured-buffer fields
/// stay **empty** — the canonical stream is on `out`.
/// `outcome.exit_code` still carries the process exit code so the
/// binary entry can `process::exit(...)`.
struct JsonRunSession {
    emitter: self::json_events::NdjsonEmitter,
    prior_sink: Option<Arc<dyn harn_vm::run_events::RunEventSink>>,
}

impl JsonRunSession {
    fn install(options: RunJsonOptions, out: Box<dyn io::Write + Send>) -> Self {
        let emitter = NdjsonEmitter::new(out, options.quiet);
        let prior_sink = harn_vm::run_events::install_sink(emitter.sink());
        Self {
            emitter,
            prior_sink,
        }
    }

    fn finalize_result(self, value: serde_json::Value, exit_code: i32) -> RunOutcome {
        self.emitter.emit_result(value, exit_code);
        RunOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
        }
    }

    fn finalize_error(
        self,
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
    ) -> RunOutcome {
        self.emitter.emit_error(code, message);
        RunOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
        }
    }
}

impl Drop for JsonRunSession {
    fn drop(&mut self) {
        match self.prior_sink.take() {
            Some(prior) => {
                harn_vm::run_events::install_sink(prior);
            }
            None => harn_vm::run_events::clear_sink(),
        }
    }
}

/// Translate a preflight failure into either the `--json` error event
/// stream or a plain stderr message plus exit-code 1. Keeps the
/// `.harnpack` verify path's error reporting consistent with the rest
/// of `harn run`.
fn finalize_harnpack_error(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    err: HarnpackError,
) -> RunOutcome {
    stderr.push_str(&format!("error: {}\n", err.message));
    if let Some(session) = json_session {
        return session.finalize_error(err.code, err.message, 1);
    }
    RunOutcome {
        stdout: String::new(),
        stderr,
        exit_code: 1,
    }
}

/// Successful `--dry-run-verify` path. Reports the bundle hash and
/// signature outcome on stderr (since stdout belongs to the script) and
/// emits a terminal `result` event when `--json` is active so consumers
/// see the run complete.
fn finalize_harnpack_dry_run(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    prepared: &PreparedHarnpack,
) -> RunOutcome {
    let summary = format!(
        "[harn] harnpack verify ok: bundle_hash={}, signature_verified={}, cache_hit={}\n",
        prepared.bundle_hash, prepared.signature_verified, prepared.cache_hit
    );
    stderr.push_str(&summary);
    if let Some(session) = json_session {
        let value = serde_json::json!({
            "bundle_hash": prepared.bundle_hash,
            "signature_verified": prepared.signature_verified,
            "key_id": prepared.key_id,
            "cache_hit": prepared.cache_hit,
            "dry_run_verify": true,
        });
        return session.finalize_result(value, 0);
    }
    RunOutcome {
        stdout: String::new(),
        stderr,
        exit_code: 0,
    }
}

fn render_return_value_error(value: &harn_vm::VmValue) -> String {
    let harn_vm::VmValue::EnumVariant(enum_variant) = value else {
        return String::new();
    };
    if !enum_variant.is_variant("Result", "Err") {
        return String::new();
    }
    let rendered = enum_variant
        .fields
        .first()
        .map(|p| p.display())
        .unwrap_or_default();
    if rendered.is_empty() {
        "error\n".to_string()
    } else if rendered.ends_with('\n') {
        rendered
    } else {
        format!("{rendered}\n")
    }
}

/// Connect to MCP servers declared in `harn.toml` and register them as
/// `mcp.<name>` globals on the VM. Connection failures are warned but do
/// not abort execution.
///
/// Servers with `lazy = true` are registered with the VM-side MCP
/// registry but NOT booted — their processes start the first time a
/// skill's `requires_mcp` list names them or user code calls
/// `mcp_ensure_active("name")` / `mcp_call(mcp.<name>, ...)`.
pub(crate) async fn connect_mcp_servers(
    servers: &[package::McpServerConfig],
    vm: &mut harn_vm::Vm,
) {
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::time::Duration;

    let mut mcp_dict: BTreeMap<String, harn_vm::VmValue> = BTreeMap::new();
    let mut registrations: Vec<harn_vm::RegisteredMcpServer> = Vec::new();

    for server in servers {
        let resolved_auth = match mcp::resolve_auth_for_server(server).await {
            Ok(resolution) => resolution,
            Err(error) => {
                eprintln!(
                    "warning: mcp: failed to load auth for '{}': {}",
                    server.name, error
                );
                AuthResolution::None
            }
        };
        let spec = serde_json::json!({
            "name": server.name,
            "transport": server.transport.clone().unwrap_or_else(|| "stdio".to_string()),
            "command": server.command,
            "args": server.args,
            "env": server.env,
            "url": server.url,
            "auth_token": match resolved_auth {
                AuthResolution::Bearer(token) => Some(token),
                AuthResolution::None => server.auth_token.clone(),
            },
            "protocol_version": server.protocol_version,
            "proxy_server_name": server.proxy_server_name,
        });

        // Register with the VM-side registry regardless of lazy flag —
        // skill activation and `mcp_ensure_active` look up specs there.
        registrations.push(harn_vm::RegisteredMcpServer {
            name: server.name.clone(),
            spec: spec.clone(),
            lazy: server.lazy,
            card: server.card.clone(),
            keep_alive: server.keep_alive_ms.map(Duration::from_millis),
        });

        if server.lazy {
            eprintln!(
                "[harn] mcp: deferred '{}' (lazy, boots on first use)",
                server.name
            );
            continue;
        }

        match harn_vm::connect_mcp_server_from_json(&spec).await {
            Ok(handle) => {
                eprintln!("[harn] mcp: connected to '{}'", server.name);
                harn_vm::mcp_install_active(&server.name, handle.clone());
                mcp_dict.insert(server.name.clone(), harn_vm::VmValue::mcp_client(handle));
            }
            Err(e) => {
                eprintln!(
                    "warning: mcp: failed to connect to '{}': {}",
                    server.name, e
                );
            }
        }
    }

    // Install registrations AFTER eager connects so `install_active`
    // above doesn't get overwritten.
    harn_vm::mcp_register_servers(registrations);

    if !mcp_dict.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::Dict(Rc::new(mcp_dict)));
    }
}

pub(crate) fn render_trace_summary() -> String {
    use std::fmt::Write;
    let entries = harn_vm::llm::take_trace();
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "\n\x1b[2m─── LLM trace ───\x1b[0m");
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    let mut total_ms = 0u64;
    for (i, entry) in entries.iter().enumerate() {
        let _ = writeln!(
            out,
            "  #{}: {} | {} in + {} out tokens | {} ms",
            i + 1,
            entry.model,
            entry.input_tokens,
            entry.output_tokens,
            entry.duration_ms,
        );
        total_input += entry.input_tokens;
        total_output += entry.output_tokens;
        total_ms += entry.duration_ms;
    }
    let total_tokens = total_input + total_output;
    // Rough cost estimate using Sonnet 4 pricing ($3/MTok in, $15/MTok out).
    let cost = (total_input as f64 * 3.0 + total_output as f64 * 15.0) / 1_000_000.0;
    let _ = writeln!(
        out,
        "  \x1b[1m{} call{}, {} tokens ({}in + {}out), {} ms, ~${:.4}\x1b[0m",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        total_tokens,
        total_input,
        total_output,
        total_ms,
        cost,
    );
    out
}

/// Run a .harn file as an MCP server using the script-driven surface.
/// The pipeline must call `mcp_tools(registry)` (or the alias
/// `mcp_serve(registry)`) so the CLI can expose its tools, and may
/// register additional resources/prompts via `mcp_resource(...)` /
/// `mcp_resource_template(...)` / `mcp_prompt(...)`.
///
/// Dispatched into by `harn serve mcp <file>` when the script does not
/// define any `pub fn` exports — see `commands::serve::run_mcp_server`.
///
/// `card_source` — optional `--card` argument. Accepts either a path to
/// a JSON file or an inline JSON string. When present, the card is
/// embedded in the `initialize` response and exposed as the
/// `well-known://mcp-card` resource.
pub(crate) async fn run_file_mcp_serve(
    path: &str,
    card_source: Option<&str>,
    mode: RunFileMcpServeMode,
) {
    let mut diagnostics = String::new();
    let Some(LoadedChunk { source, chunk }) = compile_or_load_chunk_for_run(path, &mut diagnostics)
    else {
        eprint!("{diagnostics}");
        process::exit(1);
    };
    if !diagnostics.is_empty() {
        eprint!("{diagnostics}");
    }

    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    crate::install_default_hostlib(&mut vm);
    let source_parent = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }
    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            vm.set_source_dir(p);
        }
    }

    // Same skill discovery as `harn run` — see comment there.
    let loaded = load_skills(&SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: Some(std::path::PathBuf::from(path)),
    });
    emit_loader_warnings(&loaded.loader_warnings);
    install_skills_global(&mut vm, &loaded);

    let extensions = package::load_runtime_extensions(Path::new(path));
    package::install_runtime_extensions(&extensions);
    if let Some(manifest) = extensions.root_manifest.as_ref() {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }
    if let Err(error) = package::install_manifest_triggers(&mut vm, &extensions).await {
        eprintln!("error: failed to install manifest triggers: {error}");
        process::exit(1);
    }
    if let Err(error) = package::install_manifest_hooks(&mut vm, &extensions).await {
        eprintln!("error: failed to install manifest hooks: {error}");
        process::exit(1);
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(_) => {}
                Err(e) => {
                    eprint!("{}", vm.format_runtime_error(&e));
                    process::exit(1);
                }
            }

            // Pipeline output goes to stderr — stdout is the MCP transport.
            let output = vm.output();
            if !output.is_empty() {
                eprint!("{output}");
            }

            let registry = match harn_vm::take_mcp_serve_registry() {
                Some(r) => r,
                None => {
                    eprintln!("error: pipeline did not call mcp_serve(registry)");
                    eprintln!("hint: call mcp_serve(tools) at the end of your pipeline");
                    process::exit(1);
                }
            };

            let tools = match harn_vm::tool_registry_to_mcp_tools(&registry) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {e}");
                    process::exit(1);
                }
            };

            let resources = harn_vm::take_mcp_serve_resources();
            let resource_templates = harn_vm::take_mcp_serve_resource_templates();
            let prompts = harn_vm::take_mcp_serve_prompts();

            let server_name = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("harn")
                .to_string();

            let mut caps = Vec::new();
            if !tools.is_empty() {
                caps.push(format!(
                    "{} tool{}",
                    tools.len(),
                    if tools.len() == 1 { "" } else { "s" }
                ));
            }
            let total_resources = resources.len() + resource_templates.len();
            if total_resources > 0 {
                caps.push(format!(
                    "{total_resources} resource{}",
                    if total_resources == 1 { "" } else { "s" }
                ));
            }
            if !prompts.is_empty() {
                caps.push(format!(
                    "{} prompt{}",
                    prompts.len(),
                    if prompts.len() == 1 { "" } else { "s" }
                ));
            }
            eprintln!(
                "[harn] serve mcp: serving {} as '{server_name}'",
                caps.join(", ")
            );

            let mut server =
                harn_vm::McpServer::new(server_name, tools, resources, resource_templates, prompts);
            if let Some(source) = card_source {
                match resolve_card_source(source) {
                    Ok(card) => server = server.with_server_card(card),
                    Err(e) => {
                        eprintln!("error: --card: {e}");
                        process::exit(1);
                    }
                }
            }
            match mode {
                RunFileMcpServeMode::Stdio => {
                    if let Err(e) = server.run(&mut vm).await {
                        eprintln!("error: MCP server error: {e}");
                        process::exit(1);
                    }
                }
                RunFileMcpServeMode::Http {
                    options,
                    auth_policy,
                } => {
                    if let Err(e) = crate::commands::serve::run_script_mcp_http_server(
                        server,
                        vm,
                        options,
                        auth_policy,
                    )
                    .await
                    {
                        eprintln!("error: MCP server error: {e}");
                        process::exit(1);
                    }
                }
            }
        })
        .await;
}

/// Accept either a path to a JSON file or an inline JSON blob and
/// return the parsed `serde_json::Value`. Used by `--card`. Disambiguates
/// by peeking at the first non-whitespace character: `{` → inline JSON,
/// anything else → path.
pub(crate) fn resolve_card_source(source: &str) -> Result<serde_json::Value, String> {
    let trimmed = source.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(source).map_err(|e| format!("inline JSON parse error: {e}"));
    }
    let path = std::path::Path::new(source);
    harn_vm::load_server_card_from_path(path).map_err(|e| format!("{e}"))
}

pub(crate) async fn run_watch(path: &str, denied_builtins: HashSet<String>) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        process::exit(1);
    });
    let watch_dir = abs_path.parent().unwrap_or(Path::new("."));

    eprintln!("\x1b[2m[watch] running {path}...\x1b[0m");
    run_file(
        path,
        false,
        denied_builtins.clone(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let _watcher = {
        let tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let has_harn = event
                        .paths
                        .iter()
                        .any(|p| p.extension().is_some_and(|ext| ext == "harn"));
                    if has_harn {
                        let _ = tx.blocking_send(());
                    }
                }
            }
        })
        .unwrap_or_else(|e| {
            eprintln!("Error setting up file watcher: {e}");
            process::exit(1);
        });
        watcher
            .watch(watch_dir, RecursiveMode::Recursive)
            .unwrap_or_else(|e| {
                eprintln!("Error watching directory: {e}");
                process::exit(1);
            });
        watcher // keep alive
    };

    eprintln!(
        "\x1b[2m[watch] watching {} for .harn changes (ctrl-c to stop)\x1b[0m",
        watch_dir.display()
    );

    loop {
        rx.recv().await;
        // Debounce: let bursts of events settle for 200ms before re-running.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        eprintln!();
        eprintln!("\x1b[2m[watch] change detected, re-running {path}...\x1b[0m");
        run_file(
            path,
            false,
            denied_builtins.clone(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        execute_explain_cost, execute_run, split_eval_header, CliLlmMockMode, RunProfileOptions,
        StdoutPassthroughGuard,
    };
    use std::collections::HashSet;

    #[test]
    fn split_eval_header_no_imports_returns_full_body() {
        let (header, body) = split_eval_header("log(1 + 2)");
        assert_eq!(header, "");
        assert_eq!(body, "log(1 + 2)");
    }

    #[test]
    fn split_eval_header_lifts_leading_imports() {
        let code = "import \"./lib\"\nimport { x } from \"std/math\"\nlog(x)";
        let (header, body) = split_eval_header(code);
        assert_eq!(header, "import \"./lib\"\nimport { x } from \"std/math\"");
        assert_eq!(body, "log(x)");
    }

    #[test]
    fn split_eval_header_keeps_pub_import_and_comments_in_header() {
        let code = "// header comment\npub import { y } from \"./lib\"\n\nfoo()";
        let (header, body) = split_eval_header(code);
        assert_eq!(
            header,
            "// header comment\npub import { y } from \"./lib\"\n"
        );
        assert_eq!(body, "foo()");
    }

    #[test]
    fn split_eval_header_does_not_lift_imports_after_other_statements() {
        let code = "let a = 1\nimport \"./lib\"";
        let (header, body) = split_eval_header(code);
        assert_eq!(header, "");
        assert_eq!(body, "let a = 1\nimport \"./lib\"");
    }

    #[test]
    fn cli_llm_mock_roundtrips_logprobs() {
        let mock = harn_vm::llm::parse_llm_mock_value(&serde_json::json!({
            "text": "visible",
            "logprobs": [{"token": "visible", "logprob": 0.0}]
        }))
        .expect("parse mock");
        assert_eq!(mock.logprobs.len(), 1);

        let line = harn_vm::llm::serialize_llm_mock(mock).expect("serialize mock");
        let value: serde_json::Value = serde_json::from_str(&line).expect("json line");
        assert_eq!(value["logprobs"][0]["token"].as_str(), Some("visible"));

        let reparsed = harn_vm::llm::parse_llm_mock_value(&value).expect("reparse mock");
        assert_eq!(reparsed.logprobs.len(), 1);
        assert_eq!(reparsed.logprobs[0]["logprob"].as_f64(), Some(0.0));
    }

    #[test]
    fn stdout_passthrough_guard_restores_previous_state() {
        let original = harn_vm::set_stdout_passthrough(false);
        {
            let _guard = StdoutPassthroughGuard::enable();
            assert!(harn_vm::set_stdout_passthrough(true));
        }
        assert!(!harn_vm::set_stdout_passthrough(original));
    }

    #[test]
    fn execute_explain_cost_does_not_execute_script() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let script = temp.path().join("main.harn");
        std::fs::write(
            &script,
            r#"
pipeline main() {
  write_file("executed.txt", "bad")
  llm_call("hello", nil, {provider: "mock", model: "mock"})
}
"#,
        )
        .expect("write script");

        let outcome = execute_explain_cost(&script.to_string_lossy());

        assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
        assert!(outcome.stdout.contains("LLM cost estimate"));
        assert!(
            !temp.path().join("executed.txt").exists(),
            "--explain-cost must not execute pipeline side effects"
        );
    }

    #[cfg(feature = "hostlib")]
    #[tokio::test]
    async fn execute_run_installs_hostlib_gate() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            temp.path(),
            r#"
pipeline main() {
  let _ = hostlib_enable("tools:deterministic")
  __io_println("enabled")
}
"#,
        )
        .expect("write script");

        let outcome = execute_run(
            &temp.path().to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;

        assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
        assert_eq!(outcome.stdout.trim(), "enabled");
    }

    #[cfg(all(feature = "hostlib", unix))]
    #[tokio::test]
    async fn execute_run_can_read_hostlib_command_artifacts() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            temp.path(),
            r#"
pipeline main() {
  let _ = hostlib_enable("tools:deterministic")
  let result = hostlib_tools_run_command({
    argv: ["sh", "-c", "i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done"],
    capture: {max_inline_bytes: 8},
    timeout_ms: 5000,
  })
  __io_println(starts_with(result.command_id, "cmd_"))
  __io_println(len(result.stdout))
  __io_println(result.byte_count)
  let window = hostlib_tools_read_command_output({
    command_id: result.command_id,
    offset: 1990,
    length: 20,
  })
  __io_println(len(window.content))
  __io_println(window.eof)
}
"#,
        )
        .expect("write script");

        let outcome = execute_run(
            &temp.path().to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;

        assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
        assert_eq!(outcome.stdout.trim(), "true\n8\n2000\n10\ntrue");
    }
}
