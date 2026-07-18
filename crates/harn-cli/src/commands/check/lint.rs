use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process;

use harn_lint::LintSeverity;
use harn_parser::analysis::AnalysisDatabase;

use crate::package::CheckConfig;

use super::analysis::{analyze_file, render_file_analysis_error_or_exit};
use super::outcome::{print_lint_diagnostics, CommandOutcome};

use harn_lint::{is_generated_path, path_is_stdlib_source};

/// Collect the TOML sources of `language = "harn"` rules from the project's
/// `[rules] ruleDirs` (#2849), to run as lint rules. Non-harn rules can't
/// match `.harn` source and are skipped. Returns empty when no manifest or no
/// `ruleDirs` is declared (the common case — near-zero cost).
///
/// Loaded per file for simplicity; the dirs are small and the common path is a
/// single manifest lookup. Hoisting to once-per-run is a future optimization.
pub(crate) fn project_engine_rule_sources(path: &Path) -> Vec<String> {
    let Some((manifest, dir)) = crate::package::find_nearest_manifest(path) else {
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
    let Some((manifest, dir)) = crate::package::find_nearest_manifest(path) else {
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
    let source = output.source;
    let program = output.program;

    let engine_rules = project_engine_rule_sources(path);
    let native_rule_paths = project_native_rule_paths(path);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header: lint_config.require_file_header,
        require_docstrings: lint_config.require_docstrings,
        require_public_api_types: lint_config.require_public_api_types,
        complexity_threshold: lint_config.complexity_threshold,
        persona_step_allowlist: &lint_config.persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
        engine_rules: &engine_rules,
        native_rule_paths: &native_rule_paths,
        severity_overrides: lint_config.severity_overrides.clone(),
    };
    // Generated files (`*.generated.harn`) skip style/declaration lints inside
    // `lint_with_module_graph`; type diagnostics still flow so real correctness
    // errors are never hidden.
    let generated = is_generated_path(path);
    let mut diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
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
        println!("{path_str}: no issues found");
        return CommandOutcome::default();
    }

    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let (has_error, fixable) = print_lint_diagnostics(&path_str, &source, &diagnostics);

    CommandOutcome {
        has_error,
        has_warning,
        findings: diagnostics.len(),
        fixable,
    }
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
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header: lint_config.require_file_header,
        require_docstrings: lint_config.require_docstrings,
        require_public_api_types: lint_config.require_public_api_types,
        complexity_threshold: lint_config.complexity_threshold,
        persona_step_allowlist: &lint_config.persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
        engine_rules: &engine_rules,
        native_rule_paths: &native_rule_paths,
        severity_overrides: lint_config.severity_overrides.clone(),
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
        .filter_map(|d| d.fix.as_ref())
        .chain(
            output
                .diagnostics
                .iter()
                .filter(|d| !harn_lint::type_diagnostic_lint_disabled(d, &config.disable_rules))
                .filter_map(|d| d.fix.as_ref()),
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
        return outcome_from_diagnostics(&path_str, &source, &diagnostics);
    }

    // Drop overlaps and splice right-to-left via the shared FixEdit policy, so
    // the result is byte-for-byte what `harn fmt` and the LSP on-save fixer
    // produce.
    let applied = harn_lexer::FixEdit::dedupe_overlapping(&edits).len();
    let result = harn_lexer::FixEdit::apply_all(&source, &edits);
    std::fs::write(path, &result).unwrap_or_else(|e| {
        eprintln!("Failed to write {path_str}: {e}");
        process::exit(1);
    });

    println!("{path_str}: applied {applied} fix(es)");

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
        &options,
    );
    remaining.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
        &output2.diagnostics,
        &config.disable_rules,
    ));
    // Report whatever survived the fix so the caller folds it into the exit
    // code. `applied` is only advisory (printed above); the residual outcome is
    // what governs pass/fail.
    outcome_from_diagnostics(&path_str, &source2, &remaining)
}

/// Print `diagnostics` (when any) and summarize them as a [`CommandOutcome`],
/// matching how the plain lint path renders and tallies findings.
fn outcome_from_diagnostics(
    path_str: &str,
    source: &str,
    diagnostics: &[harn_lint::LintDiagnostic],
) -> CommandOutcome {
    if diagnostics.is_empty() {
        return CommandOutcome::default();
    }
    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let (has_error, fixable) = print_lint_diagnostics(path_str, source, diagnostics);
    CommandOutcome {
        has_error,
        has_warning,
        findings: diagnostics.len(),
        fixable,
    }
}
