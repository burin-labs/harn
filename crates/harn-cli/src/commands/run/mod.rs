use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harn_parser::DiagnosticSeverity;
use harn_vm::event_log::EventLog;
use serde::Serialize;

use crate::commands::mcp::{self, AuthResolution};
use crate::commands::time::{self, PhaseRecord, RunTiming};
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

/// Post-run summary configuration for `harn run --emit-summary-json`.
#[derive(Clone, Debug)]
pub struct RunSummaryOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug)]
pub struct RunPhaseOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug)]
pub struct RunRusageOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug, Default)]
pub struct RunAuxOptions {
    pub summary: Option<RunSummaryOptions>,
    pub phase: Option<RunPhaseOptions>,
    pub rusage: Option<RunRusageOptions>,
}

#[derive(Clone, Debug, Default)]
pub struct RunControlOptions {
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct RunJsonSink {
    pub target: RunJsonSinkTarget,
    pub fd_flag: &'static str,
}

#[derive(Clone, Debug)]
pub enum RunJsonSinkTarget {
    /// Append the summary to the captured stderr buffer so it remains
    /// terminal after all diagnostics that `run_file_with_skill_dirs`
    /// flushes on return.
    Stderr,
    File(PathBuf),
    Fd(i32),
}

#[derive(Serialize)]
struct RunSummary<'a> {
    schema_version: u32,
    event: &'static str,
    wall_time_ms: u64,
    exit_code: i32,
    llm: RunSummaryLlm,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a harn_vm::profile::RunProfile>,
}

#[derive(Serialize)]
struct RunSummaryLlm {
    call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    time_ms: i64,
    cost_usd: f64,
}

pub const RUN_SUMMARY_SCHEMA_VERSION: u32 = 1;
pub const RUN_PHASE_SCHEMA_VERSION: u32 = 2;
pub const RUN_RUSAGE_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct RunPhaseEvent {
    schema_version: u32,
    event: &'static str,
    phases: Vec<PhaseRecord>,
}

#[derive(Serialize)]
struct RunRusageEvent {
    schema_version: u32,
    event: &'static str,
    cpu_ms: u64,
}

pub(crate) fn run_summary_options_from_args(
    args: &crate::cli::RunArgs,
) -> Option<RunSummaryOptions> {
    args.emit_summary_json.then(|| RunSummaryOptions {
        sink: build_run_json_sink(args.summary_file.clone(), args.summary_fd, "--summary-fd"),
    })
}

pub(crate) fn run_aux_options_from_args(args: &crate::cli::RunArgs) -> RunAuxOptions {
    RunAuxOptions {
        summary: run_summary_options_from_args(args),
        phase: run_phase_options_from_args(args),
        rusage: run_rusage_options_from_args(args),
    }
}

pub(crate) fn run_control_options_from_args(args: &crate::cli::RunArgs) -> RunControlOptions {
    RunControlOptions {
        timeout: args.timeout,
    }
}

pub(crate) fn run_phase_options_from_args(args: &crate::cli::RunArgs) -> Option<RunPhaseOptions> {
    args.emit_phase_json.then(|| RunPhaseOptions {
        sink: build_run_json_sink(args.phase_file.clone(), args.phase_fd, "--phase-fd"),
    })
}

pub(crate) fn run_rusage_options_from_args(args: &crate::cli::RunArgs) -> Option<RunRusageOptions> {
    args.emit_rusage_json.then(|| RunRusageOptions {
        sink: build_run_json_sink(args.rusage_file.clone(), args.rusage_fd, "--rusage-fd"),
    })
}

fn build_run_json_sink(
    file: Option<PathBuf>,
    fd: Option<i32>,
    fd_flag: &'static str,
) -> RunJsonSink {
    RunJsonSink {
        target: if let Some(path) = file {
            RunJsonSinkTarget::File(path)
        } else if let Some(fd) = fd {
            RunJsonSinkTarget::Fd(fd)
        } else {
            RunJsonSinkTarget::Stderr
        },
        fd_flag,
    }
}

pub(crate) enum RunFileMcpServeMode {
    Stdio,
    Http(Box<RunFileMcpServeHttp>),
}

pub(crate) struct RunFileMcpServeHttp {
    pub options: harn_serve::McpHttpServeOptions,
    pub auth_policy: harn_serve::AuthPolicy,
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
    let program = parse_source_for_run(path, &source, stderr)?;
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
        if std::env::var_os(crate::dispatch::CACHE_DEBUG_ENV).is_some() {
            eprintln!("[harn] bytecode cache write skipped: {err}");
        }
    }
    if let Some(t) = timing.as_deref_mut() {
        t.bytecode_compile = compile_step_start.elapsed();
    }

    Some(LoadedChunk { source, chunk })
}

fn parse_source_for_run(
    path: &str,
    source: &str,
    stderr: &mut String,
) -> Option<Vec<harn_parser::SNode>> {
    crate::ensure_builtin_signatures_installed();

    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(error) => {
            let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                source,
                path,
                &error_span_from_lex(&error),
                "error",
                harn_parser::diagnostic::lexer_error_code(&error),
                &error.to_string(),
                Some("here"),
                None,
            );
            stderr.push_str(&diagnostic);
            return None;
        }
    };

    let mut parser = harn_parser::Parser::new(tokens);
    match parser.parse() {
        Ok(program) => Some(program),
        Err(error) => {
            if parser.all_errors().is_empty() {
                render_parse_error(path, source, &error, stderr);
            } else {
                for error in parser.all_errors() {
                    render_parse_error(path, source, error, stderr);
                }
            }
            None
        }
    }
}

fn render_parse_error(
    path: &str,
    source: &str,
    error: &harn_parser::ParserError,
    stderr: &mut String,
) {
    let span = error_span_from_parse(error);
    let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
        source,
        path,
        &span,
        "error",
        harn_parser::diagnostic::parser_error_code(error),
        &harn_parser::diagnostic::parser_error_message(error),
        Some(harn_parser::diagnostic::parser_error_label(error)),
        harn_parser::diagnostic::parser_error_help(error),
    );
    stderr.push_str(&diagnostic);
}

fn error_span_from_lex(error: &harn_lexer::LexerError) -> harn_lexer::Span {
    match error {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

fn error_span_from_parse(error: &harn_parser::ParserError) -> harn_lexer::Span {
    match error {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
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
    let checker = crate::typecheck_imports::checker_with_resolved_imports(
        harn_parser::TypeChecker::new(),
        path,
    );
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
    let wrapped = eval_source_for_code(code);
    let tmp = create_eval_temp_file()?;
    Ok((wrapped, tmp))
}

fn eval_source_for_code(code: &str) -> String {
    if eval_code_parses_as_program(code) {
        return code.to_string();
    }
    let (header, body) = split_eval_header(code);
    if header.is_empty() {
        format!("pipeline main(task) {{\n{body}\n}}")
    } else {
        format!("{header}\npipeline main(task) {{\n{body}\n}}")
    }
}

fn eval_code_parses_as_program(code: &str) -> bool {
    harn_parser::parse_source(code)
        .map(|program| {
            program.iter().any(|node| {
                let (_, inner) = harn_parser::peel_attributes(node);
                matches!(&inner.node, harn_parser::Node::Pipeline { .. })
            })
        })
        .unwrap_or(false)
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSandboxOptions {
    /// Install the default `harn run` sandbox for this invocation.
    pub enabled: bool,
    /// Override the workspace root used by the default sandbox. This is
    /// intended for host-generated scripts whose source file lives outside
    /// the workspace they operate on.
    pub workspace_root: Option<PathBuf>,
    /// Extra writable filesystem roots mounted into the direct-run
    /// sandbox. These extend the write jail without disabling path
    /// enforcement or egress policy.
    pub write_roots: Vec<PathBuf>,
    /// Extra read-only filesystem roots. `path` resolving under one of
    /// these entries is scoped for reads, but writes still fail.
    pub read_only_roots: Vec<PathBuf>,
}

impl Default for RunSandboxOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            workspace_root: None,
            write_roots: Vec::new(),
            read_only_roots: Vec::new(),
        }
    }
}

impl RunSandboxOptions {
    /// Disable the default direct-run sandbox and egress guard.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            workspace_root: None,
            write_roots: Vec::new(),
            read_only_roots: Vec::new(),
        }
    }

    /// Constrain the default sandbox to an explicit workspace root.
    pub fn with_workspace_root(mut self, workspace_root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    /// Add writable roots to the default sandbox policy.
    pub fn with_write_roots<I>(mut self, write_roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.write_roots = write_roots.into_iter().collect();
        self
    }

    /// Add read-only roots to the default sandbox policy.
    pub fn with_read_only_roots<I>(mut self, read_only_roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.read_only_roots = read_only_roots.into_iter().collect();
        self
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
    sandbox: RunSandboxOptions,
    interrupt_tokens: Option<RunInterruptTokens>,
    json: Option<(RunJsonOptions, Box<dyn io::Write + Send>)>,
    aux: RunAuxOptions,
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
        harn_vm::llm::clear_cli_llm_mock_mode();
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
    let result = fs::write(fixture_path, body)
        .map_err(|error| format!("failed to write {}: {error}", fixture_path.display()));
    harn_vm::llm::clear_cli_llm_mock_mode();
    result
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
        RunSandboxOptions::default(),
        None,
        RunAuxOptions::default(),
        RunControlOptions::default(),
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
    sandbox: RunSandboxOptions,
    json: Option<RunJsonOptions>,
    aux: RunAuxOptions,
    control: RunControlOptions,
    harnpack: HarnpackRunOptions,
) {
    // Graceful shutdown: flush run records before exit on SIGINT/SIGTERM.
    let interrupt_tokens = install_signal_shutdown_handler();
    let deadline_guard = control
        .timeout
        .map(|timeout| start_run_deadline_watchdog(timeout, interrupt_tokens.clone()));

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
        sandbox,
        interrupt_tokens: Some(interrupt_tokens.clone()),
        json: json_with_stdout,
        aux,
        timing: None,
        harnpack,
    })
    .await;
    if let Some(guard) = &deadline_guard {
        guard.finish();
    }

    // `harn run` streams normal program stdout during execution. Any stdout
    // left here came from older capture paths, so flush it after diagnostics.
    if !outcome.stderr.is_empty() {
        io::stderr().write_all(outcome.stderr.as_bytes()).ok();
    }
    if !outcome.stdout.is_empty() {
        io::stdout().write_all(outcome.stdout.as_bytes()).ok();
    }

    let mut exit_code = outcome.exit_code;
    if deadline_guard
        .as_ref()
        .is_some_and(RunDeadlineGuard::timed_out)
        || (exit_code != 0 && interrupt_tokens.cancel_token.load(Ordering::SeqCst))
    {
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
    sandbox: RunSandboxOptions,
    json: Option<RunJsonOptions>,
    aux: RunAuxOptions,
    control: RunControlOptions,
) {
    let source = r#"import { resume_agent, wait_agent } from "std/agent/workers"

pipeline main(task) {
  const input = if len(argv) > 1 {
    argv[1]
  } else {
    nil
  }
  const handle = resume_agent(argv[0], input, true)
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
        sandbox,
        json,
        aux,
        control,
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

struct ExecutionPolicyGuard;

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        harn_vm::orchestration::pop_execution_policy();
    }
}

struct RunSandboxScope {
    _execution_policy: Option<ExecutionPolicyGuard>,
    _egress_policy: Option<harn_vm::egress::ExplicitEgressPolicyGuard>,
    _ssrf_guard: Option<harn_vm::egress::SsrfGuardScope>,
}

impl RunSandboxScope {
    fn disabled() -> Self {
        Self {
            _execution_policy: None,
            _egress_policy: None,
            _ssrf_guard: None,
        }
    }
}

fn install_run_sandbox_scope(
    options: &RunSandboxOptions,
    workspace_root: &Path,
    stderr: &mut String,
) -> RunSandboxScope {
    if !options.enabled {
        stderr.push_str(
            "warning: harn run --no-sandbox disables filesystem, process, and egress sandbox defaults\n",
        );
        return RunSandboxScope::disabled();
    }

    let execution_policy = if harn_vm::orchestration::current_execution_policy().is_none() {
        harn_vm::orchestration::push_execution_policy(default_run_capability_policy(
            workspace_root,
            &options.write_roots,
            &options.read_only_roots,
        ));
        Some(ExecutionPolicyGuard)
    } else {
        None
    };
    let egress_policy = Some(harn_vm::egress::require_explicit_egress_policy_for_host());
    // Default-on the SSRF private-address guard for outbound HTTP. Callers can
    // opt out with `egress_policy({block_private:"off"})` /
    // `HARN_EGRESS_BLOCK_PRIVATE=off`.
    let ssrf_guard = Some(harn_vm::egress::require_ssrf_guard_for_host());

    RunSandboxScope {
        _execution_policy: execution_policy,
        _egress_policy: egress_policy,
        _ssrf_guard: ssrf_guard,
    }
}

fn default_run_capability_policy(
    workspace_root: &Path,
    write_roots: &[PathBuf],
    read_only_roots: &[PathBuf],
) -> harn_vm::orchestration::CapabilityPolicy {
    let mut workspace_roots = Vec::with_capacity(1 + write_roots.len());
    workspace_roots.push(
        normalize_run_workspace_root(workspace_root)
            .display()
            .to_string(),
    );
    workspace_roots.extend(
        write_roots
            .iter()
            .map(|path| normalize_run_workspace_root(path.as_path()))
            .map(|path| path.display().to_string()),
    );

    harn_vm::orchestration::CapabilityPolicy {
        workspace_roots,
        read_only_roots: read_only_roots
            .iter()
            .map(|path| normalize_run_workspace_root(path.as_path()))
            .map(|path| path.display().to_string())
            .collect(),
        side_effect_level: Some("process_exec".to_string()),
        sandbox_profile: harn_vm::orchestration::SandboxProfile::Worktree,
        ..harn_vm::orchestration::CapabilityPolicy::default()
    }
}

fn normalize_run_workspace_root(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn default_run_workspace_root(project_root: Option<&Path>, source_parent: &Path) -> PathBuf {
    project_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| source_parent.to_path_buf())
}

fn run_sandbox_attestation(sandbox: &RunSandboxOptions) -> serde_json::Value {
    let active_policy = harn_vm::orchestration::current_execution_policy();
    let active = active_policy.is_some();
    let workspace_roots = active_policy
        .as_ref()
        .map(|policy| policy.workspace_roots.clone())
        .unwrap_or_default();
    let read_only_roots = active_policy
        .as_ref()
        .map(|policy| policy.read_only_roots.clone())
        .unwrap_or_default();
    let profile = active_policy
        .as_ref()
        .map(|policy| policy.sandbox_profile.as_str())
        .unwrap_or("unrestricted");
    let egress = if sandbox.enabled {
        "explicit_policy_required"
    } else if active {
        "host_policy"
    } else {
        "unrestricted"
    };
    let write_roots = sandbox
        .write_roots
        .iter()
        .map(|path| normalize_run_workspace_root(path).display().to_string())
        .collect::<Vec<_>>();

    serde_json::json!({
        "run_default_enabled": sandbox.enabled,
        "active": active,
        "workspace_roots": workspace_roots,
        "write_roots": write_roots,
        "read_only_roots": read_only_roots,
        "profile": profile,
        "egress": egress,
    })
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
const RUN_TIMEOUT_MESSAGE: &str =
    "[harn] run timeout reached, interrupting VM and in-flight subprocesses...";
const RUN_TIMEOUT_HARD_EXIT_MESSAGE: &str = "[harn] run timeout grace elapsed, terminating";
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

struct RunDeadlineGuard {
    finished: Arc<AtomicBool>,
    timed_out: Arc<AtomicBool>,
}

impl RunDeadlineGuard {
    fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }
}

impl Drop for RunDeadlineGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

fn start_run_deadline_watchdog(timeout: Duration, tokens: RunInterruptTokens) -> RunDeadlineGuard {
    let finished = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let task_finished = Arc::clone(&finished);
    let task_timed_out = Arc::clone(&timed_out);
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if task_finished.load(Ordering::SeqCst) {
            return;
        }
        task_timed_out.store(true, Ordering::SeqCst);
        request_vm_interrupt(&tokens, "timeout");
        eprintln!("{RUN_TIMEOUT_MESSAGE}");
        tokio::time::sleep(harn_vm::op_interrupt::SUBPROCESS_TERM_GRACE).await;
        if !task_finished.load(Ordering::SeqCst) {
            signal_run_process_cleanups(&tokens, SIGKILL);
            eprintln!("{RUN_TIMEOUT_HARD_EXIT_MESSAGE}");
            process::exit(124);
        }
    });
    RunDeadlineGuard {
        finished,
        timed_out,
    }
}

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
            // Containers without controlling-tty access can refuse signal()
            // registration. In that case, degrade to no-signal mode rather
            // than crashing the `harn run` script — the script still runs,
            // it just can't be interrupted cleanly.
            let sigterm_stream = signal(SignalKind::terminate()).ok();
            let sigint_stream = signal(SignalKind::interrupt()).ok();
            let sighup_stream = signal(SignalKind::hangup()).ok();
            let (mut sigterm, mut sigint, mut sighup) =
                match (sigterm_stream, sigint_stream, sighup_stream) {
                    (Some(t), Some(i), Some(h)) => (t, i, h),
                    _ => {
                        eprintln!(
                            "[harn] signal handlers unavailable in this environment; \
                             continuing without graceful-shutdown interception"
                        );
                        return;
                    }
                };
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
    signal_run_process_cleanups(tokens, SIGTERM);
}

fn signal_run_process_cleanups(tokens: &RunInterruptTokens, signal: i32) {
    let mut report = harn_vm::op_interrupt::signal_active_process_cleanups_for_cancel_token(
        signal,
        &tokens.cancel_token,
    );
    // Older/integration-created entries may not have an owner token. Keep the
    // fail-closed fallback for the standalone `harn run` process, while the
    // token path above prevents scoped VM runs from becoming process-global.
    if report.root_pid.is_none() && report.children.is_empty() {
        report = harn_vm::op_interrupt::signal_ownerless_active_process_cleanups(signal);
    }
    drop(report);
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
    crate::ensure_builtin_signatures_installed();
    execute_run_with_harnpack_and_sandbox_options(
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        RunSandboxOptions::default(),
        HarnpackRunOptions::default(),
    )
    .await
}

/// [`execute_run`] with an explicit sandbox policy override for in-process
/// callers whose source path is intentionally outside the workspace they
/// operate on.
#[allow(clippy::too_many_arguments)]
pub async fn execute_run_with_sandbox_options(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    sandbox: RunSandboxOptions,
) -> RunOutcome {
    execute_run_with_harnpack_and_sandbox_options(
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        sandbox,
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
    execute_run_with_harnpack_and_sandbox_options(
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
        RunSandboxOptions::default(),
        harnpack,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_run_with_harnpack_and_sandbox_options(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
    sandbox: RunSandboxOptions,
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
        sandbox,
        interrupt_tokens: None,
        json: None,
        aux: RunAuxOptions::default(),
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
        sandbox: RunSandboxOptions::default(),
        interrupt_tokens: None,
        json: Some((options, out)),
        aux: RunAuxOptions::default(),
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
    sandbox: RunSandboxOptions,
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
        sandbox,
        interrupt_tokens: None,
        json: None,
        aux: RunAuxOptions::default(),
        timing,
        harnpack: HarnpackRunOptions::default(),
    })
    .await
}

/// Directory that anchors the entry script's source-relative and `@asset`
/// resolution.
///
/// Returns the script's parent directory, or the current working directory
/// when the path is a bare filename (empty parent) — e.g. `cd project &&
/// harn run main.harn`. The old code skipped setting the source dir in that
/// case, which left the resting thread-local source dir unset (`None`). A
/// dependency provider-connector contract load during `harn run` startup then
/// repointed the thread-local at a dependency generation's `src` and, because the
/// restore-on-return path is a no-op over an unset baseline, left it there —
/// so the entry pipeline's first `render("@alias/...")` resolved against the
/// dependency's `harn.toml` instead of the project root. Always establishing
/// the entry dir keeps that resolution anchored on the project.
fn entry_source_dir(path: &str) -> std::path::PathBuf {
    match std::path::Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    }
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
        sandbox,
        interrupt_tokens,
        json,
        aux,
        timing,
        harnpack,
    } = inputs;
    let RunAuxOptions {
        summary,
        phase,
        rusage,
    } = aux;
    let run_started = Instant::now();
    let cpu_started_ms = rusage.as_ref().map(|_| time::cpu_ms());
    let mut owned_timing = if timing.is_none() && (phase.is_some() || rusage.is_some()) {
        Some(RunTiming::default())
    } else {
        None
    };
    let mut timing = timing.or(owned_timing.as_mut());

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
            Err(err) => {
                return finalize_harnpack_error(
                    stderr,
                    json_session,
                    summary.as_ref(),
                    phase.as_ref(),
                    rusage.as_ref(),
                    run_started,
                    err,
                );
            }
        };
        harn_vm::run_events::emit(harn_vm::run_events::RunEvent::PackRun {
            bundle_hash: outcome.bundle_hash.clone(),
            signature_verified: outcome.signature_verified,
            key_id: outcome.key_id.clone(),
            cache_hit: outcome.cache_hit,
            dry_run_verify: harnpack.dry_run_verify,
        });
        if harnpack.dry_run_verify {
            return finalize_harnpack_dry_run(
                stderr,
                json_session,
                summary.as_ref(),
                phase.as_ref(),
                rusage.as_ref(),
                run_started,
                cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
                &outcome,
            );
        }
        owned_run_path = outcome.entrypoint_path.to_string_lossy().into_owned();
        owned_run_path.as_str()
    } else {
        path
    };

    let Some(LoadedChunk { source, chunk }) =
        compile_or_load_chunk_with_timing(resolved_path, &mut stderr, timing.as_deref_mut())
    else {
        let message = stderr.clone();
        return finalize_run_error(
            stdout,
            stderr,
            json_session,
            summary.as_ref(),
            phase.as_ref(),
            rusage.as_ref(),
            run_started,
            None,
            timing.as_deref(),
            0,
            cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
            "compile_error",
            message,
        );
    };
    let path = resolved_path;

    let setup_start = Instant::now();
    if trace || summary.is_some() {
        harn_vm::llm::enable_tracing();
    }
    if profile.is_enabled() || phase.is_some() {
        harn_vm::tracing::set_tracing_enabled(true);
    }
    if let Err(error) = install_cli_llm_mock_mode(&llm_mock_mode) {
        stderr.push_str(&format!("error: {error}\n"));
        time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
        return finalize_run_error(
            stdout,
            stderr,
            json_session,
            summary.as_ref(),
            phase.as_ref(),
            rusage.as_ref(),
            run_started,
            None,
            timing.as_deref(),
            0,
            cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
            "llm_mock_install",
            error,
        );
    }

    let mut vm = harn_vm::Vm::new();
    if let Some(timing) = timing.as_deref_mut() {
        timing.module_phases = Some(vm.enable_module_phase_timing());
    }
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
    let sandbox_root = sandbox
        .workspace_root
        .clone()
        .unwrap_or_else(|| default_run_workspace_root(project_root.as_deref(), source_parent));
    let _sandbox_scope = install_run_sandbox_scope(&sandbox, &sandbox_root, &mut stderr);
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
                "sandbox": run_sandbox_attestation(&sandbox),
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
    let lazy_manifest_handlers = !denied_builtins.is_empty();
    if lazy_manifest_handlers {
        vm.set_denied_builtins(denied_builtins);
    }
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    // Establish the entry script's directory as the resting source dir. When
    // `path` is a bare filename (empty parent) — e.g. `cd project && harn run
    // main.harn` — anchor on the current working directory instead of skipping,
    // so the resting source dir is never left unset. See `entry_source_dir`.
    vm.set_source_dir(&entry_source_dir(path));

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
        .map(|s| harn_vm::VmValue::String(arcstr::ArcStr::from(s.as_str())))
        .collect();
    vm.set_global(
        "argv",
        harn_vm::VmValue::List(std::sync::Arc::new(argv_values)),
    );

    // Install the script's `Harness` capability handle so the auto-call
    // emitted by `Compiler::compile()` for `fn main(harness: Harness)`
    // entrypoints can read it.
    let runtime_harness =
        match crate::default_harness_for_manifest_or_base_dir(Path::new(path), store_base) {
            Ok(harness) => harness,
            Err(error) => {
                stderr.push_str(&format!(
                    "error: failed to configure harness secret provider: {error}\n"
                ));
                time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
                return finalize_run_error(
                    stdout,
                    stderr,
                    json_session,
                    summary.as_ref(),
                    phase.as_ref(),
                    rusage.as_ref(),
                    run_started,
                    None,
                    timing.as_deref(),
                    0,
                    cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
                    "harness_secret_provider",
                    error,
                );
            }
        };
    vm.set_harness(runtime_harness);

    let extensions = package::load_runtime_extensions(Path::new(path));
    package::install_runtime_extensions(&extensions);
    if let Some(manifest) = extensions.root_manifest.as_ref() {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }
    // An explicit allow/deny policy belongs to the requested target. Defer
    // unrelated manifest handler graphs until they actually fire under this VM.
    if let Err(error) =
        package::install_manifest_triggers_with_mode(&mut vm, &extensions, lazy_manifest_handlers)
            .await
    {
        stderr.push_str(&format!(
            "error: failed to install manifest triggers: {error}\n"
        ));
        time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
        return finalize_run_error(
            stdout,
            stderr,
            json_session,
            summary.as_ref(),
            phase.as_ref(),
            rusage.as_ref(),
            run_started,
            None,
            timing.as_deref(),
            0,
            cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
            "manifest_triggers",
            error.to_string(),
        );
    }
    if let Err(error) =
        package::install_manifest_hooks_with_mode(&mut vm, &extensions, lazy_manifest_handlers)
            .await
    {
        stderr.push_str(&format!(
            "error: failed to install manifest hooks: {error}\n"
        ));
        time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
        return finalize_run_error(
            stdout,
            stderr,
            json_session,
            summary.as_ref(),
            phase.as_ref(),
            rusage.as_ref(),
            run_started,
            None,
            timing.as_deref(),
            0,
            cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
            "manifest_hooks",
            error.to_string(),
        );
    }

    let local = tokio::task::LocalSet::new();
    time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
    let main_start = Instant::now();
    // Re-anchor the entry source dir immediately before executing the entry
    // pipeline. The manifest/dependency setup above (provider-connector
    // contract loads, hook-handler module loads) transiently repoints the
    // thread-local source dir and does not guarantee it is restored to the
    // entry's dir — a dependency provider connector under
    // a dependency generation would otherwise leave the entry pipeline's first
    // `render("@alias/...")` resolving against the dependency's `harn.toml`.
    vm.set_source_dir(&entry_source_dir(path));
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
        let profile_rollup = if profile.is_enabled() {
            Some(harn_vm::profile::build(&harn_vm::tracing::peek_spans()))
        } else {
            None
        };
        return finalize_run_error(
            stdout,
            stderr,
            json_session,
            summary.as_ref(),
            phase.as_ref(),
            rusage.as_ref(),
            run_started,
            profile_rollup.as_ref(),
            timing.as_deref(),
            harn_vm::tracing::peek_spans().len() as u64,
            cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
            "llm_mock_record",
            error,
        );
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
            let profile_rollup = if profile.is_enabled() {
                Some(harn_vm::profile::build(&harn_vm::tracing::peek_spans()))
            } else {
                None
            };
            return finalize_run_error(
                stdout,
                stderr,
                json_session,
                summary.as_ref(),
                phase.as_ref(),
                rusage.as_ref(),
                run_started,
                profile_rollup.as_ref(),
                timing.as_deref(),
                harn_vm::tracing::peek_spans().len() as u64,
                cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
                "attestation",
                error,
            );
        }
        harn_vm::event_log::reset_active_event_log();
    }

    match execution {
        Ok((output, return_value)) => {
            stdout.push_str(output);
            let main_events = harn_vm::tracing::peek_spans().len() as u64;
            let cpu_ms_total = cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start));
            let profile_rollup = if profile.is_enabled() {
                Some(harn_vm::profile::build(&harn_vm::tracing::peek_spans()))
            } else {
                None
            };
            let summary_llm = summary.as_ref().map(|_| run_summary_llm_snapshot());
            if trace {
                stderr.push_str(&render_trace_summary());
            }
            if let Some(profile_rollup) = profile_rollup.as_ref() {
                if let Err(error) =
                    render_and_persist_profile_rollup(&profile, profile_rollup, &mut stderr)
                {
                    stderr.push_str(&format!("warning: failed to write profile: {error}\n"));
                }
            }
            if exit_code != 0 {
                stderr.push_str(&render_return_value_error(&return_value));
            }
            let aux_emission = emit_run_aux_for_exit(
                summary.as_ref(),
                phase.as_ref(),
                rusage.as_ref(),
                run_started,
                exit_code,
                profile_rollup.as_ref(),
                summary_llm,
                timing.as_deref(),
                main_events,
                cpu_ms_total,
                json_session.is_some(),
                &mut stderr,
            );
            if let Some(session) = json_session {
                if let Some(error) = aux_emission.error {
                    let mut outcome = session.finalize_error(
                        "run_aux",
                        format!("failed to emit auxiliary run JSON: {error}"),
                        1,
                    );
                    outcome.stderr = aux_emission.stderr;
                    return outcome;
                }
                let value = harn_vm::llm::vm_value_to_json(&return_value);
                let mut outcome = session.finalize_result(value, aux_emission.exit_code);
                outcome.stderr = aux_emission.stderr;
                return outcome;
            }
            RunOutcome {
                stdout,
                stderr,
                exit_code: aux_emission.exit_code,
            }
        }
        Err(rendered_error) => {
            stderr.push_str(&rendered_error);
            let main_events = harn_vm::tracing::peek_spans().len() as u64;
            let cpu_ms_total = cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start));
            let profile_rollup = if profile.is_enabled() {
                Some(harn_vm::profile::build(&harn_vm::tracing::peek_spans()))
            } else {
                None
            };
            if let Some(profile_rollup) = profile_rollup.as_ref() {
                if let Err(error) =
                    render_and_persist_profile_rollup(&profile, profile_rollup, &mut stderr)
                {
                    stderr.push_str(&format!("warning: failed to write profile: {error}\n"));
                }
            }
            let aux_emission = emit_run_aux_for_exit(
                summary.as_ref(),
                phase.as_ref(),
                rusage.as_ref(),
                run_started,
                1,
                profile_rollup.as_ref(),
                None,
                timing.as_deref(),
                main_events,
                cpu_ms_total,
                json_session.is_some(),
                &mut stderr,
            );
            if let Some(session) = json_session {
                let mut outcome =
                    session.finalize_error("runtime", rendered_error, aux_emission.exit_code);
                outcome.stderr = aux_emission.stderr;
                return outcome;
            }
            RunOutcome {
                stdout,
                stderr,
                exit_code: aux_emission.exit_code,
            }
        }
    }
}

fn render_and_persist_profile_rollup(
    options: &RunProfileOptions,
    profile: &harn_vm::profile::RunProfile,
    stderr: &mut String,
) -> Result<(), String> {
    if options.text {
        stderr.push_str(&harn_vm::profile::render(profile));
    }
    if let Some(path) = options.json_path.as_ref() {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {error}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(profile)
            .map_err(|error| format!("serialize profile: {error}"))?;
        fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

fn build_run_summary<'a>(
    started: Instant,
    exit_code: i32,
    profile: Option<&'a harn_vm::profile::RunProfile>,
    llm: RunSummaryLlm,
) -> RunSummary<'a> {
    RunSummary {
        schema_version: RUN_SUMMARY_SCHEMA_VERSION,
        event: "run_summary",
        wall_time_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        exit_code,
        llm,
        profile,
    }
}

fn run_summary_llm_snapshot() -> RunSummaryLlm {
    let (input_tokens, output_tokens, time_ms, call_count) = harn_vm::llm::peek_trace_summary();
    let cost_usd = harn_vm::llm::peek_total_cost();
    RunSummaryLlm {
        call_count,
        input_tokens,
        output_tokens,
        time_ms,
        cost_usd: if cost_usd.is_finite() { cost_usd } else { 0.0 },
    }
}

struct RunAuxEmission {
    stderr: String,
    exit_code: i32,
    error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn emit_run_aux_for_exit(
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    exit_code: i32,
    profile: Option<&harn_vm::profile::RunProfile>,
    llm: Option<RunSummaryLlm>,
    timing: Option<&RunTiming>,
    main_events: u64,
    cpu_ms_total: Option<u64>,
    json_mode: bool,
    stderr: &mut String,
) -> RunAuxEmission {
    let mut aux_stderr = String::new();
    let mut final_exit_code = exit_code;
    let mut aux_error = None;
    let aux_target = if json_mode { &mut aux_stderr } else { stderr };
    let default_timing = RunTiming::default();
    let timing = timing.unwrap_or(&default_timing);

    if let Some(options) = summary {
        let llm = llm.unwrap_or_else(run_summary_llm_snapshot);
        let summary = build_run_summary(started, exit_code, profile, llm);
        if let Err(error) = emit_raw_json_line(&options.sink, &summary, "run summary", aux_target) {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run summary",
                error,
            );
        }
    }
    if let Some(options) = phase {
        let phase_event = RunPhaseEvent {
            schema_version: RUN_PHASE_SCHEMA_VERSION,
            event: "run_phase",
            phases: time::build_phase_records(timing, main_events),
        };
        if let Err(error) = emit_raw_json_line(&options.sink, &phase_event, "run phase", aux_target)
        {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run phase",
                error,
            );
        }
    }
    if let Some(options) = rusage {
        let rusage_event = RunRusageEvent {
            schema_version: RUN_RUSAGE_SCHEMA_VERSION,
            event: "run_rusage",
            cpu_ms: cpu_ms_total.unwrap_or(0),
        };
        if let Err(error) =
            emit_raw_json_line(&options.sink, &rusage_event, "run rusage", aux_target)
        {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run rusage",
                error,
            );
        }
    }

    RunAuxEmission {
        stderr: aux_stderr,
        exit_code: final_exit_code,
        error: aux_error,
    }
}

fn record_aux_error(
    final_exit_code: &mut i32,
    aux_error: &mut Option<String>,
    stderr: &mut String,
    label: &str,
    error: String,
) {
    stderr.push_str(&format!("error: failed to emit {label}: {error}\n"));
    if *final_exit_code == 0 {
        *final_exit_code = 1;
    }
    if aux_error.is_none() {
        *aux_error = Some(error);
    }
}

fn emit_raw_json_line(
    sink: &RunJsonSink,
    value: &impl Serialize,
    label: &str,
    stderr: &mut String,
) -> Result<(), String> {
    let line =
        serde_json::to_string(value).map_err(|error| format!("serialize {label}: {error}"))? + "\n";
    match &sink.target {
        RunJsonSinkTarget::Stderr => {
            stderr.push_str(&line);
            Ok(())
        }
        RunJsonSinkTarget::File(path) => write_raw_json_file(path, &line),
        RunJsonSinkTarget::Fd(fd) => write_raw_json_fd(*fd, &line, sink.fd_flag),
    }
}

fn write_raw_json_file(path: &Path, line: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, line).map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(unix)]
fn write_raw_json_fd(fd: i32, line: &str, flag: &str) -> Result<(), String> {
    use std::fs::File;
    use std::os::unix::io::FromRawFd;

    if fd < 0 {
        return Err(format!("invalid {flag} {fd}: must be non-negative"));
    }
    let duped = unsafe { libc::dup(fd) };
    if duped < 0 {
        return Err(format!(
            "duplicate {flag} {fd}: {}",
            io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { File::from_raw_fd(duped) };
    file.write_all(line.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| format!("write {flag} {fd}: {error}"))
}

#[cfg(not(unix))]
fn write_raw_json_fd(_fd: i32, _line: &str, flag: &str) -> Result<(), String> {
    Err(format!("{flag} is only supported on Unix platforms"))
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

#[allow(clippy::too_many_arguments)]
fn finalize_run_error(
    stdout: String,
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    profile: Option<&harn_vm::profile::RunProfile>,
    timing: Option<&RunTiming>,
    main_events: u64,
    cpu_ms_total: Option<u64>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> RunOutcome {
    let aux_emission = emit_run_aux_for_exit(
        summary,
        phase,
        rusage,
        started,
        1,
        profile,
        None,
        timing,
        main_events,
        cpu_ms_total,
        json_session.is_some(),
        &mut stderr,
    );
    if let Some(session) = json_session {
        let mut outcome = session.finalize_error(code, message, aux_emission.exit_code);
        outcome.stderr = aux_emission.stderr;
        return outcome;
    }
    RunOutcome {
        stdout,
        stderr,
        exit_code: aux_emission.exit_code,
    }
}

/// Translate a preflight failure into either the `--json` error event
/// stream or a plain stderr message plus exit-code 1. Keeps the
/// `.harnpack` verify path's error reporting consistent with the rest
/// of `harn run`.
fn finalize_harnpack_error(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    err: HarnpackError,
) -> RunOutcome {
    let code = err.code;
    let message = err.message;
    stderr.push_str(&format!("error: {message}\n"));
    finalize_run_error(
        String::new(),
        stderr,
        json_session,
        summary,
        phase,
        rusage,
        started,
        None,
        None,
        0,
        None,
        code,
        message,
    )
}

/// Successful `--dry-run-verify` path. Reports the bundle hash and
/// signature outcome on stderr (since stdout belongs to the script) and
/// emits a terminal `result` event when `--json` is active so consumers
/// see the run complete.
fn finalize_harnpack_dry_run(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary_options: Option<&RunSummaryOptions>,
    phase_options: Option<&RunPhaseOptions>,
    rusage_options: Option<&RunRusageOptions>,
    started: Instant,
    cpu_ms_total: Option<u64>,
    prepared: &PreparedHarnpack,
) -> RunOutcome {
    let summary = format!(
        "[harn] harnpack verify ok: bundle_hash={}, signature_verified={}, cache_hit={}\n",
        prepared.bundle_hash, prepared.signature_verified, prepared.cache_hit
    );
    stderr.push_str(&summary);
    let aux_emission = emit_run_aux_for_exit(
        summary_options,
        phase_options,
        rusage_options,
        started,
        0,
        None,
        None,
        None,
        0,
        cpu_ms_total,
        json_session.is_some(),
        &mut stderr,
    );
    if let Some(session) = json_session {
        if let Some(error) = aux_emission.error {
            let mut outcome = session.finalize_error(
                "run_aux",
                format!("failed to emit auxiliary run JSON: {error}"),
                1,
            );
            outcome.stderr = aux_emission.stderr;
            return outcome;
        }
        let value = serde_json::json!({
            "bundle_hash": prepared.bundle_hash,
            "signature_verified": prepared.signature_verified,
            "key_id": prepared.key_id,
            "cache_hit": prepared.cache_hit,
            "dry_run_verify": true,
        });
        let mut outcome = session.finalize_result(value, aux_emission.exit_code);
        outcome.stderr = aux_emission.stderr;
        return outcome;
    }
    RunOutcome {
        stdout: String::new(),
        stderr,
        exit_code: aux_emission.exit_code,
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
                AuthResolution::StaticBearer(token) => Some(token),
                AuthResolution::OAuthStore => None,
                AuthResolution::None => server.auth_token.clone(),
            },
            "token_exchange": server.token_exchange.clone(),
            "protocol_version": server.protocol_version,
            "protocol_mode": server.protocol_mode,
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
        vm.set_global("mcp", harn_vm::VmValue::dict(mcp_dict));
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
    // Anchor on the entry script's directory (cwd when the path is a bare
    // filename); never leave the resting source dir unset. See
    // `entry_source_dir`.
    vm.set_source_dir(&entry_source_dir(path));

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

    // Re-anchor the entry source dir immediately before executing the entry
    // pipeline, so manifest/dependency setup can't leave a leaked source dir
    // in place for the pipeline's first `render("@alias/...")`. See the sibling
    // `execute_run_inner`.
    vm.set_source_dir(&entry_source_dir(path));
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
            let metadata = harn_vm::take_mcp_serve_metadata();

            let mut server_name = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("harn")
                .to_string();
            if let Some(name) = metadata
                .as_ref()
                .and_then(|metadata| metadata.name.as_ref())
            {
                server_name = name.clone();
            }

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
            if let Some(metadata) = metadata {
                server = server.with_metadata(metadata);
            }
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
                RunFileMcpServeMode::Http(http) => {
                    let RunFileMcpServeHttp {
                        options,
                        auth_policy,
                    } = *http;
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
mod tests;
