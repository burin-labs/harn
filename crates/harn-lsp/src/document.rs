use harn_parser::analysis::{
    AnalysisDatabase, AnalysisError, SourceId, SourceVersion, TypeCheckConfig,
};
use harn_parser::SNode;
use tower_lsp::lsp_types::*;

use crate::helpers::{
    diagnostic_data_value, lexer_error_to_diagnostic, parser_error_to_diagnostic, span_to_range,
};
use crate::rules::{RuleDiagnostic, RuleWorkspace};
use crate::symbols::{build_symbol_table, SymbolInfo};

pub(crate) struct DocumentState {
    pub(crate) source: String,
    pub(crate) language_id: String,
    analysis: AnalysisDatabase,
    source_id: SourceId,
    version: SourceVersion,
    pub(crate) cached_ast: Option<Vec<SNode>>,
    pub(crate) symbols: Vec<SymbolInfo>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) lint_diagnostics: Vec<harn_lint::LintDiagnostic>,
    pub(crate) type_diagnostics: Vec<harn_parser::TypeDiagnostic>,
    pub(crate) rule_diagnostics: Vec<RuleDiagnostic>,
    pub(crate) invariant_diagnostics: Vec<harn_ir::InvariantDiagnostic>,
    pub(crate) inlay_hints: Vec<harn_parser::InlayHintInfo>,
    pub(crate) dirty: bool,
}

impl DocumentState {
    pub(crate) fn new(source: String) -> Self {
        let mut state = Self::new_unparsed(source, "harn");
        state.reparse_if_dirty();
        state
    }

    pub(crate) fn new_for_language_with_rules(
        source: String,
        language_id: impl Into<String>,
        uri: &Url,
        rule_workspace: &RuleWorkspace,
    ) -> Self {
        let mut state = Self::new_unparsed(source, language_id);
        state.reparse_if_dirty_with_rules(Some(uri), Some(rule_workspace));
        state
    }

    fn new_unparsed(source: String, language_id: impl Into<String>) -> Self {
        let language_id = language_id.into();
        let mut analysis = AnalysisDatabase::new();
        let source_id = SourceId::new("document");
        analysis.set_source(source_id.clone(), source.clone(), SourceVersion(1));
        Self {
            source,
            language_id,
            analysis,
            source_id,
            version: SourceVersion(1),
            cached_ast: None,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            lint_diagnostics: Vec::new(),
            type_diagnostics: Vec::new(),
            rule_diagnostics: Vec::new(),
            invariant_diagnostics: Vec::new(),
            inlay_hints: Vec::new(),
            dirty: true,
        }
    }

    pub(crate) fn update_source(&mut self, source: String) {
        self.source = source;
        self.version = SourceVersion(self.version.0 + 1);
        self.analysis
            .set_source(self.source_id.clone(), self.source.clone(), self.version);
        self.dirty = true;
    }

    pub(crate) fn reparse_if_dirty(&mut self) {
        self.reparse_if_dirty_with_rules(None, None);
    }

    pub(crate) fn reparse_if_dirty_with_rules(
        &mut self,
        uri: Option<&Url>,
        rule_workspace: Option<&RuleWorkspace>,
    ) {
        if !self.dirty {
            return;
        }

        self.diagnostics.clear();
        self.lint_diagnostics.clear();
        self.type_diagnostics.clear();
        self.rule_diagnostics.clear();
        self.invariant_diagnostics.clear();
        self.inlay_hints.clear();
        self.symbols.clear();
        self.cached_ast = None;

        if self.language_id != "harn" {
            self.append_rule_diagnostics(uri, rule_workspace);
            self.dirty = false;
            return;
        }

        let analysis = match self
            .analysis
            .typecheck(&self.source_id, TypeCheckConfig::new())
        {
            Ok(analysis) => analysis,
            Err(error) => {
                match error {
                    AnalysisError::Lex { error, .. } => {
                        self.diagnostics.push(lexer_error_to_diagnostic(&error));
                    }
                    AnalysisError::Parse { errors, .. } => {
                        for error in &errors {
                            self.diagnostics.push(parser_error_to_diagnostic(error));
                        }
                    }
                    AnalysisError::MissingSource(_) => {}
                }
                self.append_rule_diagnostics(uri, rule_workspace);
                self.dirty = false;
                return;
            }
        };
        let program = analysis.program;
        let type_diags = analysis.diagnostics;
        self.inlay_hints = analysis.inlay_hints;
        for diag in &type_diags {
            let severity = match diag.severity {
                harn_parser::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
                harn_parser::DiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
            };
            let range = if let Some(span) = &diag.span {
                span_to_range(span)
            } else {
                Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 1),
                }
            };
            self.diagnostics.push(Diagnostic {
                range,
                severity: Some(severity),
                source: Some("harn-typecheck".to_string()),
                code: Some(NumberOrString::String(diag.code.to_string())),
                message: diag.message.clone(),
                data: Some(diagnostic_data_value(
                    diag.code.to_string(),
                    diag.repair.as_ref(),
                )),
                ..Default::default()
            });
        }
        self.type_diagnostics = type_diags;

        let invariant_report = harn_ir::analyze_program(&program);
        for diag in &invariant_report.diagnostics {
            let range = span_to_range(&diag.span);
            self.diagnostics.push(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("harn-invariant".to_string()),
                message: format!("[{}] {}", diag.invariant, diag.message),
                ..Default::default()
            });
        }
        self.invariant_diagnostics = invariant_report.diagnostics;

        let file_path = uri.and_then(|uri| uri.to_file_path().ok());
        let is_stdlib_source = file_path
            .as_deref()
            .is_some_and(harn_lint::path_is_stdlib_source);
        let require_public_api_types = file_path
            .as_deref()
            .and_then(|path| harn_modules::project_config::load_for_path(path).ok())
            .and_then(|config| config.lint.require_public_api_types)
            .unwrap_or(false);
        let lint_options = harn_lint::LintOptions {
            file_path: file_path.as_deref(),
            require_stdlib_metadata: is_stdlib_source,
            require_public_api_types,
            ..Default::default()
        };
        let externally_imported_names = std::collections::HashSet::new();
        let lint_diags = harn_lint::lint_with_options(
            &program,
            &[],
            Some(&self.source),
            &externally_imported_names,
            &lint_options,
        );
        for ld in &lint_diags {
            let severity = match ld.severity {
                harn_lint::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
                harn_lint::LintSeverity::Warning => DiagnosticSeverity::WARNING,
                harn_lint::LintSeverity::Error => DiagnosticSeverity::ERROR,
            };
            let range = span_to_range(&ld.span);
            let lint_repair = ld.repair();
            self.diagnostics.push(Diagnostic {
                range,
                severity: Some(severity),
                source: Some("harn-lint".to_string()),
                code: Some(NumberOrString::String(ld.code.to_string())),
                message: format!("[{}] {}", ld.rule, ld.message),
                data: Some(diagnostic_data_value(
                    ld.code.to_string(),
                    lint_repair.as_ref(),
                )),
                ..Default::default()
            });
        }
        self.lint_diagnostics = lint_diags;

        self.symbols = build_symbol_table(&program, &self.source);
        self.cached_ast = Some(program);
        self.append_rule_diagnostics(uri, rule_workspace);
        self.dirty = false;
    }

    fn append_rule_diagnostics(
        &mut self,
        uri: Option<&Url>,
        rule_workspace: Option<&RuleWorkspace>,
    ) {
        let (Some(uri), Some(rule_workspace)) = (uri, rule_workspace) else {
            return;
        };
        self.rule_diagnostics =
            rule_workspace.diagnostics_for_document(uri, &self.language_id, &self.source);
        self.diagnostics.extend(
            self.rule_diagnostics
                .iter()
                .map(|item| item.diagnostic.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentState;
    use crate::rules::RuleWorkspace;
    use std::path::Path;
    use tower_lsp::lsp_types::Url;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn update_source_marks_document_dirty_until_reparse() {
        let mut state = DocumentState::new("pipeline default(task) { log(1) }\n".to_string());
        assert!(!state.dirty, "fresh parse should clear dirty flag");
        assert!(
            state.cached_ast.is_some(),
            "fresh parse should cache the AST"
        );

        state.update_source("pipeline default(task) { let = }\n".to_string());
        assert!(state.dirty, "source update should mark the document dirty");
        assert!(
            state.cached_ast.is_some(),
            "cached AST should remain available until debounce reparses"
        );

        state.reparse_if_dirty();
        assert!(!state.dirty, "reparse should clear dirty flag");
        assert!(
            !state.diagnostics.is_empty(),
            "invalid source should produce diagnostics after reparse"
        );
    }

    #[test]
    fn unchanged_document_reuses_analysis_cache() {
        let mut state = DocumentState::new("pipeline default(task) { log(1) }\n".to_string());
        let initial = state.analysis.stats();

        state.update_source("pipeline default(task) { log(1) }\n".to_string());
        state.reparse_if_dirty();

        let after = state.analysis.stats();
        assert_eq!(after.lex_runs, initial.lex_runs);
        assert_eq!(after.parse_runs, initial.parse_runs);
        assert_eq!(after.typecheck_runs, initial.typecheck_runs);
    }

    #[test]
    fn invariant_violations_surface_as_lsp_diagnostics() {
        let state = DocumentState::new(
            r#"
@invariant("approval.reachability")
fn handler() {
  write_file("src/main.rs", "unsafe")
}
"#
            .to_string(),
        );

        assert!(
            state
                .diagnostics
                .iter()
                .any(|diag| diag.source.as_deref() == Some("harn-invariant")),
            "expected invariant diagnostics, got {:?}",
            state
                .diagnostics
                .iter()
                .map(|diag| (&diag.source, &diag.message))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stdlib_return_type_lint_surfaces_as_lsp_diagnostic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join("crates/harn-stdlib/src/stdlib/stdlib_demo.harn");
        write(&path, "pub fn missing_contract() {\n  return 1\n}\n");
        let workspace = RuleWorkspace::from_root(temp.path());
        let uri = Url::from_file_path(&path).unwrap();
        let state = DocumentState::new_for_language_with_rules(
            "pub fn missing_contract() {\n  return 1\n}\n".to_string(),
            "harn",
            &uri,
            &workspace,
        );

        assert!(
            state.diagnostics.iter().any(|diag| {
                matches!(
                    diag.code.as_ref(),
                    Some(tower_lsp::lsp_types::NumberOrString::String(code)) if code == "HARN-STD-102"
                )
            }),
            "expected HARN-STD-102 LSP diagnostic, got {:?}",
            state
                .diagnostics
                .iter()
                .map(|diag| (&diag.code, &diag.message))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn project_public_api_type_policy_surfaces_as_lsp_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("harn.toml"),
            "[lint]\nrequire_public_api_types = true\n",
        );
        let path = temp.path().join("src/main.harn");
        let source = "pub pipeline ship(task) {\n  return task\n}\n";
        write(&path, source);
        let workspace = RuleWorkspace::from_root(temp.path());
        let uri = Url::from_file_path(&path).unwrap();
        let state = DocumentState::new_for_language_with_rules(
            source.to_string(),
            "harn",
            &uri,
            &workspace,
        );

        let public_api_diagnostics = state
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.code.as_ref(),
                    Some(tower_lsp::lsp_types::NumberOrString::String(code))
                        if code == "HARN-LNT-067"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(public_api_diagnostics.len(), 2);
        assert!(public_api_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("parameter `task`")));
        assert!(public_api_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("return type")));
    }

    #[test]
    fn non_harn_documents_run_rules_without_harn_parse_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("harn.toml"),
            "[rules]\nruleDirs = [\"rules\"]\n",
        );
        write(
            &temp.path().join("rules/no-debugger.toml"),
            r#"
id = "no-debugger"
language = "typescript"
message = "remove debugger statements"
severity = "warning"
safety = "behavior-preserving"
fix = ""

[rule]
regex = "debugger;"
"#,
        );

        let workspace = RuleWorkspace::from_root(temp.path());
        let uri = Url::from_file_path(temp.path().join("src/main.ts")).unwrap();
        let state = DocumentState::new_for_language_with_rules(
            "function f() { debugger; }\n".to_string(),
            "typescript",
            &uri,
            &workspace,
        );

        assert!(
            state.cached_ast.is_none(),
            "TypeScript should not parse as Harn"
        );
        assert_eq!(state.rule_diagnostics.len(), 1);
        assert_eq!(state.diagnostics.len(), 1);
        assert_eq!(state.diagnostics[0].source.as_deref(), Some("harn-rules"));
    }

    #[test]
    fn typecheck_diagnostics_carry_repair_data_envelope() {
        // A `let x = 1; x = 2` reassigns an immutable binding —
        // HARN-OWN-001 with a `bindings/make-mutable` repair. The
        // LSP-side code-action provider reads the safety class from
        // `Diagnostic.data` to decide whether to auto-apply.
        let state =
            DocumentState::new("pipeline main() {\n  const x = 1\n  x = 2\n}\n".to_string());
        let diag = state
            .diagnostics
            .iter()
            .find(|d| {
                matches!(
                    d.code.as_ref(),
                    Some(tower_lsp::lsp_types::NumberOrString::String(code)) if code == "HARN-OWN-001"
                )
            })
            .expect("expected ImmutableAssignment diagnostic");
        let data = diag.data.as_ref().expect("repair data should be attached");
        assert_eq!(
            data.get("code").and_then(|v| v.as_str()),
            Some("HARN-OWN-001")
        );
        assert_eq!(
            data.get("repair_id").and_then(|v| v.as_str()),
            Some("bindings/make-mutable")
        );
        let repair = data.get("repair").expect("data.repair should be present");
        assert_eq!(
            repair.get("id").and_then(|v| v.as_str()),
            Some("bindings/make-mutable")
        );
        assert_eq!(
            repair.get("safety").and_then(|v| v.as_str()),
            Some("scope-local")
        );
    }
}
