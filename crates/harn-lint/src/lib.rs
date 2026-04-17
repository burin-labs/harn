//! Harn's lint crate. The public surface is intentionally narrow: a
//! handful of `lint_*` entry points, the diagnostic and options types,
//! and a couple of small utility functions reused by other crates. All
//! walk state, rule dispatch, and source-aware rule implementations
//! live in sibling modules.

use std::collections::HashSet;
use std::path::Path;

use harn_modules::WildcardResolution;
use harn_parser::SNode;

mod complexity;
mod decls;
mod diagnostic;
mod fixes;
mod harndoc;
mod linter;
mod naming;
mod rules;

#[cfg(test)]
mod tests;

pub use diagnostic::{LintDiagnostic, LintOptions, LintSeverity, DEFAULT_COMPLEXITY_THRESHOLD};
pub use naming::simplify_bool_comparison;
pub use rules::file_header::derive_file_header_title;

use linter::Linter;
use rules::file_header::check_require_file_header;

/// Lint an AST program and return all diagnostics.
pub fn lint(program: &[SNode]) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, &[], None)
}

/// Lint an AST program with source-aware rules enabled.
pub fn lint_with_source(program: &[SNode], source: &str) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, &[], Some(source))
}

/// Lint an AST program, filtering out diagnostics for disabled rules.
pub fn lint_with_config(program: &[SNode], disabled_rules: &[String]) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, disabled_rules, None)
}

/// Lint an AST program, optionally using the original source for source-aware rules.
pub fn lint_with_config_and_source(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
) -> Vec<LintDiagnostic> {
    lint_full(
        program,
        disabled_rules,
        source,
        &HashSet::new(),
        &LintOptions::default(),
        None,
    )
}

/// Lint with cross-file import awareness. Functions named in
/// `externally_imported_names` are exempt from the unused-function lint
/// even without local references.
pub fn lint_with_cross_file_imports(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
) -> Vec<LintDiagnostic> {
    lint_full(
        program,
        disabled_rules,
        source,
        externally_imported_names,
        &LintOptions::default(),
        None,
    )
}

/// Lint with cross-file import awareness driven by [`harn_modules::ModuleGraph`].
pub fn lint_with_module_graph(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    file_path: &Path,
    options: &LintOptions<'_>,
) -> Vec<LintDiagnostic> {
    lint_full(
        program,
        disabled_rules,
        source,
        externally_imported_names,
        options,
        Some((module_graph, file_path)),
    )
}

/// Lint with cross-file import awareness plus extra [`LintOptions`].
pub fn lint_with_options(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    options: &LintOptions<'_>,
) -> Vec<LintDiagnostic> {
    lint_full(
        program,
        disabled_rules,
        source,
        externally_imported_names,
        options,
        None,
    )
}

fn lint_full(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    options: &LintOptions<'_>,
    module_graph: Option<(&harn_modules::ModuleGraph, &Path)>,
) -> Vec<LintDiagnostic> {
    let mut linter = Linter::new(source);
    linter
        .externally_imported_names
        .clone_from(externally_imported_names);
    if let Some((module_graph, file_path)) = module_graph {
        linter.use_module_graph_for_wildcards = true;
        linter.module_graph_wildcard_exports = match module_graph.wildcard_exports_for(file_path) {
            WildcardResolution::Resolved(exports) => Some(exports),
            WildcardResolution::Unknown => None,
        };
    }
    if let Some(threshold) = options.complexity_threshold {
        linter.complexity_threshold = threshold;
    }
    linter.lint_program(program);
    if let Some(src) = source {
        if options.require_file_header {
            check_require_file_header(src, options.file_path, &mut linter.diagnostics);
        }
    }
    linter.finalize();
    if disabled_rules.is_empty() {
        linter.diagnostics
    } else {
        linter
            .diagnostics
            .into_iter()
            .filter(|d| !disabled_rules.iter().any(|r| r == d.rule))
            .collect()
    }
}

/// Extract all function names that appear in selective import statements
/// (`import { foo, bar } from "module"`).
pub fn collect_selective_import_names(program: &[SNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for snode in program {
        if let harn_parser::Node::SelectiveImport {
            names: imported, ..
        } = &snode.node
        {
            names.extend(imported.iter().cloned());
        }
    }
    names
}
