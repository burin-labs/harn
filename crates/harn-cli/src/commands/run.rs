use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_parser::DiagnosticSeverity;
use harn_vm::event_log::EventLog;

use crate::commands::mcp::{self, AuthResolution};
use crate::package;
use crate::parse_source_file;
use crate::skill_loader::{
    canonicalize_cli_dirs, emit_loader_warnings, install_skills_global, load_skills,
    SkillLoaderInputs,
};

mod explain_cost;

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

/// Captured outcome of an in-process `execute_run` invocation. Tests use this
/// instead of spawning the `harn` binary; the binary entry point translates
/// it into real stdout/stderr writes + `process::exit`.
#[derive(Clone, Debug, Default)]
pub struct RunOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

fn load_cli_llm_mocks(path: &Path) -> Result<Vec<harn_vm::llm::LlmMock>, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut mocks = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "invalid JSON in {} line {}: {error}",
                path.display(),
                line_no
            )
        })?;
        mocks.push(parse_cli_llm_mock_value(&value).map_err(|error| {
            format!(
                "invalid --llm-mock fixture in {} line {}: {error}",
                path.display(),
                line_no
            )
        })?);
    }
    Ok(mocks)
}

fn parse_cli_llm_mock_value(value: &serde_json::Value) -> Result<harn_vm::llm::LlmMock, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "fixture line must be a JSON object".to_string())?;

    let match_pattern = optional_string_field(object, "match")?;
    let consume_on_match = object
        .get("consume_match")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let text = optional_string_field(object, "text")?.unwrap_or_default();
    let input_tokens = optional_i64_field(object, "input_tokens")?;
    let output_tokens = optional_i64_field(object, "output_tokens")?;
    let cache_read_tokens = optional_i64_field(object, "cache_read_tokens")?;
    let cache_write_tokens = optional_i64_field(object, "cache_write_tokens")?
        .or(optional_i64_field(object, "cache_creation_input_tokens")?);
    let thinking = optional_string_field(object, "thinking")?;
    let thinking_summary = optional_string_field(object, "thinking_summary")?;
    let stop_reason = optional_string_field(object, "stop_reason")?;
    let model = optional_string_field(object, "model")?.unwrap_or_else(|| "mock".to_string());
    let provider = optional_string_field(object, "provider")?;
    let blocks = optional_vec_field(object, "blocks")?;
    let logprobs = optional_vec_field(object, "logprobs")?.unwrap_or_default();
    let tool_calls = parse_cli_llm_tool_calls(object.get("tool_calls"))?;
    let error = parse_cli_llm_mock_error(object.get("error"))?;

    Ok(harn_vm::llm::LlmMock {
        text,
        tool_calls,
        match_pattern,
        consume_on_match,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        thinking,
        thinking_summary,
        stop_reason,
        model,
        provider,
        blocks,
        logprobs,
        error,
    })
}

fn parse_cli_llm_tool_calls(
    value: Option<&serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| "tool_calls must be an array".to_string())?;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            normalize_cli_llm_tool_call(item).map_err(|error| format!("tool_calls[{idx}] {error}"))
        })
        .collect()
}

fn normalize_cli_llm_tool_call(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "must be a JSON object".to_string())?;
    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "is missing string field `name`".to_string())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .or_else(|| object.get("args").cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::json!({
        "name": name,
        "arguments": arguments,
    }))
}

fn parse_cli_llm_mock_error(
    value: Option<&serde_json::Value>,
) -> Result<Option<harn_vm::llm::MockError>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        "error must be an object {category, message, retry_after_ms?}".to_string()
    })?;
    let category_str = object
        .get("category")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "error.category is required".to_string())?;
    let category = harn_vm::ErrorCategory::parse(category_str);
    if category.as_str() != category_str {
        return Err(format!("unknown error category `{category_str}`"));
    }
    let message = object
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let retry_after_ms = match object.get("retry_after_ms") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(v) => Some(v),
            None => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
        },
        Some(_) => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
    };
    Ok(Some(harn_vm::llm::MockError {
        category,
        message,
        retry_after_ms,
    }))
}

fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn optional_i64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn optional_vec_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<Vec<serde_json::Value>>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(items)) => Ok(Some(items.clone())),
        Some(_) => Err(format!("`{key}` must be an array")),
    }
}

pub fn install_cli_llm_mock_mode(mode: &CliLlmMockMode) -> Result<(), String> {
    harn_vm::llm::clear_cli_llm_mock_mode();
    match mode {
        CliLlmMockMode::Off => Ok(()),
        CliLlmMockMode::Replay { fixture_path } => {
            let mocks = load_cli_llm_mocks(fixture_path)?;
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
        .map(serialize_cli_llm_mock)
        .collect::<Result<Vec<_>, _>>()?;
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(fixture_path, body)
        .map_err(|error| format!("failed to write {}: {error}", fixture_path.display()))
}

fn serialize_cli_llm_mock(mock: harn_vm::llm::LlmMock) -> Result<String, String> {
    let mut object = serde_json::Map::new();
    if let Some(match_pattern) = mock.match_pattern {
        object.insert(
            "match".to_string(),
            serde_json::Value::String(match_pattern),
        );
    }
    if !mock.text.is_empty() {
        object.insert("text".to_string(), serde_json::Value::String(mock.text));
    }
    if !mock.tool_calls.is_empty() {
        let tool_calls = mock
            .tool_calls
            .into_iter()
            .map(|tool_call| {
                let object = tool_call
                    .as_object()
                    .ok_or_else(|| "recorded tool call must be an object".to_string())?;
                let name = object
                    .get("name")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "recorded tool call is missing `name`".to_string())?;
                Ok(serde_json::json!({
                    "name": name,
                    "args": object
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        object.insert(
            "tool_calls".to_string(),
            serde_json::Value::Array(tool_calls),
        );
    }
    if let Some(input_tokens) = mock.input_tokens {
        object.insert(
            "input_tokens".to_string(),
            serde_json::Value::Number(input_tokens.into()),
        );
    }
    if let Some(output_tokens) = mock.output_tokens {
        object.insert(
            "output_tokens".to_string(),
            serde_json::Value::Number(output_tokens.into()),
        );
    }
    if let Some(cache_read_tokens) = mock.cache_read_tokens {
        object.insert(
            "cache_read_tokens".to_string(),
            serde_json::Value::Number(cache_read_tokens.into()),
        );
    }
    if let Some(cache_write_tokens) = mock.cache_write_tokens {
        object.insert(
            "cache_write_tokens".to_string(),
            serde_json::Value::Number(cache_write_tokens.into()),
        );
        object.insert(
            "cache_creation_input_tokens".to_string(),
            serde_json::Value::Number(cache_write_tokens.into()),
        );
    }
    if let Some(thinking) = mock.thinking {
        object.insert("thinking".to_string(), serde_json::Value::String(thinking));
    }
    if let Some(stop_reason) = mock.stop_reason {
        object.insert(
            "stop_reason".to_string(),
            serde_json::Value::String(stop_reason),
        );
    }
    object.insert("model".to_string(), serde_json::Value::String(mock.model));
    if let Some(provider) = mock.provider {
        object.insert("provider".to_string(), serde_json::Value::String(provider));
    }
    if let Some(blocks) = mock.blocks {
        object.insert("blocks".to_string(), serde_json::Value::Array(blocks));
    }
    if !mock.logprobs.is_empty() {
        object.insert(
            "logprobs".to_string(),
            serde_json::Value::Array(mock.logprobs),
        );
    }
    if let Some(error) = mock.error {
        object.insert(
            "error".to_string(),
            serde_json::json!({
                "category": error.category.as_str(),
                "message": error.message,
            }),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(object))
        .map_err(|error| format!("failed to serialize recorded fixture: {error}"))
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

pub(crate) async fn run_file_with_skill_dirs(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    skill_dirs_raw: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
) {
    // Graceful shutdown: flush run records before exit on SIGINT/SIGTERM.
    let cancelled = install_signal_shutdown_handler();

    let _stdout_passthrough = StdoutPassthroughGuard::enable();
    let outcome = execute_run(
        path,
        trace,
        denied_builtins,
        script_argv,
        skill_dirs_raw,
        llm_mock_mode,
        attestation,
        profile,
    )
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
    if exit_code != 0 && cancelled.load(Ordering::SeqCst) {
        exit_code = 124;
    }
    if exit_code != 0 {
        process::exit(exit_code);
    }
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

struct StdoutPassthroughGuard {
    previous: bool,
}

impl StdoutPassthroughGuard {
    fn enable() -> Self {
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

fn install_signal_shutdown_handler() -> Arc<AtomicBool> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => {},
                _ = sigint.recv() => {},
            }
            cancelled_clone.store(true, Ordering::SeqCst);
            eprintln!("[harn] signal received, flushing state...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            process::exit(124);
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            cancelled_clone.store(true, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            process::exit(124);
        }
    });
    cancelled
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
    let mut stderr = String::new();
    let mut stdout = String::new();

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

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            stderr.push_str(&format!("error: compile error: {e}\n"));
            return RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            };
        }
    };

    if trace {
        harn_vm::llm::enable_tracing();
    }
    if profile.is_enabled() {
        harn_vm::tracing::set_tracing_enabled(true);
    }
    if let Err(error) = install_cli_llm_mock_mode(&llm_mock_mode) {
        stderr.push_str(&format!("error: {error}\n"));
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    let mut vm = harn_vm::Vm::new();
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
        return RunOutcome {
            stdout,
            stderr,
            exit_code: 1,
        };
    }

    // Run inside a LocalSet so spawn_local works for concurrency builtins.
    let local = tokio::task::LocalSet::new();
    let execution = local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(value) => Ok((vm.output(), value)),
                Err(e) => Err(vm.format_runtime_error(&e)),
            }
        })
        .await;
    if let Err(error) = persist_cli_llm_mock_recording(&llm_mock_mode) {
        stderr.push_str(&format!("error: {error}\n"));
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
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => 1,
        _ => 0,
    }
}

fn render_return_value_error(value: &harn_vm::VmValue) -> String {
    let harn_vm::VmValue::EnumVariant {
        enum_name,
        variant,
        fields,
    } = value
    else {
        return String::new();
    };
    if enum_name.as_ref() != "Result" || variant.as_ref() != "Err" {
        return String::new();
    }
    let rendered = fields.first().map(|p| p.display()).unwrap_or_default();
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
                mcp_dict.insert(server.name.clone(), harn_vm::VmValue::McpClient(handle));
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

fn render_trace_summary() -> String {
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
    let (source, program) = crate::parse_source_file(path);

    let type_diagnostics = typecheck_with_imports(&program, Path::new(path), &source);
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
                eprint!("{rendered}");
                process::exit(1);
            }
            DiagnosticSeverity::Warning => {
                let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
                eprint!("{rendered}");
            }
        }
    }

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: compile error: {e}");
            process::exit(1);
        }
    };

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
        execute_explain_cost, execute_run, parse_cli_llm_mock_value, serialize_cli_llm_mock,
        split_eval_header, CliLlmMockMode, RunProfileOptions, StdoutPassthroughGuard,
    };
    use std::collections::HashSet;

    #[test]
    fn split_eval_header_no_imports_returns_full_body() {
        let (header, body) = split_eval_header("println(1 + 2)");
        assert_eq!(header, "");
        assert_eq!(body, "println(1 + 2)");
    }

    #[test]
    fn split_eval_header_lifts_leading_imports() {
        let code = "import \"./lib\"\nimport { x } from \"std/math\"\nprintln(x)";
        let (header, body) = split_eval_header(code);
        assert_eq!(header, "import \"./lib\"\nimport { x } from \"std/math\"");
        assert_eq!(body, "println(x)");
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
        let mock = parse_cli_llm_mock_value(&serde_json::json!({
            "text": "visible",
            "logprobs": [{"token": "visible", "logprob": 0.0}]
        }))
        .expect("parse mock");
        assert_eq!(mock.logprobs.len(), 1);

        let line = serialize_cli_llm_mock(mock).expect("serialize mock");
        let value: serde_json::Value = serde_json::from_str(&line).expect("json line");
        assert_eq!(value["logprobs"][0]["token"].as_str(), Some("visible"));

        let reparsed = parse_cli_llm_mock_value(&value).expect("reparse mock");
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
  println("enabled")
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
  println(starts_with(result.command_id, "cmd_"))
  println(len(result.stdout))
  println(result.byte_count)
  let window = hostlib_tools_read_command_output({
    command_id: result.command_id,
    offset: 1990,
    length: 20,
  })
  println(len(window.content))
  println(window.eof)
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
