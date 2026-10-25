//! Compile adjacent bytecode and imported interfaces for `harn precompile`.
//!
//! The embedded driver owns the directory walk, placement, and rendering. One
//! inner batch child compiles every source and emits or reuses all artifacts.
use std::path::Path;

use harn_package::precompile::{precompile_source, Outcome, Precompiled};
use harn_parser::DiagnosticSeverity;

use crate::cli::{PrecompileArgs, PRECOMPILE_JOBS_ENV};
use crate::command_error;
use crate::commands::collect_harn_files;
use crate::compiler_context::{ensure_builtin_signatures_installed, SourceCompilerAuthority};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::parse_source_file;
use crate::typecheck_imports::checker_with_resolved_graph;

mod batch;
mod warm_cache;

/// Per-source result consumed by the embedded directory driver. Successful
/// children distinguish compiled output from reused output on this stream.
pub const PRECOMPILE_OUTCOME_PREFIX: &str = "precompile-outcome:";

/// The embedded driver invokes this exact executable for each source.
pub const PRECOMPILE_BIN_ENV: &str = "HARN_CLI_SELF_EXE";
pub const PRECOMPILE_ARTIFACT_CONTRACT: &str = "harn.precompile.artifacts/relocatable-v1";

/// Output directory forwarded to each child; cleared after dispatch.
const PRECOMPILE_OUT_ENV: &str = "HARN_PRECOMPILE_OUT";
const PRECOMPILE_KEEP_GOING_ENV: &str = "HARN_PRECOMPILE_KEEP_GOING";
const PRECOMPILE_QUIET_ENV: &str = "HARN_PRECOMPILE_QUIET";
/// `1` compiles the target here; `batch` compiles every source its manifest names.
pub const PRECOMPILE_INNER_ENV: &str = "HARN_PRECOMPILE_INNER";

pub async fn run(args: PrecompileArgs) {
    if args.artifact_contract {
        println!("{PRECOMPILE_ARTIFACT_CONTRACT}");
        return;
    }
    if args.warm_cache {
        warm_cache::run(&args);
        return;
    }
    match std::env::var(PRECOMPILE_INNER_ENV).as_deref() {
        Ok("1") => return run_inner_compile(args),
        Ok("batch") => return batch::run_args(&args),
        _ => {}
    }

    let exe = std::env::current_exe().unwrap_or_else(|error| {
        command_error(&format!("failed to resolve current executable: {error}"))
    });
    let exe_str = exe.to_string_lossy().into_owned();
    let _bin = ScopedEnvVar::set(PRECOMPILE_BIN_ENV, &exe_str);
    let _out = args
        .out
        .as_ref()
        .map(|p| ScopedEnvVar::set(PRECOMPILE_OUT_ENV, &p.to_string_lossy()));
    let _keep = args
        .keep_going
        .then(|| ScopedEnvVar::set(PRECOMPILE_KEEP_GOING_ENV, "1"));
    let _quiet = args
        .quiet
        .then(|| ScopedEnvVar::set(PRECOMPILE_QUIET_ENV, "1"));
    let _jobs = ScopedEnvVar::set(PRECOMPILE_JOBS_ENV, &args.resolved_jobs().to_string());
    let batch_dir = batch::BatchDir::create();

    let target = args
        .target
        .as_ref()
        .unwrap_or_else(|| command_error("precompile target is required"));
    let argv = vec![target.to_string_lossy().into_owned()];
    // Use the no-sandbox dispatch: precompile's target is whatever path
    // the user passed, which is typically outside the script's
    // tempfile-derived workspace root. The actual compile work still
    // runs inside the spawned child's default sandbox; the orchestration
    // layer this script implements just needs to read directory entries.
    let exit = dispatch::dispatch_to_embedded_script_no_sandbox(
        "precompile",
        argv,
        /* json_mode */ false,
    )
    .await;
    drop(batch_dir);
    if exit != 0 {
        std::process::exit(exit);
    }
}

/// Outcome aggregated across all sources walked in one invocation.
#[derive(Default)]
struct Stats {
    compiled: usize,
    reused: usize,
    failed: usize,
}

/// Rust compiler entrypoint used by the `.harn` directory-walk driver for
/// each source file.
pub fn run_inner_compile(args: PrecompileArgs) {
    let target = args
        .target
        .clone()
        .unwrap_or_else(|| command_error("precompile target is required"));
    if !target.exists() {
        command_error(&format!("target does not exist: {}", target.display()));
    }

    let (sources, mut source_root) = if target.is_dir() {
        let mut files = Vec::new();
        collect_harn_files(&target, &mut files);
        files.sort();
        files.dedup();
        let root = target.canonicalize().unwrap_or_else(|_| target.clone());
        (files, Some(root))
    } else {
        (vec![target.clone()], None)
    };
    if source_root.is_none() && args.relocatable {
        source_root = target.parent().map(Path::to_path_buf);
    }

    if sources.is_empty() {
        command_error(&format!("no .harn files found under {}", target.display()));
    }

    let mut stats = Stats::default();
    for source in &sources {
        let result = precompile_source(source, source_root.as_deref(), args.out.as_deref());
        match result {
            Ok(Precompiled { outcome, warnings }) => {
                if !warnings.is_empty() {
                    eprint!("{warnings}");
                }
                let label = match outcome {
                    Outcome::Compiled(_) => {
                        stats.compiled += 1;
                        "compiled"
                    }
                    Outcome::Reused(_) => {
                        stats.reused += 1;
                        "reused"
                    }
                };
                println!("{PRECOMPILE_OUTCOME_PREFIX} {label}");
                if !args.quiet {
                    println!(
                        "{} -> {}",
                        source.display(),
                        outcome.destination().display()
                    );
                }
            }
            Err(err) => {
                stats.failed += 1;
                eprintln!("{}: {err}", source.display());
                if !args.keep_going {
                    break;
                }
            }
        }
    }

    if !args.quiet {
        eprintln!(
            "precompile: {} compiled, {} reused, {} failed",
            stats.compiled, stats.reused, stats.failed
        );
    }
    if stats.failed > 0 {
        std::process::exit(1);
    }
}
