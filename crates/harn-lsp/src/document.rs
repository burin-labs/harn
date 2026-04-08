use harn_lexer::Lexer;
use harn_parser::{Parser, SNode, TypeChecker};
use tower_lsp::lsp_types::*;

use crate::helpers::{lexer_error_to_diagnostic, parser_error_to_diagnostic, span_to_range};
use crate::symbols::{build_symbol_table, SymbolInfo};

// ---------------------------------------------------------------------------
// Document state: caches parse results per file
// ---------------------------------------------------------------------------

pub(crate) struct DocumentState {
    pub(crate) source: String,
    pub(crate) ast: Option<Vec<SNode>>,
    pub(crate) symbols: Vec<SymbolInfo>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) lint_diagnostics: Vec<harn_lint::LintDiagnostic>,
    pub(crate) type_diagnostics: Vec<harn_parser::TypeDiagnostic>,
    pub(crate) inlay_hints: Vec<harn_parser::InlayHintInfo>,
}

impl DocumentState {
    pub(crate) fn new(source: String) -> Self {
        let mut state = Self {
            source,
            ast: None,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            lint_diagnostics: Vec::new(),
            type_diagnostics: Vec::new(),
            inlay_hints: Vec::new(),
        };
        state.reparse();
        state
    }

    pub(crate) fn update(&mut self, source: String) {
        self.source = source;
        self.reparse();
    }

    fn reparse(&mut self) {
        self.diagnostics.clear();
        self.lint_diagnostics.clear();
        self.type_diagnostics.clear();
        self.inlay_hints.clear();
        self.symbols.clear();
        self.ast = None;

        // Lex
        let mut lexer = Lexer::new(&self.source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(e) => {
                self.diagnostics.push(lexer_error_to_diagnostic(&e));
                return;
            }
        };

        // Parse (with error recovery — report all errors)
        let mut parser = Parser::new(tokens);
        let program = match parser.parse() {
            Ok(p) => p,
            Err(_) => {
                for e in parser.all_errors() {
                    self.diagnostics.push(parser_error_to_diagnostic(e));
                }
                return;
            }
        };

        // Type check (with source for autofix generation and inlay hints)
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

        // Lint
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

        // Build symbol table
        self.symbols = build_symbol_table(&program, &self.source);
        self.ast = Some(program);
    }
}
