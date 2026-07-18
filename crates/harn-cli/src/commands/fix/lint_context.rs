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
            require_public_api_types: self.lint.require_public_api_types,
            complexity_threshold: self.lint.complexity_threshold,
            persona_step_allowlist: &self.lint.persona_step_allowlist,
            require_stdlib_metadata: check::path_is_stdlib_source(path),
            engine_rules: &self.engine_rules,
            native_rule_paths: &self.native_rule_paths,
            severity_overrides: self.lint.severity_overrides.clone(),
        }
    }
}
