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
#![deny(clippy::print_stdout)]

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use harn_parser::analysis::{AnalysisDatabase, SourceId};

use crate::{package, CLI_RUNTIME_STACK_SIZE};

use super::check_cmd::{
    check_file_report_inner, CheckDiagnostic, CheckFileReport, CheckFileStatus, CheckTextOutput,
};
use super::host_capabilities::{resolve_host_capabilities, ResolvedHostCapabilities};

/// Environment override for the check worker-pool size. `1` forces the
/// serial path; unset defaults to the machine's available parallelism.
pub(crate) const CHECK_JOBS_ENV: &str = "HARN_CHECK_JOBS";

/// CLI flags that override each file's `[check]` config from `harn.toml`.
#[derive(Debug, Clone, Default)]
pub(crate) struct CheckCliOverrides {
    pub host_capabilities: Option<String>,
    pub bundle_root: Option<String>,
    pub strict: bool,
    pub strict_types: bool,
    pub trusted_host_dispatch: bool,
    pub preflight: Option<String>,
    pub invariants: bool,
}

impl From<&crate::cli::CheckArgs> for CheckCliOverrides {
    fn from(args: &crate::cli::CheckArgs) -> Self {
        Self {
            host_capabilities: args.host_capabilities.clone(),
            bundle_root: args.bundle_root.clone(),
            strict: args.strict,
            strict_types: args.strict_types,
            trusted_host_dispatch: args.trusted_host_dispatch,
            preflight: args.preflight.clone(),
            invariants: args.invariants,
        }
    }
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

/// Check every file against the shared compact module graph, in parallel,
/// returning results in input order. Workers parse only their current file;
/// retaining every seed AST beside the graph makes peak memory scale with the
/// corpus before the bounded worker pool starts.
pub(crate) fn check_files(
    files: &[PathBuf],
    module_graph: &harn_modules::ModuleGraph,
    cross_file_imports: &HashSet<String>,
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> Vec<CheckedFile> {
    let workers = worker_count(files.len());
    // Resolve directory-stable check inputs before worker fan-out. In
    // particular, an external host-capability manifest is read and parsed once
    // per source directory instead of once per checked file.
    let config_by_dir = build_check_contexts(files, overrides);
    let mut checked = run_ordered_checks(
        files,
        workers,
        want_text,
        AnalysisDatabase::new,
        |analysis, file| {
            check_one(
                analysis,
                file,
                module_graph,
                &config_by_dir,
                cross_file_imports,
                overrides,
                want_text,
            )
        },
    );
    attach_host_reconciliation_diagnostics(files, &mut checked, &config_by_dir, want_text);
    checked
}

/// Check a source corpus with one-file module-resolution semantics while
/// retaining one process and the native bounded worker pool.
///
/// A shared graph is the correct default for a project: sibling imports and
/// cross-file lint exemptions must see each other. Fixture corpora are a
/// different ownership boundary. Their files are independent programs, and a
/// graph containing every fixture can make unrelated sources satisfy imports
/// or activate type-aware lints that a one-file check would not see. The old
/// workaround spawned `harn check` once per file. This path keeps the exact
/// semantics without the process and CLI-startup amplification.
pub(crate) fn check_files_independently(
    files: &[PathBuf],
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> Vec<CheckedFile> {
    let workers = worker_count(files.len());
    let config_by_dir = build_check_contexts(files, overrides);
    let mut checked = run_ordered_checks(
        files,
        workers,
        want_text,
        || (),
        |(), file| {
            let module_graph = super::build_module_graph(std::slice::from_ref(file));
            let cross_file_imports = super::collect_cross_file_imports(&module_graph);
            let mut analysis = AnalysisDatabase::new();
            check_one(
                &mut analysis,
                file,
                &module_graph,
                &config_by_dir,
                &cross_file_imports,
                overrides,
                want_text,
            )
        },
    );
    attach_host_reconciliation_diagnostics(files, &mut checked, &config_by_dir, want_text);
    checked
}

/// Report each missing host operation once per project config.
///
/// Attach the report to the first file so a workspace check does not repeat it
/// for every file.
fn attach_host_reconciliation_diagnostics(
    files: &[PathBuf],
    checked: &mut [CheckedFile],
    config_by_dir: &HashMap<PathBuf, EffectiveCheckConfig>,
    want_text: bool,
) {
    let mut reported = HashSet::new();
    for (index, file) in files.iter().enumerate() {
        let key = check_config_key(file);
        let Some(reconciliation) = config_by_dir
            .get(&key)
            .and_then(|context| context.host_capabilities.reconciliation.as_ref())
        else {
            continue;
        };
        let checked_file = &mut checked[index];
        if let Some(error) = reconciliation.error.as_ref() {
            let report_key = format!("{}:error:{error}", reconciliation.served_path);
            if reported.insert(report_key) {
                attach_host_reconciliation_diagnostic(
                    checked_file,
                    error,
                    "check the served operations file and each runtime-installed operation name",
                    want_text,
                );
            }
        }
        for missing in &reconciliation.missing_operations {
            let qualified = missing.qualified_name();
            if !reported.insert(format!(
                "{}:operation:{qualified}",
                reconciliation.served_path
            )) {
                continue;
            }
            attach_host_reconciliation_diagnostic(
                checked_file,
                &format!(
                    "declared host operation `{qualified}` is missing from `{}`",
                    reconciliation.served_path
                ),
                "add the operation to the host, remove the declaration, or list its exact name in `runtime_installed_host_operations`",
                want_text,
            );
        }
    }
}

fn attach_host_reconciliation_diagnostic(
    checked: &mut CheckedFile,
    message: &str,
    help: &str,
    want_text: bool,
) {
    let code = harn_parser::diagnostic_codes::Code::CapabilityOperationUnserved;
    checked.report.status = CheckFileStatus::Error;
    checked.report.diagnostics.push(CheckDiagnostic {
        source: "host-capabilities",
        severity: "error",
        code: Some(code.to_string()),
        message: message.to_string(),
        span: None,
        help: Some(help.to_string()),
    });
    if want_text {
        checked.text.rendered.push_str(&format!(
            "{}: error[{code}]: {message}\n  help: {help}\n",
            checked.report.path
        ));
    }
}

fn run_ordered_checks<State>(
    files: &[PathBuf],
    workers: usize,
    want_text: bool,
    init: impl Fn() -> State + Sync,
    check: impl Fn(&mut State, &PathBuf) -> CheckedFile + Sync,
) -> Vec<CheckedFile> {
    let next = AtomicUsize::new(0);
    let run_worker = || {
        let mut state = init();
        let mut produced = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(file) = files.get(index) else {
                break;
            };
            let checked = match catch_unwind(AssertUnwindSafe(|| check(&mut state, file))) {
                Ok(checked) => checked,
                Err(_) => {
                    // A checker panic is an internal failure of this file, not
                    // permission to drop the rest of a corpus. The unwind may
                    // have left per-worker analysis state partially mutated,
                    // so replace it before claiming another file.
                    state = init();
                    internal_failure(file, want_text)
                }
            };
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
            let handles: Vec<_> = (0..workers)
                .map(|index| {
                    std::thread::Builder::new()
                        .name(format!("harn-check-{index}"))
                        .stack_size(CLI_RUNTIME_STACK_SIZE)
                        .spawn_scoped(scope, run_worker)
                        .expect("failed to spawn harn check worker")
                })
                .collect();
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

fn internal_failure(file: &Path, want_text: bool) -> CheckedFile {
    let path = file.to_string_lossy().into_owned();
    let message = "internal `harn check` failure while analyzing this file";
    let text = if want_text {
        CheckTextOutput {
            rendered: format!("{path}: error: {message}\n"),
        }
    } else {
        CheckTextOutput::default()
    };
    CheckedFile {
        report: CheckFileReport {
            path,
            status: CheckFileStatus::Error,
            diagnostics: vec![CheckDiagnostic {
                source: "check",
                severity: "error",
                code: None,
                message: message.to_string(),
                span: None,
                help: Some(
                    "report this reproducible checker failure to the Harn maintainers".to_string(),
                ),
            }],
        },
        strict: false,
        text,
    }
}

fn check_one(
    analysis: &mut AnalysisDatabase,
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
    config_by_dir: &HashMap<PathBuf, EffectiveCheckConfig>,
    cross_file_imports: &HashSet<String>,
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> CheckedFile {
    let checked = check_one_retaining_analysis(
        analysis,
        file,
        module_graph,
        config_by_dir,
        cross_file_imports,
        overrides,
        want_text,
    );
    // A batch result owns its compact report. Keeping the analysis entry as
    // well retains source text, tokens, AST, and every typecheck projection
    // for the rest of this worker's shard, making peak RSS scale with corpus
    // size instead of bounded worker concurrency.
    analysis.remove_source(&SourceId::path(file));
    checked
}

fn check_one_retaining_analysis(
    analysis: &mut AnalysisDatabase,
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
    config_by_dir: &HashMap<PathBuf, EffectiveCheckConfig>,
    cross_file_imports: &HashSet<String>,
    overrides: &CheckCliOverrides,
    want_text: bool,
) -> CheckedFile {
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
        super::apply_harn_lint_config(file, &mut config);
        if let Some(path) = overrides.host_capabilities.as_ref() {
            config.host.host_capabilities_path = Some(path.clone());
        }
        if let Some(path) = overrides.bundle_root.as_ref() {
            config.bundle_root = Some(path.clone());
        }
        if overrides.strict {
            config.strict = true;
        }
        if overrides.strict_types {
            config.strict_types = true;
        }
        if overrides.trusted_host_dispatch {
            config.trusted_host_dispatch = true;
        }
        if let Some(severity) = overrides.preflight.as_deref() {
            config.preflight_severity = Some(severity.to_string());
        }
        let host_capabilities = resolve(&config);
        // Hand the project's declaration to the typechecker. Host operations
        // exist only at runtime, so without this the capability-method check
        // reports every one of them as undeclared.
        harn_parser::install_declared_host_operations(
            host_capabilities
                .capabilities
                .operation_pairs()
                .map(|(capability, operation)| (capability.to_string(), operation.to_string())),
        );
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
        assert!(first_context
            .host_capabilities
            .capabilities
            .contains_operation("custom", "inspect"));
        assert_eq!(
            first_context.host_capabilities.source_content.as_deref(),
            Some(manifest_content)
        );
    }

    #[test]
    fn check_context_includes_disabled_lint_rules() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.harn");
        std::fs::write(&file, "pipeline main() { assert(true) }\n").unwrap();
        std::fs::write(
            dir.path().join("harn.toml"),
            "[lint]\ndisabled = [\"assert-outside-test\"]\n",
        )
        .unwrap();

        let files = vec![file.clone()];
        let contexts = build_check_contexts_with(
            &files,
            &CheckCliOverrides::default(),
            resolve_host_capabilities,
        );

        assert_eq!(
            contexts[&check_config_key(&file)].config.disable_rules,
            ["assert-outside-test"]
        );
    }

    #[test]
    fn completed_batch_check_evicts_its_analysis_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.harn");
        std::fs::write(&file, "pipeline main() { return nil }\n").unwrap();
        let files = vec![file.clone()];
        let overrides = CheckCliOverrides::default();
        let module_graph = super::super::build_module_graph(&files);
        let cross_file_imports = super::super::collect_cross_file_imports(&module_graph);
        let contexts = build_check_contexts(&files, &overrides);
        let mut analysis = AnalysisDatabase::new();

        let checked = check_one(
            &mut analysis,
            &file,
            &module_graph,
            &contexts,
            &cross_file_imports,
            &overrides,
            false,
        );

        assert_eq!(checked.report.path, file.to_string_lossy());
        assert!(
            !analysis.remove_source(&SourceId::path(&file)),
            "the compact check report must be the only retained per-file projection"
        );
    }

    fn diagnostic_facts(checked: &CheckedFile) -> Vec<(&str, Option<&str>, &str)> {
        checked
            .report
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.severity,
                    diagnostic.code.as_deref(),
                    diagnostic.message.as_str(),
                )
            })
            .collect()
    }

    fn successful_file(file: &Path) -> CheckedFile {
        CheckedFile {
            report: CheckFileReport {
                path: file.to_string_lossy().into_owned(),
                status: CheckFileStatus::Ok,
                diagnostics: Vec::new(),
            },
            strict: false,
            text: CheckTextOutput::default(),
        }
    }

    #[test]
    fn shared_host_surface_reports_each_missing_operation_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join("harn.toml"),
            r#"
[check]
host_capabilities_path = "declared.json"
host_served_capabilities_path = "served.json"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("declared.json"),
            r#"{"workspace":["read_text"]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("served.json"), "{}").unwrap();
        let files = [
            dir.path().join("a/first.harn"),
            dir.path().join("b/second.harn"),
        ];
        for file in &files {
            std::fs::write(file, "pipeline main() {}\n").unwrap();
        }

        let contexts = build_check_contexts(&files, &CheckCliOverrides::default());
        let mut checked = files
            .iter()
            .map(|file| successful_file(file))
            .collect::<Vec<_>>();
        attach_host_reconciliation_diagnostics(&files, &mut checked, &contexts, false);

        assert_eq!(
            checked
                .iter()
                .flat_map(|file| &file.report.diagnostics)
                .filter(|diagnostic| diagnostic.code.as_deref() == Some("HARN-CAP-008"))
                .count(),
            1
        );
    }

    #[test]
    fn checker_panic_is_a_file_diagnostic_and_serial_checks_continue() {
        let files = ["before.harn", "panic.harn", "after.harn"]
            .map(PathBuf::from)
            .to_vec();
        let initializations = AtomicUsize::new(0);
        let checked = run_ordered_checks(
            &files,
            1,
            true,
            || initializations.fetch_add(1, Ordering::Relaxed) + 1,
            |generation, file| {
                assert_ne!(
                    file,
                    Path::new("panic.harn"),
                    "payload and location must not enter the diagnostic"
                );
                if file == Path::new("after.harn") {
                    assert_eq!(*generation, 2, "worker state must reset after unwind");
                }
                successful_file(file)
            },
        );

        assert_eq!(initializations.load(Ordering::Relaxed), 2);
        assert_eq!(
            checked
                .iter()
                .map(|file| file.report.path.as_str())
                .collect::<Vec<_>>(),
            ["before.harn", "panic.harn", "after.harn"]
        );
        assert_eq!(
            diagnostic_facts(&checked[1]),
            [(
                "error",
                None,
                "internal `harn check` failure while analyzing this file"
            )]
        );
        assert_eq!(
            checked[1].text.rendered,
            "panic.harn: error: internal `harn check` failure while analyzing this file\n"
        );
        assert!(matches!(checked[1].report.status, CheckFileStatus::Error));
    }

    #[test]
    fn parallel_checker_panic_preserves_every_ordered_result() {
        let files = ["zero.harn", "panic.harn", "two.harn", "three.harn"]
            .map(PathBuf::from)
            .to_vec();
        let visits = AtomicUsize::new(0);
        let checked = run_ordered_checks(
            &files,
            3,
            false,
            || (),
            |(), file| {
                visits.fetch_add(1, Ordering::Relaxed);
                assert_ne!(file, Path::new("panic.harn"), "adversarial worker panic");
                successful_file(file)
            },
        );

        assert_eq!(visits.load(Ordering::Relaxed), files.len());
        assert_eq!(
            checked
                .iter()
                .map(|file| file.report.path.as_str())
                .collect::<Vec<_>>(),
            ["zero.harn", "panic.harn", "two.harn", "three.harn"]
        );
        assert_eq!(diagnostic_facts(&checked[1]).len(), 1);
        assert!(checked[1].text.rendered.is_empty());
    }

    #[test]
    fn independent_checks_match_one_target_module_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library.harn");
        let consumer = dir.path().join("consumer.harn");
        std::fs::write(&library, "fn helper() { return 1 }\n").unwrap();
        std::fs::write(&consumer, "import { helper } from \"library\"\nhelper()\n").unwrap();
        let overrides = CheckCliOverrides::default();

        let single_graph = super::super::build_module_graph(std::slice::from_ref(&library));
        let single_imports = super::super::collect_cross_file_imports(&single_graph);
        let single = check_files(
            std::slice::from_ref(&library),
            &single_graph,
            &single_imports,
            &overrides,
            false,
        );
        let independent =
            check_files_independently(&[library.clone(), consumer.clone()], &overrides, false);
        assert_eq!(
            diagnostic_facts(&independent[0]),
            diagnostic_facts(&single[0])
        );
        assert!(diagnostic_facts(&single[0])
            .iter()
            .any(|(_, code, _)| *code == Some("HARN-LNT-019")));

        let files = [library, consumer];
        let shared_graph = super::super::build_module_graph(&files);
        let shared_imports = super::super::collect_cross_file_imports(&shared_graph);
        let shared = check_files(&files, &shared_graph, &shared_imports, &overrides, false);
        assert_ne!(diagnostic_facts(&shared[0]), diagnostic_facts(&single[0]));
    }
}
