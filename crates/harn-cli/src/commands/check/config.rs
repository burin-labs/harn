use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process;

use crate::config as harn_config;
use crate::package::CheckConfig;

/// Merge `[lint].disabled` from the nearest harn.toml into `disable_rules`.
/// The `require_file_header` flag is handled separately via
/// [`harn_lint_require_file_header`] so it can be enabled without a full
/// `[check]` section.
pub(crate) fn apply_harn_lint_config(path: &Path, config: &mut CheckConfig) {
    let Ok(cfg) = harn_config::load_for_path(path) else {
        return;
    };
    if let Some(disabled) = cfg.lint.disabled {
        for rule in disabled {
            if !config.disable_rules.iter().any(|r| r == &rule) {
                config.disable_rules.push(rule);
            }
        }
    }
}

/// Read `[lint] require_file_header` from the nearest harn.toml, defaulting
/// to `false`. Invalid config is treated as `false` and surfaced via a
/// warning.
pub(crate) fn harn_lint_require_file_header(path: &Path) -> bool {
    match harn_config::load_for_path(path) {
        Ok(cfg) => cfg.lint.require_file_header.unwrap_or(false),
        Err(e) => {
            eprintln!("warning: {e}");
            false
        }
    }
}

/// Read `[lint] complexity_threshold` from the nearest harn.toml. Returns
/// `None` when unset or when the manifest is missing/malformed — the
/// linter falls back to `harn_lint::DEFAULT_COMPLEXITY_THRESHOLD`.
pub(crate) fn harn_lint_complexity_threshold(path: &Path) -> Option<usize> {
    match harn_config::load_for_path(path) {
        Ok(cfg) => cfg.lint.complexity_threshold,
        Err(e) => {
            eprintln!("warning: {e}");
            None
        }
    }
}

pub(crate) fn collect_harn_targets(targets: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            super::super::collect_harn_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Collect every function name that appears in a selective import across
/// the given files, so the linter doesn't flag library functions consumed
/// by other files as unused.
pub(crate) fn collect_cross_file_imports(
    module_graph: &harn_modules::ModuleGraph,
) -> HashSet<String> {
    module_graph
        .all_selective_import_names()
        .into_iter()
        .map(|name| name.to_string())
        .collect()
}

pub(crate) fn build_module_graph(files: &[PathBuf]) -> harn_modules::ModuleGraph {
    for file in files {
        if let Err(error) = crate::package::ensure_dependencies_materialized(file) {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
    harn_modules::build(files)
}
