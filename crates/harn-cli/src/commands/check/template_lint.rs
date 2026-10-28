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
//! - [`lint_prompt_fix_file`] — apply prompt-template fixes and report any
//!   diagnostics that remain.
//!
//! Output goes to stderr in its entirety, like the `.harn` path; see
//! [`super::outcome::print_lint_diagnostics`].
#![deny(clippy::print_stdout)]

use std::path::{Path, PathBuf};

use super::outcome::{print_lint_diagnostics, write_applied_fixes, CommandOutcome};

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
            cli_diagnostic!("error: failed to read {path_str}: {error}");
            return CommandOutcome {
                has_error: true,
                has_warning: false,
                ..CommandOutcome::default()
            };
        }
    };
    let diagnostics = harn_lint::lint_prompt_template(&source, branch_threshold, disabled_rules);
    if diagnostics.is_empty() {
        cli_diagnostic!("{path_str}: no issues found");
        return CommandOutcome::default();
    }
    print_lint_diagnostics(&path_str, &source, &diagnostics)
}

/// Apply machine-applicable prompt-template lint fixes and re-lint the result.
pub(crate) fn lint_prompt_fix_file(
    path: &Path,
    branch_threshold: Option<usize>,
    disabled_rules: &[String],
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            cli_diagnostic!("error: failed to read {path_str}: {error}");
            return CommandOutcome {
                has_error: true,
                has_warning: false,
                ..CommandOutcome::default()
            };
        }
    };
    let diagnostics = harn_lint::lint_prompt_template(&source, branch_threshold, disabled_rules);
    let edits: Vec<harn_lexer::FixEdit> = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.fix.as_ref())
        .flatten()
        .cloned()
        .collect();
    if edits.is_empty() {
        return print_lint_diagnostics(&path_str, &source, &diagnostics);
    }

    let applied = harn_lexer::FixEdit::dedupe_overlapping(&edits).len();
    let fixed = harn_lexer::FixEdit::apply_all(&source, &edits);
    write_applied_fixes(path, &path_str, applied, &fixed);

    let remaining = harn_lint::lint_prompt_template(&fixed, branch_threshold, disabled_rules);
    print_lint_diagnostics(&path_str, &fixed, &remaining)
}
