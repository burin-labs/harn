use std::collections::HashSet;
use std::path::Path;
use std::process;

use harn_lint::LintSeverity;
use harn_parser::TypeChecker;

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
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);

    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
    };
    let diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );

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
) -> usize {
    let path_str = path.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);

    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
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

    let mut checker = TypeChecker::with_strict_types(config.strict_types);
    if let Some(imported) = module_graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = module_graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    let type_diags = checker.check_with_source(&program, &source);

    let mut edits: Vec<&harn_lexer::FixEdit> = lint_diags
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .chain(type_diags.iter().filter_map(|d| d.fix.as_ref()))
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
    let remaining = harn_lint::lint_with_module_graph(
        &program2,
        &config.disable_rules,
        Some(&source2),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );
    if !remaining.is_empty() {
        print_lint_diagnostics(&path_str, &source, &remaining);
    }

    applied
}
