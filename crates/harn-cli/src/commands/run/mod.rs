use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::commands::time::{self, RunTiming};
use crate::package;
use crate::skill_loader::{
    canonicalize_cli_dirs, emit_loader_warnings, install_skills_global, load_skills,
    SkillLoaderInputs,
};
use harn_parser::DiagnosticSeverity;

mod chunk_loading;
pub(crate) mod environment;
mod eval_source;
mod explain_cost;
pub mod harnpack;
mod interrupts;
pub mod json_events;
mod lifecycle;
mod llm_mock;
mod manifest_runtime;
mod mcp_serve;
mod reporting;
pub(crate) mod sandbox;

pub(crate) use self::chunk_loading::{
    compile_or_load_chunk_for_run, compile_or_load_chunk_with_timing, LoadedChunk,
};
use self::chunk_loading::{parse_source_for_run, typecheck_with_imports};
pub(crate) use self::environment::{EnvironmentPolicyArg, EnvironmentPolicyConfig};
use self::eval_source::create_eval_temp_file;
pub(crate) use self::eval_source::prepare_eval_temp_file;
#[cfg(test)]
use self::eval_source::{eval_source_for_code, split_eval_header};
use self::harnpack::{HarnpackError, HarnpackRunOptions, PreparedHarnpack};
use self::interrupts::{
    install_signal_shutdown_handler, start_run_deadline_watchdog, RunDeadlineGuard,
};
use self::json_events::NdjsonEmitter;
pub use self::lifecycle::RunProfileOptions;
use self::lifecycle::{RunExecution, TerminalRun};
pub use self::llm_mock::*;
pub(crate) use self::manifest_runtime::connect_mcp_servers;
pub(crate) use self::mcp_serve::{
    resolve_card_source, run_file_mcp_serve, RunFileAppServe, RunFileMcpServeHttp,
    RunFileMcpServeMode,
};
use self::reporting::{
    append_run_provenance_event, emit_run_attestation, emit_run_aux_for_exit,
    exit_code_from_return_value, now_ms, render_and_persist_profile_rollup,
    run_summary_llm_snapshot,
};
pub(crate) use self::reporting::{
    render_trace_summary, run_aux_options_from_args, run_control_options_from_args,
};
pub use self::reporting::{
    RunAuxOptions, RunControlOptions, RunJsonOptions, RunJsonSink, RunJsonSinkTarget,
    RunPhaseOptions, RunRusageOptions, RunSummaryOptions, RUN_PHASE_SCHEMA_VERSION,
    RUN_RUSAGE_SCHEMA_VERSION, RUN_SUMMARY_SCHEMA_VERSION,
};
#[cfg(test)]
use self::sandbox::default_run_capability_policy;
pub use self::sandbox::RunSandboxOptions;
use self::sandbox::{
    default_run_workspace_root, install_run_sandbox_scope, run_sandbox_attestation,
};

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunAttestationOptions {
    pub receipt_out: Option<PathBuf>,
    pub agent_id: Option<String>,
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
    json: Option<JsonRunSession>,
    aux: RunAuxOptions,
    timing: Option<&'a mut RunTiming>,
    harnpack: HarnpackRunOptions,
    defer_project_handlers: bool,
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

pub(crate) async fn run_file(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
    llm_mock_mode: CliLlmMockMode,
    attestation: Option<RunAttestationOptions>,
    profile: RunProfileOptions,
) {
    let exit_code = run_file_with_skill_dirs(
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
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

pub(crate) fn run_explain_cost_file_with_skill_dirs(path: &str) -> i32 {
    let outcome = execute_explain_cost(path);
    if !outcome.stderr.is_empty() {
        io::stderr().write_all(outcome.stderr.as_bytes()).ok();
    }
    if !outcome.stdout.is_empty() {
        io::stdout().write_all(outcome.stdout.as_bytes()).ok();
    }
    outcome.exit_code
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
) -> i32 {
    // Graceful shutdown: flush run records before exit on SIGINT/SIGTERM.
    let interrupt_tokens = install_signal_shutdown_handler();
    let deadline_guard = control
        .timeout
        .map(|timeout| start_run_deadline_watchdog(timeout, interrupt_tokens.clone()));

    let _stdout_passthrough = StdoutPassthroughGuard::enable();
    let json_session = json.map(|options| {
        JsonRunSession::new(options, Box::new(io::stdout()) as Box<dyn io::Write + Send>)
    });
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
        json: json_session,
        aux,
        timing: None,
        harnpack,
        defer_project_handlers: control.defer_project_handlers,
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
    exit_code
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
) -> i32 {
    let source = r#"import { resume_agent, wait_agent } from "std/agent/workers"

pipeline main(harness: Harness) {
  const input = if len(argv) > 1 {
    argv[1]
  } else {
    nil
  }
  const handle = resume_agent(harness.agent, argv[0], input, true)
  return wait_agent(harness.agent, handle)
}
"#;
    let tmp = match create_eval_temp_file() {
        Ok(tmp) => tmp,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let tmp_path = tmp.path().to_path_buf();
    if let Err(error) = fs::write(&tmp_path, source) {
        eprintln!("error: failed to write temp file for --resume: {error}");
        return 1;
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
    .await
}

pub fn execute_explain_cost(path: &str) -> RunOutcome {
    let stdout = String::new();
    let mut stderr = String::new();

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            stderr.push_str(&format!("Error reading {path}: {error}\n"));
            return RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            };
        }
    };
    let program = match parse_source_for_run(path, &source, &mut stderr) {
        Some(program) => program,
        None => {
            return RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            };
        }
    };

    let mut had_type_error = false;
    let type_diagnostics = match typecheck_with_imports(&program, Path::new(path), &source) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            stderr.push_str(&format!("error: {error}\n"));
            return RunOutcome {
                stdout,
                stderr,
                exit_code: 1,
            };
        }
    };
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
        defer_project_handlers: false,
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
        json: Some(JsonRunSession::new(options, out)),
        aux: RunAuxOptions::default(),
        timing: None,
        harnpack: HarnpackRunOptions::default(),
        defer_project_handlers: false,
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
        defer_project_handlers: false,
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
    let mut inputs = inputs;
    let json_session = inputs.json.take();
    let Some(json_session) = json_session else {
        return execute_run_inner_scoped(inputs, None).await;
    };
    let sink = json_session.sink();
    harn_vm::run_events::scope(sink, execute_run_inner_scoped(inputs, Some(json_session))).await
}

async fn execute_run_inner_scoped(
    inputs: ExecuteRunInputs<'_>,
    json_session: Option<JsonRunSession>,
) -> RunOutcome {
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
        json: _,
        aux,
        timing,
        harnpack,
        defer_project_handlers,
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

    let mut stderr = String::new();
    let mut stdout = String::new();

    // `.harnpack` preflight: verify signature + replay archive into the
    // content-addressed cache before we touch the chunk loader. The
    // outcome path (entrypoint inside the unpacked tree) replaces the
    // CLI-supplied `path` for everything below.
    let owned_run_path: String;
    let mut prepared_harnpack: Option<PreparedHarnpack> = None;
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
            execution_artifact_state: outcome.execution_artifact_state.to_string(),
            fallback_reason: outcome.fallback_reason.clone(),
            artifact_decode_ms: outcome.artifact_decode_elapsed.as_millis() as u64,
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
        prepared_harnpack = Some(outcome);
        owned_run_path.as_str()
    } else {
        path
    };

    let mut linked_runtime = None;
    let loaded = if let Some(linked) = prepared_harnpack
        .as_mut()
        .and_then(|prepared| prepared.linked_program.take())
    {
        let source = match std::fs::read_to_string(resolved_path) {
            Ok(source) => source,
            Err(error) => {
                stderr.push_str(&format!("Error reading {resolved_path}: {error}\n"));
                return finalize_run_error(
                    stdout,
                    stderr.clone(),
                    json_session,
                    summary.as_ref(),
                    phase.as_ref(),
                    rusage.as_ref(),
                    run_started,
                    None,
                    timing.as_deref(),
                    0,
                    cpu_started_ms.map(|start| time::cpu_ms().saturating_sub(start)),
                    "linked_program_source",
                    stderr,
                );
            }
        };
        let source_root = prepared_harnpack
            .as_ref()
            .expect("linked program came from a prepared pack")
            .cache_dir
            .join("sources");
        let runtime = linked.into_runtime(&source_root);
        let chunk = runtime.entry_chunk.clone();
        linked_runtime = Some(runtime);
        Some(LoadedChunk {
            source,
            chunk,
            link_table: None,
        })
    } else {
        compile_or_load_chunk_with_timing(resolved_path, &mut stderr, timing.as_deref_mut())
    };
    let Some(LoadedChunk {
        source,
        chunk,
        link_table,
    }) = loaded
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
    // Per-builtin recording is only paid for when a profile is asked for: the
    // categorical buckets fold every non-LLM, non-tool builtin into
    // `residual`, which cannot name what a slow run is waiting on. The guard
    // lives for the rest of this function, so recording ends with the run on
    // every exit path below.
    let _builtin_profile_guard = profile.is_enabled().then(harn_vm::builtin_profile::enable);
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
    vm.set_graph_link_table(link_table);
    if let Some(runtime) = &linked_runtime {
        vm.set_linked_program_runtime(runtime);
    }
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

    // Launch the session's environment policy so this run's
    // subprocesses build their environment through the closed allowlist +
    // grants resolver (harn#4992). A launch failure — a missing launcher
    // variable, or a grant on an isolated policy — fails the run loudly rather
    // than silently dropping the credential. Held for the run's duration; on
    // drop the ambient environment policy is cleared.
    let (_environment_scope, environment_policy, grant_receipts) =
        match environment::launch_scope(&sandbox.environment, &mut stderr) {
            Ok(launched) => launched,
            Err(error) => {
                stderr.push_str(&format!("error: {error}\n"));
                time::record_run_setup_elapsed(timing.as_deref_mut(), setup_start);
                let code = error.code();
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
                    code,
                    error.to_string(),
                );
            }
        };

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
                "environment_policy": environment_policy.as_str(),
                // Non-secret grant receipts; never the granted value.
                "environment_grants": &grant_receipts,
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
    let defer_manifest_handlers = defer_project_handlers || !denied_builtins.is_empty();
    if !denied_builtins.is_empty() {
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

    // An explicit allow/deny policy belongs to the requested target. Defer
    // unrelated manifest handler graphs until they actually fire under this VM.
    let _manifest_runtime = match manifest_runtime::install_manifest_runtime(
        Path::new(path),
        store_base,
        &mut vm,
        defer_manifest_handlers,
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(error) => {
            stderr.push_str(&format!(
                "error: failed to install {}: {error}\n",
                error.label()
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
                error.phase(),
                error.to_string(),
            );
        }
    };

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
                Ok(value) => RunExecution::Terminal(TerminalRun::Returned(value)),
                Err(error) => match error.process_exit_code() {
                    Some(code) => RunExecution::Terminal(TerminalRun::ProcessExited(code)),
                    None => RunExecution::Failed(vm.format_runtime_error(&error)),
                },
            }
        })
        .await;
    let output = vm.output();
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
        RunExecution::Terminal(terminal) => terminal.exit_code(),
        RunExecution::Failed(_) => 1,
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
        RunExecution::Terminal(terminal) => {
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
            if let Some(diagnostic) = terminal.nonzero_return_diagnostic() {
                stderr.push_str(&diagnostic);
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
                let value = terminal.json_value();
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
        RunExecution::Failed(rendered_error) => {
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

/// State for a single `harn run --json` invocation. `execute_run_inner`
/// attaches its sink to the run's ambient execution scope, including setup and
/// terminal handling, so every observable event stays in this stream.
///
/// `finalize_result` / `finalize_error` emit the terminal event and
/// build a [`RunOutcome`] whose stdout/stderr captured-buffer fields
/// stay **empty** — the canonical stream is on `out`.
/// `outcome.exit_code` still carries the process exit code so the
/// binary entry can `process::exit(...)`.
struct JsonRunSession {
    emitter: self::json_events::NdjsonEmitter,
}

impl JsonRunSession {
    fn new(options: RunJsonOptions, out: Box<dyn io::Write + Send>) -> Self {
        Self {
            emitter: NdjsonEmitter::new(out, options.quiet),
        }
    }

    fn sink(&self) -> Arc<dyn harn_vm::run_events::RunEventSink> {
        self.emitter.sink()
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
        "[harn] harnpack verify ok: bundle_hash={}, signature_verified={}, cache_hit={}, execution_artifact={}\n",
        prepared.bundle_hash,
        prepared.signature_verified,
        prepared.cache_hit,
        prepared.execution_artifact_state
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
            "execution_artifact_state": prepared.execution_artifact_state,
            "fallback_reason": prepared.fallback_reason,
            "artifact_decode_ms": prepared.artifact_decode_elapsed.as_millis() as u64,
            "link_report": prepared.manifest.execution_artifact.as_ref().map(|artifact| &artifact.link_report),
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
