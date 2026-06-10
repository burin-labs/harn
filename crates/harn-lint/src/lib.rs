//! Harn's lint crate. The public surface is intentionally narrow: a
//! handful of `lint_*` entry points, the diagnostic and options types,
//! and a couple of small utility functions reused by other crates. All
//! walk state, rule dispatch, and source-aware rule implementations
//! live in sibling modules.

use std::collections::HashSet;
use std::path::Path;

use harn_modules::WildcardResolution;
use harn_parser::{DiagnosticCode as Code, SNode};

mod complexity;
mod decls;
mod diagnostic;
mod engine_rule;
mod fixes;
mod harndoc;
mod linter;
mod naming;
pub mod native;
mod native_rule;
mod rule;
mod rules;

#[cfg(test)]
mod tests;

pub use diagnostic::{LintDiagnostic, LintOptions, LintSeverity, DEFAULT_COMPLEXITY_THRESHOLD};
pub use naming::simplify_bool_comparison;
pub use rules::file_header::derive_file_header_title;
pub use rules::template_variant_explosion::DEFAULT_BRANCH_THRESHOLD as DEFAULT_TEMPLATE_VARIANT_BRANCH_THRESHOLD;

/// Lint a single `.harn.prompt` template source. Returns the
/// diagnostics produced by the template-specific lint rules
/// (`template-provider-identity-branch`, `template-variant-explosion`).
///
/// `branch_threshold` overrides the default for the variant-
/// explosion rule (see [`DEFAULT_TEMPLATE_VARIANT_BRANCH_THRESHOLD`]);
/// `disabled_rules` is the same comma-separated list `harn lint`
/// accepts everywhere else.
///
/// Returns a single `LintDiagnostic` with rule `"template-parse"`
/// when the template doesn't parse — surface that to the user before
/// continuing, mirroring how `harn lint` reports parse failures for
/// `.harn` programs.
pub fn lint_prompt_template(
    source: &str,
    branch_threshold: Option<usize>,
    disabled_rules: &[String],
) -> Vec<LintDiagnostic> {
    let constructs = match harn_vm::stdlib::template::lint::parse(source) {
        Ok(constructs) => constructs,
        Err(message) => {
            return vec![LintDiagnostic {
                code: Code::LintTemplateParse,
                rule: "template-parse".into(),
                message: format!("template did not parse: {message}"),
                span: harn_lexer::Span::dummy(),
                severity: LintSeverity::Error,
                suggestion: None,
                fix: None,
            }];
        }
    };
    let threshold = branch_threshold.unwrap_or(DEFAULT_TEMPLATE_VARIANT_BRANCH_THRESHOLD);
    let mut diagnostics = Vec::new();
    diagnostics.extend(rules::template_provider_identity::check(
        &constructs,
        source,
    ));
    diagnostics.extend(rules::template_variant_explosion::check(
        &constructs,
        threshold,
        source,
    ));
    if disabled_rules.is_empty() {
        diagnostics
    } else {
        diagnostics
            .into_iter()
            .filter(|d| !rule_disabled(&d.rule, disabled_rules))
            .collect()
    }
}

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

/// The filename suffix that marks a machine-generated Harn source file:
/// `<name>.generated.harn`.
pub const GENERATED_HARN_SUFFIX: &str = ".generated.harn";

/// True if `path` names a machine-generated Harn file (`*.generated.harn`).
///
/// Style and declaration lints are skipped for these files because their shape
/// is owned by the generator (e.g. `harn pg codegen`), not the author: an unused
/// generated row type or banner comment is noise, not a defect. Type diagnostics
/// run on a separate path and still apply, and `harn fmt` still formats them.
///
/// The signal is the *filename*, deliberately not an in-file `@generated` /
/// `DO NOT EDIT` comment: a content marker is a one-line lint backdoor any
/// author can paste in to dodge rules, whereas renaming a hand-written file to
/// `*.generated.harn` is structural and obvious in review. (Compare Go's
/// `// Code generated … DO NOT EDIT.` regex and Biome's `@generated`; we trade
/// their convenience for a signal that cannot be forged in passing.)
pub fn is_generated_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(GENERATED_HARN_SUFFIX))
}

fn lint_full(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    options: &LintOptions<'_>,
    module_graph: Option<(&harn_modules::ModuleGraph, &Path)>,
) -> Vec<LintDiagnostic> {
    // Generated files (`*.generated.harn`) skip style/declaration lints entirely.
    // Type diagnostics flow through a separate path, so real correctness errors
    // are never hidden.
    if options.file_path.is_some_and(is_generated_path) {
        return Vec::new();
    }
    let mut linter = Linter::new(source);
    // Append project rule-engine rules to the registry. They run in the
    // whole-program phase over the source; a malformed one is skipped.
    for engine_source in options.engine_rules {
        if let Some(rule) = crate::engine_rule::EngineRule::from_toml(engine_source) {
            linter.rules.push(Box::new(rule));
        }
    }
    let (mut native_rules, mut native_load_diagnostics) =
        crate::native_rule::load_rules_from_paths(options.native_rule_paths);
    linter.rules.append(&mut native_rules);
    linter.rules_visit_nodes = linter.rules.iter().any(|rule| rule.visits_nodes());
    linter.diagnostics.append(&mut native_load_diagnostics);
    linter.file_path = options.file_path.map(Path::to_path_buf);
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
    linter.require_stdlib_metadata = options.require_stdlib_metadata;
    linter.require_docstrings = options.require_docstrings;
    linter
        .persona_step_allowlist
        .extend(options.persona_step_allowlist.iter().cloned());
    linter.lint_program(program);
    if let Some(src) = source {
        if options.require_file_header {
            check_require_file_header(src, options.file_path, &mut linter.diagnostics);
        }
    }
    linter.finalize();
    let mut diagnostics: Vec<LintDiagnostic> = if disabled_rules.is_empty() {
        linter.diagnostics
    } else {
        linter
            .diagnostics
            .into_iter()
            .filter(|d| !rule_disabled(&d.rule, disabled_rules))
            .collect()
    };
    // Per-rule severity overrides apply after disable-filtering.
    if !options.severity_overrides.is_empty() {
        for diagnostic in &mut diagnostics {
            if let Some(&severity) = options.severity_overrides.get(diagnostic.rule.as_ref()) {
                diagnostic.severity = severity;
            }
        }
    }
    diagnostics
}

/// Convert type-checker diagnostics tagged as lint rules into ordinary
/// lint diagnostics so CLI/editor callers can share rule filtering,
/// rendering, and autofix plumbing.
pub fn lint_diagnostics_from_type_diagnostics(
    diagnostics: &[harn_parser::TypeDiagnostic],
    disabled_rules: &[String],
) -> Vec<LintDiagnostic> {
    diagnostics
        .iter()
        .filter_map(type_diagnostic_as_lint)
        .filter(|diagnostic| !rule_disabled(&diagnostic.rule, disabled_rules))
        .collect()
}

/// Returns true when a type diagnostic is a lint diagnostic disabled by
/// the caller's lint configuration.
pub fn type_diagnostic_lint_disabled(
    diagnostic: &harn_parser::TypeDiagnostic,
    disabled_rules: &[String],
) -> bool {
    type_diagnostic_lint_rule(diagnostic).is_some_and(|rule| rule_disabled(rule, disabled_rules))
}

fn type_diagnostic_as_lint(diagnostic: &harn_parser::TypeDiagnostic) -> Option<LintDiagnostic> {
    let rule = type_diagnostic_lint_rule(diagnostic)?;
    let span = diagnostic.span?;
    Some(LintDiagnostic {
        code: diagnostic.code,
        rule: rule.into(),
        message: diagnostic.message.clone(),
        span,
        severity: match diagnostic.severity {
            harn_parser::DiagnosticSeverity::Warning => LintSeverity::Warning,
            harn_parser::DiagnosticSeverity::Error => LintSeverity::Error,
        },
        suggestion: diagnostic.help.clone(),
        fix: diagnostic.fix.clone(),
    })
}

fn type_diagnostic_lint_rule(diagnostic: &harn_parser::TypeDiagnostic) -> Option<&'static str> {
    match &diagnostic.details {
        Some(harn_parser::DiagnosticDetails::LintRule { rule }) => Some(*rule),
        _ => None,
    }
}

fn rule_disabled(rule: &str, disabled_rules: &[String]) -> bool {
    disabled_rules
        .iter()
        .any(|disabled| rule_matches_disabled(rule, disabled))
}

fn rule_matches_disabled(rule: &str, disabled: &str) -> bool {
    rule == disabled || (rule == "dead-code-after-return" && disabled == "unreachable-code")
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
