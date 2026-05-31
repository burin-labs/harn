use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;

use crate::config as harn_config;
use crate::package::CheckConfig;
use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};

#[derive(Clone, Debug, Default)]
pub(crate) struct HarnLintConfig {
    pub(crate) disabled: Vec<String>,
    pub(crate) require_file_header: bool,
    pub(crate) complexity_threshold: Option<usize>,
    pub(crate) persona_step_allowlist: Vec<String>,
    pub(crate) template_variant_branch_threshold: Option<usize>,
}

pub(crate) fn load_harn_lint_config(path: &Path) -> HarnLintConfig {
    match harn_config::load_for_path(path) {
        Ok(cfg) => HarnLintConfig {
            disabled: cfg.lint.disabled.unwrap_or_default(),
            require_file_header: cfg.lint.require_file_header.unwrap_or(false),
            complexity_threshold: cfg.lint.complexity_threshold,
            persona_step_allowlist: cfg.lint.persona_step_allowlist,
            template_variant_branch_threshold: cfg.lint.template_variant_branch_threshold,
        },
        Err(e) => {
            eprintln!("warning: {e}");
            HarnLintConfig::default()
        }
    }
}

/// Merge `[lint].disabled` from the nearest harn.toml into `disable_rules`.
/// The `require_file_header` flag is handled separately via
/// [`harn_lint_require_file_header`] so it can be enabled without a full
/// `[check]` section.
pub(crate) fn apply_harn_lint_config(path: &Path, config: &mut CheckConfig) {
    apply_loaded_harn_lint_config(&load_harn_lint_config(path), config);
}

pub(crate) fn apply_loaded_harn_lint_config(lint: &HarnLintConfig, config: &mut CheckConfig) {
    for rule in &lint.disabled {
        if !config.disable_rules.iter().any(|r| r == rule) {
            config.disable_rules.push(rule.clone());
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

/// Read `[lint] persona_step_allowlist` from the nearest harn.toml.
pub(crate) fn harn_lint_persona_step_allowlist(path: &Path) -> Vec<String> {
    match harn_config::load_for_path(path) {
        Ok(cfg) => cfg.lint.persona_step_allowlist,
        Err(e) => {
            eprintln!("warning: {e}");
            Vec::new()
        }
    }
}

pub(crate) fn collect_harn_targets(targets: &[&str]) -> Vec<PathBuf> {
    super::super::collect_source_targets(targets, true, false).harn
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
    ensure_module_dependencies(files);
    harn_modules::build(files)
}

pub(crate) fn build_module_graph_and_seed_analysis(
    files: &[PathBuf],
    analysis: &mut AnalysisDatabase,
) -> harn_modules::ModuleGraph {
    ensure_module_dependencies(files);
    let mut build = harn_modules::build_with_parsed_sources(files);
    let mut targets_by_canonical: HashMap<PathBuf, Vec<&PathBuf>> = HashMap::new();
    for (canonical, file) in files.iter().filter_map(|file| {
        std::fs::canonicalize(file)
            .ok()
            .map(|canonical| (canonical, file))
    }) {
        targets_by_canonical
            .entry(canonical)
            .or_default()
            .push(file);
    }
    for (canonical, files) in targets_by_canonical {
        if let Some(parsed) = build.parsed_sources.remove(&canonical) {
            let mut files = files;
            if let Some(file) = files.pop() {
                for file in files {
                    seed_parsed_source(analysis, file, parsed.clone());
                }
                seed_parsed_source(analysis, file, parsed);
            }
        }
    }
    build.graph
}

fn seed_parsed_source(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    parsed: harn_modules::ParsedModuleSource,
) {
    analysis.set_parsed_source(
        SourceId::path(path),
        parsed.source,
        SourceVersion(1),
        parsed.program,
    );
}

fn ensure_module_dependencies(files: &[PathBuf]) {
    for file in files {
        if let Err(error) = crate::package::ensure_dependencies_materialized(file) {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
