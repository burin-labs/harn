use harn_lexer::Lexer;
use harn_parser::{Parser, SNode, TypeChecker};
use tower_lsp::lsp_types::*;

use crate::helpers::{lexer_error_to_diagnostic, parser_error_to_diagnostic, span_to_range};
use crate::symbols::{build_symbol_table, SymbolInfo};

pub(crate) struct DocumentState {
    pub(crate) source: String,
    pub(crate) cached_ast: Option<Vec<SNode>>,
    pub(crate) symbols: Vec<SymbolInfo>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) lint_diagnostics: Vec<harn_lint::LintDiagnostic>,
    pub(crate) type_diagnostics: Vec<harn_parser::TypeDiagnostic>,
    pub(crate) invariant_diagnostics: Vec<harn_ir::InvariantDiagnostic>,
    pub(crate) inlay_hints: Vec<harn_parser::InlayHintInfo>,
    pub(crate) dirty: bool,
}

impl DocumentState {
    pub(crate) fn new(source: String) -> Self {
        let mut state = Self {
            source,
            cached_ast: None,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            lint_diagnostics: Vec::new(),
            type_diagnostics: Vec::new(),
            invariant_diagnostics: Vec::new(),
            inlay_hints: Vec::new(),
            dirty: true,
        };
        state.reparse_if_dirty();
        state
    }

    pub(crate) fn update_source(&mut self, source: String) {
        self.source = source;
        self.dirty = true;
    }

    pub(crate) fn reparse_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }

        self.diagnostics.clear();
        self.lint_diagnostics.clear();
        self.type_diagnostics.clear();
        self.invariant_diagnostics.clear();
        self.inlay_hints.clear();
        self.symbols.clear();
        self.cached_ast = None;

        let mut lexer = Lexer::new(&self.source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(e) => {
                self.diagnostics.push(lexer_error_to_diagnostic(&e));
                self.dirty = false;
                return;
            }
        };

        // Parse with recovery so every error surfaces, not just the first.
        let mut parser = Parser::new(tokens);
        let program = match parser.parse() {
            Ok(p) => p,
            Err(_) => {
                for e in parser.all_errors() {
                    self.diagnostics.push(parser_error_to_diagnostic(e));
                }
                self.dirty = false;
                return;
            }
        };

        // Source is required here so the checker can emit autofix text and inlay hints.
        let (type_diags, inlay_hints) = TypeChecker::new().check_with_hints(&program, &self.source);
        self.inlay_hints = inlay_hints;
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
                message: diag.message.clone(),
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

        let lint_diags = harn_lint::lint_with_source(&program, &self.source);
        for ld in &lint_diags {
            let severity = match ld.severity {
                harn_lint::LintSeverity::Warning => DiagnosticSeverity::WARNING,
                harn_lint::LintSeverity::Error => DiagnosticSeverity::ERROR,
            };
            let range = span_to_range(&ld.span);
            self.diagnostics.push(Diagnostic {
                range,
                severity: Some(severity),
                source: Some("harn-lint".to_string()),
                message: format!("[{}] {}", ld.rule, ld.message),
                ..Default::default()
            });
        }
        self.lint_diagnostics = lint_diags;

        self.symbols = build_symbol_table(&program, &self.source);
        self.cached_ast = Some(program);
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentState;

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
}
