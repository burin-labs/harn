//! Structured `--json` report for `harn lint`.
//!
//! Lint mirrors the per-file diagnostic shape already used by
//! `harn check --json` so agent consumers can dispatch on a single
//! `CheckDiagnostic` layout regardless of whether they invoked
//! `check` or `lint`.
//!
//! See `docs/src/cli-json-contract.md` for the envelope contract and
//! `crates/harn-cli/src/json_envelope.rs` for the `JsonEnvelope`
//! wrapper.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use harn_lint::LintSeverity;
use harn_parser::analysis::AnalysisDatabase;
use serde::Serialize;

use crate::json_envelope::{JsonEnvelope, JsonError};
use crate::package::CheckConfig;

use super::analysis::{analyze_file, FileAnalysisError};
use super::check_cmd::{
    check_diagnostic_from_analysis_error, check_span, CheckDiagnostic, CheckFileStatus,
};
use super::outcome::CommandOutcome;
use super::ChangedLintScope;

pub(crate) const LINT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub(crate) struct LintJsonCommandOutcome {
    pub envelope: JsonEnvelope<LintReport>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LintJsonOptions {
    pub strict: bool,
    pub require_file_header: bool,
    pub require_public_api_types: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LintReport {
    pub files: Vec<LintFileReport>,
    pub summary: LintSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<ChangedLintScope>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LintFileReport {
    pub path: String,
    pub status: CheckFileStatus,
    pub diagnostics: Vec<CheckDiagnostic>,
    /// Number of autofix edits that *would* apply if `--fix` were set
    /// (or that did apply, when `--fix` was set). Mirrors the same
    /// field on the human-readable output.
    pub fixable: usize,
    /// Number of edits actually applied by `--fix`. Always zero when
    /// `--fix` is not set.
    pub fixed: usize,
    #[serde(skip)]
    pub(crate) fixable_diagnostics: Vec<usize>,
}

impl LintFileReport {
    pub(crate) fn outcome(&self) -> CommandOutcome {
        CommandOutcome {
            has_error: matches!(self.status, CheckFileStatus::Error),
            has_warning: matches!(self.status, CheckFileStatus::Warning),
            findings: self.diagnostics.len(),
            fixable: self.fixable,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct LintSummary {
    pub ok: usize,
    pub warnings: usize,
    pub errors: usize,
    pub diagnostics: usize,
    pub fixable: usize,
    pub fixed: usize,
}

impl LintReport {
    pub(crate) fn from_files(files: Vec<LintFileReport>) -> Self {
        let mut summary = LintSummary::default();
        for file in &files {
            match file.status {
                CheckFileStatus::Ok => summary.ok += 1,
                CheckFileStatus::Warning => summary.warnings += 1,
                CheckFileStatus::Error => summary.errors += 1,
            }
            summary.diagnostics += file.diagnostics.len();
            summary.fixable += file.fixable;
            summary.fixed += file.fixed;
        }
        Self {
            files,
            summary,
            changed: None,
        }
    }
}

/// Execute the structured lint command without rendering or terminating the
/// process. JSON mode is report-only: it never applies `--fix` edits.
pub(crate) async fn run_lint_json(
    files: &[PathBuf],
    options: LintJsonOptions,
) -> LintJsonCommandOutcome {
    let mut analysis = AnalysisDatabase::new();
    let module_graph = super::build_module_graph_and_seed_analysis(files, &mut analysis);
    let cross_file_imports = super::collect_cross_file_imports(&module_graph);
    let script_rule_diags = super::run_project_script_rules(files).await;
    let mut should_fail = false;
    let mut reports = Vec::with_capacity(files.len());

    for file in files {
        let mut config = crate::package::load_check_config(Some(file));
        let mut lint_config = super::load_harn_lint_config(file);
        lint_config.require_file_header |= options.require_file_header;
        lint_config.require_public_api_types |= options.require_public_api_types;
        super::apply_loaded_harn_lint_config(&lint_config, &mut config);
        let script_diagnostics = script_rule_diags
            .get(file)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let report = lint_file_report(
            &mut analysis,
            file,
            &config,
            &cross_file_imports,
            &module_graph,
            &lint_config,
            script_diagnostics,
        );
        should_fail |= report
            .outcome()
            .should_fail(config.strict || options.strict);
        reports.push(report);
    }

    let report = LintReport::from_files(reports);
    let envelope = if should_fail {
        JsonEnvelope {
            schema_version: LINT_SCHEMA_VERSION,
            ok: false,
            data: Some(report),
            error: Some(JsonError {
                code: "lint_failed".to_string(),
                message: "one or more files failed `harn lint`".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    } else {
        JsonEnvelope::ok(LINT_SCHEMA_VERSION, report)
    };

    LintJsonCommandOutcome {
        envelope,
        exit_code: i32::from(should_fail),
    }
}

/// Lint one `.harn` file and return a structured report. Mirrors
/// [`super::lint::lint_file_inner`] but suppresses human-readable
/// stderr rendering and captures every diagnostic into a serializable
/// shape.
pub(crate) fn lint_file_report(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    lint_config: &super::config::HarnLintConfig,
    script_rule_diagnostics: &[harn_lint::LintDiagnostic],
) -> LintFileReport {
    let path_str = path.to_string_lossy().into_owned();
    let output = match analyze_file(analysis, path, config, module_graph) {
        Ok(output) => output,
        Err(error) => return file_analysis_error_report(path_str, error),
    };
    let source = output.source;
    let program = output.program;

    let engine_rules = super::lint::project_engine_rule_sources(path);
    let native_rule_paths = super::lint::project_native_rule_paths(path);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header: lint_config.require_file_header,
        require_docstrings: lint_config.require_docstrings,
        require_public_api_types: lint_config.require_public_api_types,
        complexity_threshold: lint_config.complexity_threshold,
        persona_step_allowlist: &lint_config.persona_step_allowlist,
        require_stdlib_metadata: harn_lint::path_is_stdlib_source(path),
        engine_rules: &engine_rules,
        native_rule_paths: &native_rule_paths,
        severity_overrides: lint_config.severity_overrides.clone(),
    };
    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );
    let type_lint_diagnostics = harn_lint::lint_diagnostics_from_type_diagnostics(
        &output.diagnostics,
        &config.disable_rules,
    );

    let mut has_error = false;
    let mut has_warning = false;
    let mut fixable = 0usize;
    let mut fixable_diagnostics = Vec::new();
    let mut diagnostics: Vec<CheckDiagnostic> = Vec::new();

    for diag in &lint_diagnostics {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        if diag.fix.is_some() {
            fixable += 1;
            fixable_diagnostics.push(diagnostics.len());
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    for diag in &type_lint_diagnostics {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        if diag.fix.is_some() {
            fixable += 1;
            fixable_diagnostics.push(diagnostics.len());
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    // `.harn`-authored custom lint rules (#2850), pre-computed in the async
    // command handler (they need the VM) and merged into the report so they
    // appear and affect status exactly like built-in rules.
    for diag in script_rule_diagnostics.iter().filter(|d| {
        !config
            .disable_rules
            .iter()
            .any(|r| r.as_str() == d.rule.as_ref())
    }) {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    let status = if has_error {
        CheckFileStatus::Error
    } else if has_warning {
        CheckFileStatus::Warning
    } else {
        CheckFileStatus::Ok
    };

    LintFileReport {
        path: path_str,
        status,
        diagnostics,
        fixable,
        fixed: 0,
        fixable_diagnostics,
    }
}

fn lint_severity_label(severity: LintSeverity) -> &'static str {
    match severity {
        LintSeverity::Info => "info",
        LintSeverity::Error => "error",
        LintSeverity::Warning => "warning",
    }
}

fn file_analysis_error_report(path: String, error: FileAnalysisError) -> LintFileReport {
    let diagnostic = match error {
        FileAnalysisError::Read(error) => CheckDiagnostic {
            source: "io",
            severity: "error",
            code: None,
            message: format!("Error reading {path}: {error}"),
            span: None,
            help: None,
        },
        FileAnalysisError::Analysis(error) => check_diagnostic_from_analysis_error(error),
    };
    LintFileReport {
        path,
        status: CheckFileStatus::Error,
        diagnostics: vec![diagnostic],
        fixable: 0,
        fixed: 0,
        fixable_diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_untyped_api(root: &Path) -> PathBuf {
        let path = root.join("api.harn");
        std::fs::write(
            &path,
            "pub fn run(value) { return value }\npub pipeline deploy(task) { return task }\n",
        )
        .expect("write API fixture");
        path
    }

    fn public_api_diagnostics(outcome: &LintJsonCommandOutcome) -> Vec<&CheckDiagnostic> {
        outcome.envelope.data.as_ref().expect("lint report").files[0]
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_deref() == Some("HARN-LNT-067"))
            .collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn public_api_type_command_override_emits_structured_diagnostics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = write_untyped_api(temp.path());

        let outcome = run_lint_json(
            &[file],
            LintJsonOptions {
                require_public_api_types: true,
                ..LintJsonOptions::default()
            },
        )
        .await;

        assert_eq!(outcome.exit_code, 0, "warnings remain advisory");
        assert!(outcome.envelope.ok);
        assert!(outcome.envelope.error.is_none());
        let diagnostics = public_api_diagnostics(&outcome);
        assert_eq!(diagnostics.len(), 4, "envelope: {:#?}", outcome.envelope);
        assert!(diagnostics
            .iter()
            .all(|diagnostic| diagnostic.source == "lint"));
        assert!(diagnostics
            .iter()
            .all(|diagnostic| diagnostic.span.is_some()));
        serde_json::to_value(&outcome.envelope).expect("lint envelope serializes");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn public_api_type_project_policy_and_severity_fail_without_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = write_untyped_api(temp.path());
        std::fs::write(
            temp.path().join("harn.toml"),
            r#"
[lint]
require-public-api-types = true

[lint.severity]
missing-public-api-type = "error"
"#,
        )
        .expect("write project policy");

        let outcome = run_lint_json(&[file], LintJsonOptions::default()).await;

        assert_eq!(outcome.exit_code, 1);
        assert!(!outcome.envelope.ok);
        assert_eq!(
            outcome
                .envelope
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("lint_failed")
        );
        let diagnostics = public_api_diagnostics(&outcome);
        assert_eq!(diagnostics.len(), 4, "envelope: {:#?}", outcome.envelope);
        assert!(diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == "error"));
        serde_json::to_value(&outcome.envelope).expect("lint envelope serializes");
    }
}
