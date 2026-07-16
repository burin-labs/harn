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
use super::host_capabilities::{resolve_host_capabilities, ResolvedHostCapabilities};

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

struct EffectiveCheckConfig {
    config: package::CheckConfig,
    host_capabilities: ResolvedHostCapabilities,
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
    // Resolve directory-stable check inputs before worker fan-out. In
    // particular, an external host-capability manifest is read and parsed once
    // per source directory instead of once per checked file.
    let config_by_dir = build_check_contexts(files, overrides);
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
    config_by_dir: &HashMap<PathBuf, EffectiveCheckConfig>,
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
    let context = config_by_dir
        .get(&check_config_key(file))
        .expect("every checked file has a precomputed check context");
    let config = &context.config;

    // Persistent result cache (#4391): key on the file's content + import
    // closure + check config + this file's cross-file lint exemptions, replay
    // on hit, and record the preflight's external filesystem probes on miss
    // so the artifact can be revalidated. Unreadable files skip the cache and
    // report their IO error through the normal path.
    let cache_key = super::result_cache::enabled()
        .then(|| std::fs::read_to_string(file).ok())
        .flatten()
        .map(|source| {
            let exemptions = lint_exemptions_for_file(file, module_graph, cross_file_imports);
            super::result_cache::result_cache_key(
                file,
                &file.to_string_lossy(),
                &source,
                config,
                context.host_capabilities.source_content.as_deref(),
                overrides.invariants,
                &exemptions,
            )
        });
    if let Some(key) = cache_key.as_ref() {
        if let Some(hit) =
            super::result_cache::load(key, &file.to_string_lossy(), config, want_text)
        {
            return hit;
        }
    }

    // Render text even in JSON mode when the result will be stored: cached
    // artifacts must replay under either output mode.
    let mut text = (want_text || cache_key.is_some()).then(CheckTextOutput::default);
    let (report, probes) = super::result_cache::with_probe_recording(cache_key.is_some(), || {
        check_file_report_inner(
            analysis,
            file,
            config,
            cross_file_imports,
            module_graph,
            &context.host_capabilities.capabilities,
            overrides.invariants,
            text.as_mut(),
        )
    });
    let checked = CheckedFile {
        report,
        strict: config.strict,
        text: text.unwrap_or_default(),
    };
    if let Some(key) = cache_key.as_ref() {
        super::result_cache::store(key, &checked, probes);
    }
    if want_text {
        checked
    } else {
        CheckedFile {
            text: CheckTextOutput::default(),
            ..checked
        }
    }
}

/// The subset of the run's cross-file selective-import names that could
/// affect this file's lint output: the linter only consults the set to
/// exempt names *declared in this file* from unused-function findings, so
/// only that intersection belongs in the file's cache key. An unrelated
/// import change elsewhere in the tree leaves the subset — and the cached
/// result — intact.
fn lint_exemptions_for_file(
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
    cross_file_imports: &HashSet<String>,
) -> Vec<String> {
    let Some(declared) = module_graph.declared_names_for_file(file) else {
        // Unknown to the graph: over-approximate with the full set (sorted
        // for stability) so the key stays conservative.
        let mut all: Vec<String> = cross_file_imports.iter().cloned().collect();
        all.sort_unstable();
        return all;
    };
    declared
        .into_iter()
        .filter(|name| cross_file_imports.contains(*name))
        .map(str::to_string)
        .collect()
}

/// Key directory-stable check inputs by a file's parent. `load_check_config`
/// walks up to 16 ancestor directories probing for `harn.toml`; sibling files
/// share both that config and its resolved host-capability manifest, so the
/// driver computes the effective context once per directory before fan-out.
fn check_config_key(file: &Path) -> PathBuf {
    file.parent().unwrap_or(file).to_path_buf()
}

fn build_check_contexts(
    files: &[PathBuf],
    overrides: &CheckCliOverrides,
) -> HashMap<PathBuf, EffectiveCheckConfig> {
    build_check_contexts_with(files, overrides, resolve_host_capabilities)
}

fn build_check_contexts_with(
    files: &[PathBuf],
    overrides: &CheckCliOverrides,
    resolve: impl Fn(&package::CheckConfig) -> ResolvedHostCapabilities,
) -> HashMap<PathBuf, EffectiveCheckConfig> {
    let mut contexts = HashMap::new();
    for file in files {
        let key = check_config_key(file);
        if contexts.contains_key(&key) {
            continue;
        }
        let mut config = package::load_check_config(Some(file));
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
        let host_capabilities = resolve(&config);
        contexts.insert(
            key,
            EffectiveCheckConfig {
                config,
                host_capabilities,
            },
        );
    }
    contexts
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn sibling_files_resolve_one_host_capability_context() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.harn");
        let second = dir.path().join("second.harn");
        std::fs::write(&first, "pipeline first() {}\n").unwrap();
        std::fs::write(&second, "pipeline second() {}\n").unwrap();
        let manifest = dir.path().join("host-capabilities.toml");
        let manifest_content = "[capabilities.custom]\noperations = [\"inspect\"]\n";
        std::fs::write(&manifest, manifest_content).unwrap();

        let resolutions = AtomicUsize::new(0);
        let overrides = CheckCliOverrides {
            host_capabilities: Some(manifest.display().to_string()),
            ..CheckCliOverrides::default()
        };
        let files = vec![first.clone(), second];
        let contexts = build_check_contexts_with(&files, &overrides, |config| {
            resolutions.fetch_add(1, Ordering::Relaxed);
            resolve_host_capabilities(config)
        });

        assert_eq!(resolutions.load(Ordering::Relaxed), 1);
        assert_eq!(contexts.len(), 1);
        let first_context = &contexts[&check_config_key(&first)];
        assert!(first_context.host_capabilities.capabilities["custom"].contains("inspect"));
        assert_eq!(
            first_context.host_capabilities.source_content.as_deref(),
            Some(manifest_content)
        );
    }
}
