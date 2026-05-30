//! Glue between `harn lint` and the prompt-template lint rules.
//!
//! `.harn.prompt` files don't go through the regular Harn parser, so
//! they need their own discovery + invocation path. This module
//! provides:
//!
//! - [`collect_prompt_targets`] — recursive walker that picks up
//!   `*.harn.prompt` / `*.prompt` files under the targets passed to
//!   `harn lint`.
//! - [`lint_prompt_file_inner`] — read the source, dispatch to
//!   `harn_lint::lint_prompt_template`, and print diagnostics in the
//!   same format as `harn lint` uses for `.harn` files.

use std::path::{Path, PathBuf};

use harn_lint::LintSeverity;

use super::outcome::{print_lint_diagnostics, CommandOutcome};

pub(crate) fn collect_lint_targets(targets: &[&str]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let files = super::super::collect_source_targets(targets, true, true);
    (files.harn, files.prompts)
}

/// Lint a single `.harn.prompt` template. Prints diagnostics through
/// the shared CLI formatter so output is consistent with `.harn` lint
/// runs.
pub(crate) fn lint_prompt_file_inner(
    path: &Path,
    branch_threshold: Option<usize>,
    disabled_rules: &[String],
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("error: failed to read {path_str}: {error}");
            return CommandOutcome {
                has_error: true,
                has_warning: false,
            };
        }
    };
    let diagnostics = harn_lint::lint_prompt_template(&source, branch_threshold, disabled_rules);
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
