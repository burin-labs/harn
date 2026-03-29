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
}

impl DocumentState {
    pub(crate) fn new(source: String) -> Self {
        let mut state = Self {
            source,
            ast: None,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
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

        // Type check
        let type_diags = TypeChecker::new().check(&program);
        for diag in type_diags {
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
                message: diag.message,
                ..Default::default()
            });
        }

        // Lint
        let lint_diags = harn_lint::lint(&program);
        for ld in lint_diags {
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

        // Build symbol table
        self.symbols = build_symbol_table(&program);
        self.ast = Some(program);
    }
}
