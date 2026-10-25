//! One child compiles the directory driver's whole source list.
//!
//! A child per source paid process startup, stdlib preparation, and every
//! shared import's parse again for each file. One child on a worker pool
//! shares all of that, and reports per source exactly what that source's own
//! child would have, so the driver's rendering and exit status are unchanged.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use harn_package::precompile::{precompile_source, Outcome, Precompiled};
use serde::{Deserialize, Serialize};

use super::PRECOMPILE_OUTCOME_PREFIX;
use crate::cli::PrecompileArgs;
use crate::command_error;
use crate::env_guard::ScopedEnvVar;

/// Prefix of each per-source line the batch child prints for the driver.
const PRECOMPILE_RECORD_PREFIX: &str = "precompile-record:";

/// Directory the dispatch shim owns, where the driver writes the manifest.
const PRECOMPILE_BATCH_DIR_ENV: &str = "HARN_PRECOMPILE_BATCH_DIR";

/// The manifest directory, exported to the driver while this value lives and
/// removed when it drops.
pub(super) struct BatchDir {
    _env: ScopedEnvVar,
    _dir: tempfile::TempDir,
}

impl BatchDir {
    pub(super) fn create() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("harn-precompile-")
            .tempdir()
            .unwrap_or_else(|error| {
                command_error(&format!("create precompile batch dir: {error}"))
            });
        let env = ScopedEnvVar::set(PRECOMPILE_BATCH_DIR_ENV, &dir.path().to_string_lossy());
        Self {
            _env: env,
            _dir: dir,
        }
    }
}

/// Compile every source named by the manifest at `args.target`.
pub(super) fn run_args(args: &PrecompileArgs) {
    let manifest = args
        .target
        .as_deref()
        .unwrap_or_else(|| command_error("precompile manifest is required"));
    run(manifest, args.resolved_jobs(), args.keep_going);
}

#[derive(Deserialize)]
struct Manifest {
    /// Set for a directory walk: each source's key is relative to its directory.
    relocatable: bool,
    sources: Vec<ManifestSource>,
}

#[derive(Deserialize)]
struct ManifestSource {
    source: PathBuf,
    /// Directory the source's artifacts land in; empty writes them beside it.
    out: String,
}

/// The exit code and streams of one source's single-file compile.
#[derive(Serialize)]
struct Record<'a> {
    source: &'a Path,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run(manifest_path: &Path, jobs: usize, keep_going: bool) {
    let text = std::fs::read_to_string(manifest_path).unwrap_or_else(|error| {
        command_error(&format!(
            "read precompile manifest {}: {error}",
            manifest_path.display()
        ))
    });
    let manifest: Manifest = serde_json::from_str(&text).unwrap_or_else(|error| {
        command_error(&format!(
            "parse precompile manifest {}: {error}",
            manifest_path.display()
        ))
    });
    let results = compile_in_order(&manifest.sources, jobs, keep_going, |item| {
        let source_root = if manifest.relocatable {
            item.source.parent()
        } else {
            None
        };
        let out = (!item.out.is_empty()).then(|| Path::new(&item.out));
        precompile_source(&item.source, source_root, out)
    });
    let mut stdout = std::io::stdout().lock();
    for (item, result) in manifest.sources.iter().zip(results) {
        let Some(result) = result else { break };
        let failed = result.is_err();
        let record = serde_json::to_string(&record(&item.source, result))
            .expect("a precompile record serializes");
        writeln!(stdout, "{PRECOMPILE_RECORD_PREFIX} {record}")
            .unwrap_or_else(|error| command_error(&format!("write precompile record: {error}")));
        if failed && !keep_going {
            break;
        }
    }
}

/// What `harn precompile --quiet <source>` prints and exits with.
fn record(source: &Path, result: Result<Precompiled, String>) -> Record<'_> {
    match result {
        Ok(Precompiled { outcome, warnings }) => {
            let label = match outcome {
                Outcome::Compiled(_) => "compiled",
                Outcome::Reused(_) => "reused",
            };
            Record {
                source,
                exit_code: 0,
                stdout: format!("{PRECOMPILE_OUTCOME_PREFIX} {label}\n"),
                stderr: warnings,
            }
        }
        Err(error) => Record {
            source,
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("{}: {error}\n", source.display()),
        },
    }
}

/// Run `compile` over `items` on up to `jobs` workers, index-aligned with
/// `items`. Workers take items in order, so without `keep_going` every item
/// before a failure has started; `None` marks one that never started.
fn compile_in_order<T: Sync>(
    items: &[T],
    jobs: usize,
    keep_going: bool,
    compile: impl Fn(&T) -> Result<Precompiled, String> + Sync,
) -> Vec<Option<Result<Precompiled, String>>> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let work = || {
        let mut done = Vec::new();
        while keep_going || !failed.load(Ordering::Acquire) {
            let index = next.fetch_add(1, Ordering::AcqRel);
            let Some(item) = items.get(index) else {
                break;
            };
            let result = compile(item);
            if result.is_err() {
                failed.store(true, Ordering::Release);
            }
            done.push((index, result));
        }
        done
    };
    let workers = jobs.clamp(1, items.len().max(1));
    let finished = if workers == 1 {
        work()
    } else {
        harn_parser::runtime_stack::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope
                        .spawn_named("harn-precompile", work)
                        .unwrap_or_else(|error| {
                            command_error(&format!("spawn precompile worker: {error}"))
                        })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| match handle.join() {
                    Ok(done) => done,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        })
    };
    let mut results: Vec<_> = items.iter().map(|_| None).collect();
    for (index, result) in finished {
        results[index] = Some(result);
    }
    results
}
