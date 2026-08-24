use std::path::{Path, PathBuf};

use harn_lint::LintOptions;

use crate::commands::check;

pub(super) struct FixLintContext {
    lint: check::HarnLintConfig,
    engine_rules: Vec<String>,
    native_rule_paths: Vec<PathBuf>,
}

impl FixLintContext {
    pub(super) fn load(path: &Path) -> Self {
        Self {
            lint: check::load_harn_lint_config(path),
            engine_rules: check::project_engine_rule_sources(path),
            native_rule_paths: check::project_native_rule_paths(path),
        }
    }

    pub(super) fn options<'a>(&'a self, path: &'a Path) -> LintOptions<'a> {
        LintOptions {
            file_path: Some(path),
            require_file_header: self.lint.require_file_header,
            require_docstrings: self.lint.require_docstrings,
            complexity_threshold: self.lint.complexity_threshold,
            persona_step_allowlist: &self.lint.persona_step_allowlist,
            require_stdlib_metadata: check::path_is_stdlib_source(path),
            engine_rules: &self.engine_rules,
            native_rule_paths: &self.native_rule_paths,
            severity_overrides: self.lint.severity_overrides.clone(),
            // `harn fix` never rewrites a privileged wire — `HARN-LNT-072`
            // carries no repair — so the trust flag would change nothing here.
            trusted_host_dispatch: false,
            connector_runtime_module: crate::package::is_declared_connector_module(path),
        }
    }
}
