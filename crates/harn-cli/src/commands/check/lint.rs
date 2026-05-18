use std::collections::HashSet;
use std::path::Path;
use std::process;

use harn_lint::LintSeverity;
use harn_parser::{TypeChecker, TypeDiagnostic};

use crate::package::CheckConfig;
use crate::parse_source_file;

use super::outcome::{print_lint_diagnostics, CommandOutcome};

pub(crate) fn lint_file_inner(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);

    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
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
    let type_diags = type_check_for_lint(path, config, module_graph, &program, &source);
    diagnostics.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
        &type_diags,
        &config.disable_rules,
    ));

    if diagnostics.is_empty() {
        println!("{path_str}: no issues found");
        return CommandOutcome::default();
    }

    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let has_error = print_lint_diagnostics(&path_str, &source, &diagnostics);

    CommandOutcome {
        has_error,
        has_warning,
    }
}

/// Apply autofix edits from lint and type-check diagnostics and write back to disk.
/// Returns the number of fixes applied.
pub(crate) fn lint_fix_file(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
) -> usize {
    let path_str = path.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);

    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
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

    let type_diags = type_check_for_lint(path, config, module_graph, &program, &source);

    let mut edits: Vec<&harn_lexer::FixEdit> = lint_diags
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .chain(
            type_diags
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

    let mut result = source.clone();
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

    let (source2, program2) = parse_source_file(&path_str);
    let mut remaining = harn_lint::lint_with_module_graph(
        &program2,
        &config.disable_rules,
        Some(&source2),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );
    let type_remaining = type_check_for_lint(path, config, module_graph, &program2, &source2);
    remaining.extend(harn_lint::lint_diagnostics_from_type_diagnostics(
        &type_remaining,
        &config.disable_rules,
    ));
    if !remaining.is_empty() {
        print_lint_diagnostics(&path_str, &source2, &remaining);
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

fn type_check_for_lint(
    path: &Path,
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    program: &[harn_parser::SNode],
    source: &str,
) -> Vec<TypeDiagnostic> {
    let mut checker = TypeChecker::with_strict_types(config.strict_types);
    if let Some(imported) = module_graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = module_graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = module_graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker.check_with_source(program, source)
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
