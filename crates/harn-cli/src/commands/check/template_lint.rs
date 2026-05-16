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

/// Recursively gather `.harn.prompt` / `.prompt` files under `targets`.
/// Targets that are files themselves are returned as-is when they
/// match one of those extensions.
pub(crate) fn collect_prompt_targets(targets: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            collect_prompt_files(path, &mut files);
        } else if is_prompt_file(path) {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files.dedup();
    files
}

fn collect_prompt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_prompt_files(&path, out);
        } else if is_prompt_file(&path) {
            out.push(path);
        }
    }
}

fn is_prompt_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".harn.prompt") || name.ends_with(".prompt")
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
