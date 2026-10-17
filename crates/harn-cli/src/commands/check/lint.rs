//! `harn lint` over `.harn` sources.
//!
//! Every line this module prints is human-readable, so it all goes to stderr;
//! see [`super::outcome::print_lint_diagnostics`] for why.
#![deny(clippy::print_stdout)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use harn_parser::analysis::AnalysisDatabase;

use crate::package::CheckConfig;

use super::analysis::{analyze_file, render_file_analysis_error_or_exit};
use super::outcome::{
    print_lint_diagnostics, render_lint_outcome, write_applied_fixes, CommandOutcome,
};

use harn_lint::is_generated_path;

/// Collect the TOML sources of `language = "harn"` rules from the project's
/// `[rules] ruleDirs` (#2849), to run as lint rules. Non-harn rules can't
/// match `.harn` source and are skipped. Returns empty when no manifest or no
/// `ruleDirs` is declared (the common case — near-zero cost).
///
/// Loaded per file for simplicity; the dirs are small and the common path is a
/// single manifest lookup. Hoisting to once-per-run is a future optimization.
pub(crate) fn project_engine_rule_sources(path: &Path) -> Vec<String> {
    let Some((manifest, dir)) = crate::package::nearest_manifest_or_warn(path) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    for rel in &manifest.rules.rule_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join(rel)) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        files.sort();
        for file in files {
            if let Ok(src) = std::fs::read_to_string(&file) {
                if rule_targets_harn(&src) {
                    sources.push(src);
                }
            }
        }
    }
    sources
}

/// Collect native lint-rule dynamic libraries from the nearest manifest's
/// `[rules] nativeRuleDirs`. These paths are trusted by configuration: Harn
/// never searches ambient directories or environment variables for native code.
pub(crate) fn project_native_rule_paths(path: &Path) -> Vec<PathBuf> {
    let Some((manifest, dir)) = crate::package::nearest_manifest_or_warn(path) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for rel in &manifest.rules.native_rule_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join(rel)) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()) == Some(std::env::consts::DLL_EXTENSION)
            })
            .collect();
        files.sort();
        paths.extend(files);
    }
    paths
}

/// True when a rule TOML declares `language = "harn"`.
fn rule_targets_harn(src: &str) -> bool {
    toml::from_str::<toml::Value>(src)
        .ok()
        .as_ref()
        .and_then(|v| v.get("language"))
        .and_then(|l| l.as_str())
        == Some("harn")
}

/// Lint one file and print its findings, exiting on a lex or parse error.
#[cfg(test)]
pub(crate) fn lint_file_inner(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    lint_config: &super::config::HarnLintConfig,
    script_rule_diagnostics: &[harn_lint::LintDiagnostic],
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let output = analyze_file(analysis, path, config, module_graph)
        .unwrap_or_else(|error| render_file_analysis_error_or_exit(&path_str, error));
    let (outcome, rendered) = lint_analyzed_file(
        path,
        &output,
        config,
        externally_imported_names,
        module_graph,
        lint_config,
        script_rule_diagnostics,
    );
    eprint!("{rendered}");
    outcome
}

/// One file's lint result with its rendered text, replayed in input order by
/// [`lint_files`].
pub(crate) struct LintedFile {
    pub(crate) outcome: CommandOutcome,
    pub(crate) rendered: String,
    pub(crate) strict: bool,
}

/// Lint every file on a worker pool, as `harn check` does, and return the
/// results in input order. Each worker seeds its analysis database with the
/// file's parse from the module-graph build, so a file is lexed, parsed, and
/// type checked once, and the linter reads that type check's facts.
pub(crate) fn lint_files(
    files: &[PathBuf],
    module_graph: &harn_modules::ModuleGraph,
    parsed_targets: &super::config::ParsedTargets,
    externally_imported_names: &HashSet<String>,
    script_rule_diagnostics: &std::collections::HashMap<PathBuf, Vec<harn_lint::LintDiagnostic>>,
    require_file_header: bool,
    trusted_host_dispatch: bool,
) -> Vec<LintedFile> {
    super::driver::run_ordered(
        files,
        super::driver::worker_count(files.len()),
        AnalysisDatabase::new,
        |analysis, file| {
            let mut config = crate::package::load_check_config(Some(file));
            let mut lint_config = super::config::load_harn_lint_config(file);
            lint_config.require_file_header |= require_file_header;
            super::config::apply_loaded_harn_lint_config(&lint_config, &mut config);
            config.trusted_host_dispatch |= trusted_host_dispatch;
            parsed_targets.seed(analysis, file);
            let path_str = file.to_string_lossy().into_owned();
            let output = analyze_file(analysis, file, &config, module_graph);
            // Release the file's AST and facts once it is linted, so a
            // worker's memory stays bounded by one file, not its shard.
            analysis.remove_source(&harn_parser::analysis::SourceId::path(file));
            let (outcome, rendered) = match output {
                Ok(output) => lint_analyzed_file(
                    file,
                    &output,
                    &config,
                    externally_imported_names,
                    module_graph,
                    &lint_config,
                    script_rule_diagnostics
                        .get(file)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ),
                Err(error) => (
                    CommandOutcome {
                        has_error: true,
                        ..CommandOutcome::default()
                    },
                    super::analysis::render_file_analysis_error_to_string(&path_str, &error),
                ),
            };
            LintedFile {
                outcome,
                rendered,
                strict: config.strict,
            }
        },
        |file| LintedFile {
            outcome: CommandOutcome {
                has_error: true,
                ..CommandOutcome::default()
            },
            rendered: format!(
                "{}: error: internal `harn lint` failure while analyzing this file\n",
                file.display()
            ),
            strict: false,
        },
    )
}

fn lint_analyzed_file(
    path: &Path,
    output: &harn_parser::analysis::TypeCheckOutput,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    lint_config: &super::config::HarnLintConfig,
    script_rule_diagnostics: &[harn_lint::LintDiagnostic],
) -> (CommandOutcome, String) {
    let path_str = path.to_string_lossy().into_owned();
    let engine_rules = project_engine_rule_sources(path);
    let native_rule_paths = project_native_rule_paths(path);
    let options = super::config::lint_options(
        path,
        config,
        lint_config,
        &engine_rules,
        &native_rule_paths,
        super::config::LintSurface::Lint,
    );
    let options = harn_lint::LintOptions {
        binding_types: Some(&output.binding_types),
        ..options
    };
    // Generated files (`*.generated.harn`) skip style/declaration lints inside
    // `lint_with_module_graph`; type diagnostics still flow so real correctness
    // errors are never hidden.
    let generated = is_generated_path(path);
    let mut diagnostics = harn_lint::lint_with_module_graph(
        &output.program,
        &config.disable_rules,
        Some(&output.source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );
    diagnostics.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
        &output.diagnostics,
        &config.disable_rules,
    ));
    // `.harn`-authored custom lint rules (#2850), pre-computed in the async
    // command handler (they need the VM) and merged here so they render and
    // affect the exit code exactly like built-in rules.
    if !generated {
        diagnostics.extend(
            script_rule_diagnostics
                .iter()
                .filter(|d| {
                    !config
                        .disable_rules
                        .iter()
                        .any(|r| r.as_str() == d.rule.as_ref())
                })
                .cloned(),
        );
    }

    if diagnostics.is_empty() {
        return (
            CommandOutcome::default(),
            format!("{path_str}: no issues found\n"),
        );
    }

    render_lint_outcome(&path_str, &output.source, &diagnostics)
}

/// Apply autofix edits from lint and type-check diagnostics and write back to
/// disk. Returns a [`CommandOutcome`] describing the *residual* diagnostics —
/// the unfixable findings when nothing was autofixable, or whatever survives a
/// re-lint after fixes are applied — so the caller folds `--fix` into the exit
/// code exactly like the plain and `--json` lint paths. Without this, an
/// error-level unfixable diagnostic would let `harn lint --fix` exit 0 (and
/// print nothing) over a real error.
pub(crate) fn lint_fix_file(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    lint_config: &super::config::HarnLintConfig,
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let output = analyze_file(analysis, path, config, module_graph)
        .unwrap_or_else(|error| render_file_analysis_error_or_exit(&path_str, error));
    let source = output.source;
    let program = output.program;

    let engine_rules = project_engine_rule_sources(path);
    let native_rule_paths = project_native_rule_paths(path);
    let options = super::config::lint_options(
        path,
        config,
        lint_config,
        &engine_rules,
        &native_rule_paths,
        super::config::LintSurface::Lint,
    );
    let options = harn_lint::LintOptions {
        binding_types: Some(&output.binding_types),
        ..options
    };
    // Generated files self-skip style lints inside `lint_with_module_graph`, so
    // no style-lint autofix edits are produced for them.
    let lint_diags = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );

    let edits: Vec<harn_lexer::FixEdit> = lint_diags
        .iter()
        .filter_map(harn_lint::LintDiagnostic::machine_applicable_fix)
        .chain(
            output
                .diagnostics
                .iter()
                .filter(|d| !harn_lint::type_diagnostic_lint_disabled(d, &config.disable_rules))
                .filter_map(harn_parser::TypeDiagnostic::machine_applicable_fix),
        )
        .flatten()
        .cloned()
        .collect();

    if edits.is_empty() {
        // Nothing is machine-fixable. Mirror the plain lint path: print the
        // diagnostics and report their outcome so the caller can fail the exit
        // code, instead of silently returning success over a real error.
        let mut diagnostics = lint_diags;
        diagnostics.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
            &output.diagnostics,
            &config.disable_rules,
        ));
        return print_lint_diagnostics(&path_str, &source, &diagnostics);
    }

    let applied = harn_lexer::FixEdit::dedupe_overlapping(&edits).len();
    let result = harn_lexer::FixEdit::apply_all(&source, &edits);
    let result =
        crate::commands::source_formatting::keep_canonical_formatting(path, &source, result);
    write_applied_fixes(path, &path_str, applied, &result);

    let output2 = analyze_file(analysis, path, config, module_graph)
        .unwrap_or_else(|error| render_file_analysis_error_or_exit(&path_str, error));
    let source2 = output2.source;
    let program2 = output2.program;
    let mut remaining = harn_lint::lint_with_module_graph(
        &program2,
        &config.disable_rules,
        Some(&source2),
        externally_imported_names,
        module_graph,
        path,
        &harn_lint::LintOptions {
            binding_types: Some(&output2.binding_types),
            ..options
        },
    );
    remaining.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
        &output2.diagnostics,
        &config.disable_rules,
    ));
    // Report whatever survived the fix so the caller folds it into the exit
    // code. `applied` is only advisory (printed above); the residual outcome is
    // what governs pass/fail.
    print_lint_diagnostics(&path_str, &source2, &remaining)
}
