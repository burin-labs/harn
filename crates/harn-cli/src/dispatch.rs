//! Subcommand dispatch wedge: routes named subcommands to embedded
//! `.harn` scripts so CLI surfaces can be implemented in Harn itself
//! instead of Rust.
//!
//! Motivation lives in harn#2293 (epic) and harn#2294 (G1). Scripts are
//! defined in `crates/harn-stdlib/src/stdlib/cli/<name>.harn` and
//! registered in [`harn_stdlib::STDLIB_CLI_SCRIPTS`]. Each dispatched
//! script receives:
//!
//!   * `argv: list<string>` — the per-subcommand argv after top-level
//!     clap parsing. Same global `harn run -- a b c` exposes.
//!   * `HARN_OUTPUT_JSON=1` (env var) when the caller asked for JSON
//!     output. Scripts read it via `harness.env.get_or("HARN_OUTPUT_JSON", "0")`
//!     and switch between human-readable and JSON-envelope rendering
//!     without re-parsing `--json` themselves.
//!
//! Stdout / stderr / exit code propagate through the existing
//! `execute_run` codepath so the wedge inherits bytecode cache, source
//! dir handling, harness install, skill loader, and store/metadata/
//! checkpoint builtins for free.
//!
//! ## AOT fast path (G7 / harn#2300)
//!
//! [`crate::cli_bytecode`] embeds every embedded script from the optional
//! release/package AOT payload as a `.harnbc` artifact (the same on-disk
//! format the runtime bytecode cache writes). When AOT is enabled (the default; opt out
//! with [`DISABLE_AOT_ENV`]) the wedge writes that artifact adjacent
//! to its source tempfile before handing off to `execute_run`. The
//! runtime's existing `adjacent_cache_path` check picks the artifact
//! up and skips parse + typecheck + compile entirely. On any mismatch
//! (e.g. the user flipped `HARN_DISABLE_OPTIMIZATIONS` between build
//! and run) the cache header rejects the artifact and the loader
//! transparently falls back to source compilation — no crash, no
//! special handling at the dispatch layer.
//!
//! ## Example shim
//!
//! ```ignore
//! pub async fn run(args: ExplainArgs) -> i32 {
//!     let mut argv = Vec::new();
//!     if let Some(target) = args.target { argv.push(target); }
//!     if args.catalog { argv.push("--catalog".into()); }
//!     // ... fold the rest of the parsed flags into argv ...
//!     crate::dispatch::dispatch_to_embedded_script("explain", argv, args.json).await
//! }
//! ```

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use crate::cli_bytecode::find_cli_script_bytecode;
use crate::commands::run::{
    execute_run, execute_run_with_sandbox_options, CliLlmMockMode, RunOutcome, RunProfileOptions,
    RunSandboxOptions,
};
use crate::env_guard::ScopedEnvVar;

/// Env var ports read to decide whether to emit a JSON envelope vs.
/// human-readable output. Set to `"1"` for the script's lifetime when
/// the host (clap) saw `--json`; left untouched otherwise so a
/// user-provided value in the environment still wins.
pub const JSON_MODE_ENV: &str = "HARN_OUTPUT_JSON";

/// Opt-out for the AOT bytecode fast path added in G7 (harn#2300).
/// When set to any truthy value (anything but unset, `0`, `false`,
/// `no`, or `off`), dispatch never drops the embedded bytecode
/// artifact adjacent to the tempfile and the runtime always parses,
/// type-checks, and compiles the source. Useful for debugging a
/// discrepancy between AOT and from-source behavior.
pub const DISABLE_AOT_ENV: &str = "HARN_DISABLE_AOT_CLI";

/// Shared with `bytecode_cache`. When set to any value, dispatch logs
/// a single-line `eprintln!` whenever it dropped an embedded bytecode
/// artifact but observably failed to use the fast path (write error,
/// missing-OUT_DIR build, etc.). Off by default to keep happy-path
/// stderr clean.
pub const CACHE_DEBUG_ENV: &str = "HARN_BYTECODE_CACHE_DEBUG";

/// Exit code returned when the named script can't be found in
/// [`harn_stdlib::STDLIB_CLI_SCRIPTS`]. Matches `EX_SOFTWARE` from
/// `sysexits.h` — an "internal software error" the user can't fix
/// without a new release.
const EX_SOFTWARE: i32 = 70;

/// Dispatch a CLI subcommand to its embedded `.harn` script and forward
/// stdout/stderr to the real terminal. Returns the exit code the caller
/// should hand to `process::exit`. Output is written to stderr first,
/// then stdout, before this returns — matching what `harn run` does.
pub async fn dispatch_to_embedded_script(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
) -> i32 {
    let outcome = run_embedded_script(script_name, argv, json_mode).await;
    flush_outcome(&outcome);
    outcome.exit_code
}

/// `dispatch_to_embedded_script` with the workspace-rooted sandbox
/// disabled. Used by ports whose user-supplied file paths intentionally
/// fall outside the workspace (e.g. `harn precompile <any-file.harn>`).
/// Without this, the script's temp-file location becomes the sandbox
/// root and `harness.fs.*` calls on the user's input path are denied.
///
/// Network egress, process spawning, and the rest of the host policy
/// are unaffected — this only loosens the FS access check.
pub async fn dispatch_to_embedded_script_no_sandbox(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
) -> i32 {
    let mut outcome = run_embedded_script_with_sandbox(
        script_name,
        argv,
        json_mode,
        RunSandboxOptions::disabled(),
    )
    .await;
    // The shared `execute_run` path prefixes a `warning: harn run
    // --no-sandbox ...` banner whenever the sandbox is disabled. That
    // banner is meant for direct `harn run --no-sandbox` invocations
    // where the user explicitly opted out; here the sandbox is off by
    // dispatch-wedge construction, not user choice, so the banner is
    // noise — strip it before the outcome reaches the terminal.
    outcome.stderr = strip_sandbox_warning(&outcome.stderr);
    flush_outcome(&outcome);
    outcome.exit_code
}

const SANDBOX_WARNING: &str =
    "warning: harn run --no-sandbox disables filesystem, process, and egress sandbox defaults\n";

fn strip_sandbox_warning(stderr: &str) -> String {
    if let Some(rest) = stderr.strip_prefix(SANDBOX_WARNING) {
        rest.to_string()
    } else {
        stderr.to_string()
    }
}

/// Capture-mode variant suitable for tests: returns the full
/// [`RunOutcome`] instead of writing to real stdio. Production code
/// should prefer [`dispatch_to_embedded_script`] which flushes for you.
pub async fn run_embedded_script(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
) -> RunOutcome {
    run_embedded_script_inner(script_name, argv, json_mode, None).await
}

/// Capture-mode variant that runs the script with the given sandbox
/// options instead of the default workspace-rooted sandbox. Use this
/// when the script's job is to operate on user-supplied paths outside
/// the workspace (see [`dispatch_to_embedded_script_no_sandbox`]).
pub async fn run_embedded_script_with_sandbox(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
    sandbox: RunSandboxOptions,
) -> RunOutcome {
    run_embedded_script_inner(script_name, argv, json_mode, Some(sandbox)).await
}

async fn run_embedded_script_inner(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
    sandbox: Option<RunSandboxOptions>,
) -> RunOutcome {
    let Some(source) = harn_stdlib::find_cli_script(script_name) else {
        return RunOutcome {
            stdout: String::new(),
            stderr: format!(
                "internal error: CLI dispatch target '{script_name}' is not embedded.\n\
                 This is a harn-cli build bug — please file an issue at \
                 https://github.com/burin-labs/harn/issues.\n"
            ),
            exit_code: EX_SOFTWARE,
        };
    };

    let temp = match write_script_to_tempfile(script_name, source) {
        Ok(t) => t,
        Err(error) => {
            return RunOutcome {
                stdout: String::new(),
                stderr: format!(
                    "internal error: failed to materialize embedded CLI script \
                     '{script_name}': {error}\n"
                ),
                exit_code: EX_SOFTWARE,
            };
        }
    };
    let path_str = temp.path().to_string_lossy().into_owned();

    // AOT fast path (G7 / harn#2300): if a precompiled `.harnbc`
    // artifact was emitted at build time for this script, drop it next
    // to the source tempfile so the existing runtime loader picks it
    // up via its adjacent-cache check. On any failure (write error,
    // header mismatch at load time, AOT not built into this binary)
    // the loader transparently falls back to source compilation.
    let _adjacent = maybe_drop_adjacent_bytecode(script_name, temp.path());

    // Set HARN_OUTPUT_JSON only when the host explicitly asked for JSON
    // mode. If json_mode=false we leave the env alone — a user shell
    // export still wins, matching the NO_COLOR convention. ScopedEnvVar
    // restores the prior value on drop so tests stay isolated.
    let _scope = json_mode.then(|| ScopedEnvVar::set(JSON_MODE_ENV, "1"));

    let outcome = match sandbox {
        Some(sandbox) => {
            execute_run_with_sandbox_options(
                &path_str,
                false,
                HashSet::new(),
                argv,
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions::default(),
                sandbox,
            )
            .await
        }
        None => {
            execute_run(
                &path_str,
                false,
                HashSet::new(),
                argv,
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions::default(),
            )
            .await
        }
    };

    drop(temp);
    outcome
}

fn flush_outcome(outcome: &RunOutcome) {
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
}

/// RAII guard that removes a dropped-adjacent bytecode file when the
/// dispatch wedge finishes — without this the temp directory would
/// leak a `.harnbc` per invocation. `Drop` is a best-effort cleanup;
/// any I/O error is swallowed because the tempfile cleanup already
/// covers the common-case "the OS will reap /tmp" path.
struct AdjacentBytecodeGuard {
    path: std::path::PathBuf,
}

impl Drop for AdjacentBytecodeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// True when the AOT fast path is enabled for this invocation. Reads
/// `HARN_DISABLE_AOT_CLI` with the same parsing rule as
/// [`harn_vm::bytecode_cache::cache_enabled`] so users can flip both
/// switches with the same conventions.
fn aot_enabled() -> bool {
    match std::env::var(DISABLE_AOT_ENV).ok().as_deref() {
        Some(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

fn cache_debug_enabled() -> bool {
    std::env::var_os(CACHE_DEBUG_ENV).is_some()
}

/// Materialize the embedded `.harnbc` for `script_name` next to the
/// dispatch tempfile. Returns the RAII guard that cleans the file up
/// on drop, or `None` when AOT is disabled, no artifact is registered,
/// or the adjacent path can't be computed.
fn maybe_drop_adjacent_bytecode(
    script_name: &str,
    source_tempfile_path: &Path,
) -> Option<AdjacentBytecodeGuard> {
    if !aot_enabled() {
        return None;
    }
    let bytes = find_cli_script_bytecode(script_name)?;
    let adjacent = harn_vm::bytecode_cache::adjacent_cache_path(source_tempfile_path)?;
    match std::fs::write(&adjacent, bytes) {
        Ok(()) => Some(AdjacentBytecodeGuard { path: adjacent }),
        Err(err) => {
            if cache_debug_enabled() {
                eprintln!(
                    "[harn] AOT bytecode drop failed for `{script_name}` at {}: {err}",
                    adjacent.display()
                );
            }
            None
        }
    }
}

fn write_script_to_tempfile(name: &str, source: &str) -> std::io::Result<tempfile::NamedTempFile> {
    // Nested script names like `eval/prompt` collapse to `eval-prompt`
    // so the temp file stays a single path segment without falling out
    // of the OS temp dir.
    let safe_name = name.replace('/', "-");
    let mut file = tempfile::Builder::new()
        .prefix(&format!("harn-cli-{safe_name}-"))
        .suffix(".harn")
        .tempfile()?;
    file.write_all(source.as_bytes())?;
    file.flush()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_script_returns_software_error() {
        let outcome = run_embedded_script("definitely/not/a/real/script", vec![], false).await;
        assert_eq!(outcome.exit_code, EX_SOFTWARE);
        assert!(
            outcome.stderr.contains("not embedded"),
            "stderr should explain the dispatch miss; got: {}",
            outcome.stderr
        );
        assert!(outcome.stdout.is_empty());
    }

    #[tokio::test]
    async fn echo_round_trips_argv_as_json_array() {
        let outcome = run_embedded_script("echo", vec!["foo".into(), "bar".into()], false).await;
        assert_eq!(
            outcome.exit_code, 0,
            "echo failed: stderr={}",
            outcome.stderr
        );
        assert_eq!(outcome.stdout, "[\"foo\",\"bar\"]\n");
        assert!(outcome.stderr.is_empty(), "stderr was: {}", outcome.stderr);
    }

    #[tokio::test]
    async fn echo_handles_empty_argv() {
        let outcome = run_embedded_script("echo", vec![], false).await;
        assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
        assert_eq!(outcome.stdout, "[]\n");
        assert!(outcome.stderr.is_empty(), "stderr was: {}", outcome.stderr);
    }

    /// A run that departs from nothing announces nothing.
    ///
    /// Several launch-time surfaces write a one-line advisory to stderr — the
    /// sandbox grant disclosure in `commands::run::sandbox`, the session
    /// environment-policy disclosure in `commands::run::environment` — and more
    /// will follow. They share one rule: name a departure from what the caller
    /// already had, and stay silent on the default. `sandbox_grant_disclosure`
    /// encodes it directly by returning `None` for an unmodified default run.
    ///
    /// The rule is load-bearing. `--summary-fd` and `--rusage-fd` exist so a
    /// machine-readable run puts its JSON on a dedicated descriptor and leaves
    /// stderr free for the caller's own diagnostics; `harn_cli_e2e`'s
    /// `run_json_cli` asserts that directly, and it dates to those features
    /// themselves (#2372, #2401). An advisory printed on every invocation
    /// breaks it for every consumer in order to report the status quo. It also
    /// blunts the disclosures that matter, since a line that fires on 100% of
    /// runs is one operators learn to skip.
    ///
    /// This guard is deliberately here rather than in `harn_cli_e2e`. The `ci`
    /// nextest profile filters every harn-cli integration binary out of the
    /// required lane except `harn_cli_fast`, and the e2e tier otherwise runs
    /// only nightly or behind the `e2e` label — so a regression there passes
    /// every required check and ships. #5602 did exactly that. This test runs
    /// in the harn-cli lib suite, which the required lane does execute, and it
    /// goes through the real launch path, so it sees any emitter on it.
    ///
    /// Assert on the whole stream rather than the absence of one known string:
    /// the point is to catch the next emitter, whose wording nobody has
    /// thought of yet.
    #[tokio::test]
    async fn a_default_run_writes_nothing_to_stderr() {
        let outcome = run_embedded_script("echo", vec!["quiet".into()], false).await;

        assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
        assert_eq!(
            outcome.stderr, "",
            "a default run must leave stderr clean, so `--summary-fd` / \
             `--rusage-fd` consumers receive only their own diagnostics. A new \
             launch-time advisory has to stay silent when the run does not \
             depart from the caller's defaults."
        );
    }
}
