//! Parallel per-file driver for `harn check`.
//!
//! Checking a tree is embarrassingly parallel once the module graph is
//! built: each file's typecheck/compile/lint/preflight pass only reads the
//! shared [`harn_modules::ModuleGraph`] and its own import closure. The
//! historical driver ran files strictly serially on one core, so whole-tree
//! checks (CI lint gates, editor save hooks over pipelines directories) paid
//! `sum(per-file cost)` wall clock. This driver fans files out over a worker
//! pool and replays buffered per-file output in input order, so the observable
//! stream stays byte-identical to the serial driver's ordering.
//!
//! Two deliberate exceptions to "identical":
//! - A lex/parse failure in one file no longer aborts the whole run before
//!   later files are checked (the serial text path used to `exit(1)` at the
//!   first such file). Every file is now always checked and rendered, and the
//!   process still exits non-zero. JSON mode always behaved this way.
//! - Diagnostics print when each file's slot drains rather than the instant
//!   they are found.
//!
//! Worker count comes from [`std::thread::available_parallelism`], capped by
//! the file count, with `HARN_CHECK_JOBS=<n>` as the explicit override
//! (`HARN_CHECK_JOBS=1` restores the fully serial driver for bisection).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};

use crate::package;

use super::check_cmd::{check_file_report_inner, CheckFileReport, CheckTextOutput};

/// Environment override for the check worker-pool size. `1` forces the
/// serial path; unset defaults to the machine's available parallelism.
pub(crate) const CHECK_JOBS_ENV: &str = "HARN_CHECK_JOBS";

/// CLI flags that override each file's `[check]` config from `harn.toml`.
#[derive(Debug, Clone, Default)]
pub(crate) struct CheckCliOverrides {
    pub host_capabilities: Option<String>,
    pub bundle_root: Option<String>,
    pub strict_types: bool,
    pub preflight: Option<String>,
    pub invariants: bool,
}

/// One file's finished check: the structured report, the strictness the
/// file's own config resolved to (drives `should_fail`), and the buffered
/// text output (empty when the caller asked for JSON).
pub(crate) struct CheckedFile {
    pub report: CheckFileReport,
    pub strict: bool,
    pub text: CheckTextOutput,
}

fn worker_count(files: usize) -> usize {
    let configured = std::env::var(CHECK_JOBS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&jobs| jobs > 0);
    let default = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    configured.unwrap_or(default).min(files.max(1))
}

/// Check every file against the shared module graph, in parallel, returning
/// results in input order. `parsed_sources` carries the ASTs the module-graph
/// build already produced for the seed files so workers skip re-parsing;
/// each entry is consumed by the first worker that checks that file.
pub(crate) fn check_files(
    files: &[PathBuf],
    module_graph: &harn_modules::ModuleGraph,
    parsed_sources: HashMap<PathBuf, harn_modules::ParsedModuleSource>,
    cross_file_imports: &HashSet<String>,
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> Vec<CheckedFile> {
    let workers = worker_count(files.len());
    let parsed_sources = Mutex::new(parsed_sources);
    let config_by_dir: Mutex<HashMap<PathBuf, package::CheckConfig>> = Mutex::new(HashMap::new());
    let next = AtomicUsize::new(0);

    let run_worker = || {
        let mut analysis = AnalysisDatabase::new();
        let mut produced: Vec<(usize, CheckedFile)> = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(file) = files.get(index) else {
                break;
            };
            let checked = check_one(
                &mut analysis,
                file,
                module_graph,
                &parsed_sources,
                &config_by_dir,
                cross_file_imports,
                overrides,
                want_text,
            );
            produced.push((index, checked));
        }
        produced
    };

    let mut merged: Vec<Option<CheckedFile>> = Vec::with_capacity(files.len());
    merged.resize_with(files.len(), || None);
    if workers <= 1 {
        for (index, checked) in run_worker() {
            merged[index] = Some(checked);
        }
    } else {
        let produced = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers).map(|_| scope.spawn(run_worker)).collect();
            handles
                .into_iter()
                .flat_map(|handle| match handle.join() {
                    Ok(produced) => produced,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect::<Vec<_>>()
        });
        for (index, checked) in produced {
            merged[index] = Some(checked);
        }
    }
    merged
        .into_iter()
        .map(|slot| slot.expect("every input file produces exactly one check result"))
        .collect()
}

fn check_one(
    analysis: &mut AnalysisDatabase,
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
    parsed_sources: &Mutex<HashMap<PathBuf, harn_modules::ParsedModuleSource>>,
    config_by_dir: &Mutex<HashMap<PathBuf, package::CheckConfig>>,
    cross_file_imports: &HashSet<String>,
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> CheckedFile {
    if let Some(parsed) = take_parsed_source(parsed_sources, file) {
        analysis.set_parsed_source(
            SourceId::path(file),
            parsed.source,
            SourceVersion(1),
            parsed.program,
        );
    }
    let mut config = load_check_config_cached(config_by_dir, file);
    if let Some(path) = overrides.host_capabilities.as_ref() {
        config.host_capabilities_path = Some(path.clone());
    }
    if let Some(path) = overrides.bundle_root.as_ref() {
        config.bundle_root = Some(path.clone());
    }
    if overrides.strict_types {
        config.strict_types = true;
    }
    if let Some(severity) = overrides.preflight.as_deref() {
        config.preflight_severity = Some(severity.to_string());
    }
    let mut text = want_text.then(CheckTextOutput::default);
    let report = check_file_report_inner(
        analysis,
        file,
        &config,
        cross_file_imports,
        module_graph,
        overrides.invariants,
        text.as_mut(),
    );
    CheckedFile {
        report,
        strict: config.strict,
        text: text.unwrap_or_default(),
    }
}

/// Load the `[check]` config for `file`, memoized per parent directory for
/// the run. `load_check_config` walks up to 16 ancestor directories probing
/// for `harn.toml` and re-parses the manifest on every call; sibling files
/// share the answer, so a whole-tree check needs it once per directory, not
/// once per file.
fn load_check_config_cached(
    config_by_dir: &Mutex<HashMap<PathBuf, package::CheckConfig>>,
    file: &Path,
) -> package::CheckConfig {
    let Some(dir) = file.parent() else {
        return package::load_check_config(Some(file));
    };
    if let Some(hit) = config_by_dir
        .lock()
        .expect("check config memo lock poisoned")
        .get(dir)
        .cloned()
    {
        return hit;
    }
    let config = package::load_check_config(Some(file));
    config_by_dir
        .lock()
        .expect("check config memo lock poisoned")
        .insert(dir.to_path_buf(), config.clone());
    config
}

/// Claim the module-graph build's parsed AST for `file`, if still unclaimed.
/// Keyed by canonical path exactly like the graph build's retention set; on
/// any canonicalization failure the worker just re-parses from disk.
fn take_parsed_source(
    parsed_sources: &Mutex<HashMap<PathBuf, harn_modules::ParsedModuleSource>>,
    file: &Path,
) -> Option<harn_modules::ParsedModuleSource> {
    let canonical = harn_modules::canonical_path(file);
    parsed_sources
        .lock()
        .expect("parsed-source map lock poisoned")
        .remove(&canonical)
}
