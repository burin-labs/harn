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
            complexity_threshold: cfg.lint.complexity_threshold,
            persona_step_allowlist: cfg.lint.persona_step_allowlist,
            template_variant_branch_threshold: cfg.lint.template_variant_branch_threshold,
            severity_overrides: cfg.lint.severity,
        },
        Err(e) => {
            eprintln!("warning: {e}");
            HarnLintConfig::default()
        }
    }
}

/// Which command is running the lint rules.
///
/// The two surfaces genuinely differ in one place, and naming it is the point:
/// the difference used to be expressed as `LintOptions::default()` on the
/// `check` side, which silently defaulted *every* field rather than the one
/// that should differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LintSurface {
    /// `harn lint`. Honors the project `[lint]` block and auto-enables the
    /// stdlib contract lints for sources under the embedded stdlib, which is
    /// what `LintOptions::require_stdlib_metadata` documents.
    Lint,
    /// The lint pass inside `harn check`. It has never loaded the `[lint]`
    /// block, and the stdlib contract lints stay off here — teaching `harn
    /// check` to enforce either would *add* findings to projects that set
    /// `require_file_header` and friends, so it is a separate, deliberate
    /// change. The trust declaration still flows, because ignoring *that* one
    /// produces false positives rather than fewer of them.
    Check,
}

/// The options a lint pass runs with, from the configs that own them.
///
/// One constructor, because the fields come from two places and the call sites
/// had drifted: three built this struct field-for-field and identically, while
/// `harn check`'s lint pass built `LintOptions::default()` and discarded every
/// setting — including the trust declaration, so
/// `harn check --trusted-host-dispatch` cleared the type error on a privileged
/// wire and left the lint warning behind (harn#6171). A new field now reaches
/// every surface or none, and a surface that should not get it says so.
///
/// `engine_rules` and `native_rule_paths` stay parameters rather than being
/// loaded here: they are read per file from `[rules]`, and the caller that has
/// them already paid for that walk.
pub(crate) fn lint_options<'a>(
    path: &'a Path,
    config: &CheckConfig,
    lint_config: &'a HarnLintConfig,
    engine_rules: &'a [String],
    native_rule_paths: &'a [PathBuf],
    surface: LintSurface,
) -> harn_lint::LintOptions<'a> {
    harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header: lint_config.require_file_header,
        require_docstrings: lint_config.require_docstrings,
        complexity_threshold: lint_config.complexity_threshold,
        persona_step_allowlist: &lint_config.persona_step_allowlist,
        require_stdlib_metadata: surface == LintSurface::Lint
            && harn_lint::path_is_stdlib_source(path),
        engine_rules,
        native_rule_paths,
        severity_overrides: lint_config.severity_overrides.clone(),
        // The trust decision the type-checker already read, so `check` and
        // `lint` cannot disagree about who may reach a privileged wire
        // (harn#6162, harn#6171).
        trusted_host_dispatch: config.trusted_host_dispatch,
        // The manifest's own answer about which module is a connector, so the
        // lint does not have to infer it from the file's contents
        // (harn#6149).
        connector_runtime_module: crate::package::is_declared_connector_module(path),
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
