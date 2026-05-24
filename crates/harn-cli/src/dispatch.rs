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
//! A future ticket may replace the temp-file path with an in-memory
//! entry point if cold-start budgets demand it (harn#2300 / G7 AOT
//! bytecode embedding is the planned next step).
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

use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use crate::env_guard::ScopedEnvVar;

/// Env var ports read to decide whether to emit a JSON envelope vs.
/// human-readable output. Set to `"1"` for the script's lifetime when
/// the host (clap) saw `--json`; left untouched otherwise so a
/// user-provided value in the environment still wins.
pub const JSON_MODE_ENV: &str = "HARN_OUTPUT_JSON";

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

/// Capture-mode variant suitable for tests: returns the full
/// [`RunOutcome`] instead of writing to real stdio. Production code
/// should prefer [`dispatch_to_embedded_script`] which flushes for you.
pub async fn run_embedded_script(
    script_name: &str,
    argv: Vec<String>,
    json_mode: bool,
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

    // Set HARN_OUTPUT_JSON only when the host explicitly asked for JSON
    // mode. If json_mode=false we leave the env alone — a user shell
    // export still wins, matching the NO_COLOR convention. ScopedEnvVar
    // restores the prior value on drop so tests stay isolated.
    let _scope = json_mode.then(|| ScopedEnvVar::set(JSON_MODE_ENV, "1"));

    let outcome = execute_run(
        &path_str,
        false,
        HashSet::new(),
        argv,
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

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
    }
}
