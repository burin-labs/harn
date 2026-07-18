use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;

use crate::package::CheckConfig;
use harn_modules::project_config as harn_config;
use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};

#[derive(Clone, Debug, Default)]
pub(crate) struct HarnLintConfig {
    pub(crate) disabled: Vec<String>,
    pub(crate) require_file_header: bool,
    pub(crate) require_docstrings: bool,
    pub(crate) require_public_api_types: bool,
    pub(crate) complexity_threshold: Option<usize>,
    pub(crate) persona_step_allowlist: Vec<String>,
    pub(crate) template_variant_branch_threshold: Option<usize>,
    pub(crate) severity_overrides: HashMap<String, harn_lint::LintSeverity>,
}

pub(crate) fn load_harn_lint_config(path: &Path) -> HarnLintConfig {
    match harn_config::load_for_path(path) {
        Ok(cfg) => HarnLintConfig {
            disabled: cfg.lint.disabled.unwrap_or_default(),
            require_file_header: cfg.lint.require_file_header.unwrap_or(false),
            require_docstrings: cfg.lint.require_docstrings.unwrap_or(false),
            require_public_api_types: cfg.lint.require_public_api_types.unwrap_or(false),
            complexity_threshold: cfg.lint.complexity_threshold,
            persona_step_allowlist: cfg.lint.persona_step_allowlist,
            template_variant_branch_threshold: cfg.lint.template_variant_branch_threshold,
            severity_overrides: parse_severity_overrides(cfg.lint.severity),
        },
        Err(e) => {
            eprintln!("warning: {e}");
            HarnLintConfig::default()
        }
    }
}

/// Merge `[lint].disabled` from the nearest harn.toml into `disable_rules`.
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

/// Per-rule severity overrides from `[lint.severity]` (#2851), parsed to
/// [`harn_lint::LintSeverity`]. An unrecognized severity string is dropped
/// with a warning rather than failing the run.
fn parse_severity_overrides(
    raw: HashMap<String, String>,
) -> HashMap<String, harn_lint::LintSeverity> {
    raw.into_iter()
        .filter_map(
            |(rule, severity)| match severity.to_ascii_lowercase().as_str() {
                "error" => Some((rule, harn_lint::LintSeverity::Error)),
                "warning" | "warn" => Some((rule, harn_lint::LintSeverity::Warning)),
                "info" => Some((rule, harn_lint::LintSeverity::Info)),
                other => {
                    eprintln!("warning: [lint.severity] `{rule}`: unknown severity `{other}`");
                    None
                }
            },
        )
        .collect()
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

/// Build the module graph and hand back the seed files' parsed ASTs for the
/// parallel driver to distribute (each worker seeds its own
/// [`AnalysisDatabase`] from this map instead of re-parsing from disk).
pub(crate) fn build_module_graph_with_parsed_sources(
    files: &[PathBuf],
) -> (
    harn_modules::ModuleGraph,
    HashMap<PathBuf, harn_modules::ParsedModuleSource>,
) {
    ensure_module_dependencies(files);
    let build = harn_modules::build_with_parsed_sources(files);
    (build.graph, build.parsed_sources)
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
