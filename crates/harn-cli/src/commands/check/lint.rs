use std::collections::HashSet;
use std::path::Path;
use std::process;

use harn_lint::LintSeverity;
use harn_parser::analysis::AnalysisDatabase;

use crate::package::CheckConfig;

use super::analysis::{analyze_file, render_file_analysis_error_or_exit};
use super::outcome::{print_lint_diagnostics, CommandOutcome};

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
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let output = analyze_file(analysis, path, config, module_graph)
        .unwrap_or_else(|error| render_file_analysis_error_or_exit(&path_str, error));
    let source = output.source;
    let program = output.program;

    let engine_rules = project_engine_rule_sources(path);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
        engine_rules: &engine_rules,
    };
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

/// Apply autofix edits from lint and type-check diagnostics and write back to disk.
/// Returns the number of fixes applied.
pub(crate) fn lint_fix_file(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
) -> usize {
    let path_str = path.to_string_lossy().into_owned();
    let output = analyze_file(analysis, path, config, module_graph)
        .unwrap_or_else(|error| render_file_analysis_error_or_exit(&path_str, error));
    let source = output.source;
    let program = output.program;

    let engine_rules = project_engine_rule_sources(path);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
        engine_rules: &engine_rules,
    };
    let lint_diags = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );

    let mut edits: Vec<&harn_lexer::FixEdit> = lint_diags
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
        .collect();

    if edits.is_empty() {
        return 0;
    }

    // Descending by span.start so edits apply right-to-left without
    // invalidating earlier offsets; drop overlaps in that same order.
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));

    let mut accepted: Vec<&harn_lexer::FixEdit> = Vec::new();
    for edit in &edits {
        let overlaps = accepted
            .iter()
            .any(|prev| edit.span.start < prev.span.end && edit.span.end > prev.span.start);
        if !overlaps {
            accepted.push(edit);
        }
    }

    let mut result = source;
    for edit in &accepted {
        let before = &result[..edit.span.start];
        let after = &result[edit.span.end..];
        result = format!("{before}{}{after}", edit.replacement);
    }

    let applied = accepted.len();
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
    if !remaining.is_empty() {
        let _ = print_lint_diagnostics(&path_str, &source2, &remaining);
    }

    applied
}

/// Stdlib metadata enforcement is path-driven: when `harn lint` runs over a
/// canonical embedded source under `crates/harn-stdlib/src/stdlib/`, every
/// `pub fn` must carry the `@effects` / `@allocation` / `@errors` /
/// `@api_stability` / `@example` block (HARN-STD-101). Outside that tree
/// the rule is dormant, so user scripts never trip on it.
pub(crate) fn path_is_stdlib_source(path: &Path) -> bool {
    use std::path::Component;
    let mut prev: Option<&std::ffi::OsStr> = None;
    let mut prev_prev: Option<&std::ffi::OsStr> = None;
    for comp in path.components() {
        if let Component::Normal(name) = comp {
            if prev == Some(std::ffi::OsStr::new("src"))
                && prev_prev == Some(std::ffi::OsStr::new("harn-stdlib"))
                && name == std::ffi::OsStr::new("stdlib")
            {
                return true;
            }
            prev_prev = prev;
            prev = Some(name);
        } else {
            prev_prev = prev;
            prev = None;
        }
    }
    false
}

#[cfg(test)]
mod path_is_stdlib_source_tests {
    use super::path_is_stdlib_source;
    use std::path::Path;

    #[test]
    fn detects_canonical_embedded_layout() {
        assert!(path_is_stdlib_source(Path::new(
            "crates/harn-stdlib/src/stdlib/stdlib_fs.harn"
        )));
        assert!(path_is_stdlib_source(Path::new(
            "/abs/path/crates/harn-stdlib/src/stdlib/agent/loop.harn"
        )));
    }

    #[test]
    fn rejects_non_stdlib_paths() {
        assert!(!path_is_stdlib_source(Path::new("scripts/foo.harn")));
        assert!(!path_is_stdlib_source(Path::new(
            "crates/harn-vm/src/stdlib_acp.harn"
        )));
        assert!(!path_is_stdlib_source(Path::new(
            "conformance/tests/foo.harn"
        )));
    }
}
