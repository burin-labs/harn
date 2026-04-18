use std::collections::HashSet;

use crate::ast::*;
use crate::builtin_signatures;
use harn_lexer::{FixEdit, Span};

mod binary_ops;
mod exits;
mod format;
mod inference;
mod schema_inference;
mod scope;
mod union;

pub use exits::{block_definitely_exits, stmt_definitely_exits};
pub use format::{format_type, shape_mismatch_detail};

use schema_inference::schema_type_expr_from_node;
use scope::TypeScope;

/// An inlay hint produced during type checking.
#[derive(Debug, Clone)]
pub struct InlayHintInfo {
    /// Position (line, column) where the hint should be displayed (after the variable name).
    pub line: usize,
    pub column: usize,
    /// The type label to display (e.g. ": string").
    pub label: String,
}

/// A diagnostic produced by the type checker.
#[derive(Debug, Clone)]
pub struct TypeDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Option<Span>,
    pub help: Option<String>,
    /// Machine-applicable fix edits.
    pub fix: Option<Vec<FixEdit>>,
    /// Optional structured payload that higher-level tooling (e.g. the
    /// LSP code-action provider) can consume to synthesise fixes that
    /// need more than a static `FixEdit`. Out-of-band from `fix` so the
    /// string-based rendering pipeline doesn't have to care.
    pub details: Option<DiagnosticDetails>,
}

/// Optional structured companion data on a `TypeDiagnostic`. The
/// variants map one-to-one with diagnostics that have specific
/// tooling-consumable state beyond the human-readable message; each
/// variant is attached only by the sites that produce its
/// corresponding diagnostic, so a consumer can pattern-match on the
/// variant without parsing the error string.
#[derive(Debug, Clone)]
pub enum DiagnosticDetails {
    /// A `match` expression with missing variant coverage. `missing`
    /// holds the formatted literal values of each uncovered variant
    /// (quoted for strings, bare for ints), ready to drop into a new
    /// arm prefix. The diagnostic's `span` covers the whole `match`
    /// expression, so a code-action can locate the closing `}` by
    /// reading the source at `span.end`.
    NonExhaustiveMatch { missing: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// The static type checker.
pub struct TypeChecker {
    diagnostics: Vec<TypeDiagnostic>,
    scope: TypeScope,
    source: Option<String>,
    hints: Vec<InlayHintInfo>,
    /// When true, flag unvalidated boundary-API values used in field access.
    strict_types: bool,
    /// Lexical depth of enclosing function-like bodies (fn/tool/pipeline/closure).
    /// `try*` requires `fn_depth > 0` so the rethrow has a body to live in.
    fn_depth: usize,
    /// Maps function name -> deprecation metadata `(since, use_hint)`. Populated
    /// when an `@deprecated` attribute is encountered on a top-level fn decl
    /// during the `check_inner` pre-pass; consulted at every `FunctionCall`
    /// site to emit a warning + help line.
    deprecated_fns: std::collections::HashMap<String, (Option<String>, Option<String>)>,
    /// Names statically known to be introduced by cross-module imports
    /// (resolved via `harn-modules`). `Some(set)` switches the checker into
    /// strict cross-module mode: an unresolved callable name is reported as
    /// an error instead of silently passing through. `None` preserves the
    /// conservative pre-v0.7.12 behavior (no cross-module undefined-name
    /// diagnostics).
    imported_names: Option<HashSet<String>>,
}

impl TypeChecker {
    pub(in crate::typechecker) fn wildcard_type() -> TypeExpr {
        TypeExpr::Named("_".into())
    }

    pub(in crate::typechecker) fn is_wildcard_type(ty: &TypeExpr) -> bool {
        matches!(ty, TypeExpr::Named(name) if name == "_")
    }

    pub(in crate::typechecker) fn base_type_name(ty: &TypeExpr) -> Option<&str> {
        match ty {
            TypeExpr::Named(name) => Some(name.as_str()),
            TypeExpr::Applied { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            scope: TypeScope::new(),
            source: None,
            hints: Vec::new(),
            strict_types: false,
            fn_depth: 0,
            deprecated_fns: std::collections::HashMap::new(),
            imported_names: None,
        }
    }

    /// Create a type checker with strict types mode.
    /// When enabled, flags unvalidated boundary-API values used in field access.
    pub fn with_strict_types(strict: bool) -> Self {
        Self {
            diagnostics: Vec::new(),
            scope: TypeScope::new(),
            source: None,
            hints: Vec::new(),
            strict_types: strict,
            fn_depth: 0,
            deprecated_fns: std::collections::HashMap::new(),
            imported_names: None,
        }
    }

    /// Attach the set of names statically introduced by cross-module imports.
    ///
    /// Enables strict cross-module undefined-call errors: call sites that are
    /// not builtins, not local declarations, not struct constructors, not
    /// callable variables, and not in `imported` will produce a type error.
    ///
    /// Passing `None` (the default) preserves pre-v0.7.12 behavior where
    /// unresolved call names only surface via lint diagnostics. Callers
    /// should only pass `Some(set)` when every import in the file resolved
    /// — see `harn_modules::ModuleGraph::imported_names_for_file`.
    pub fn with_imported_names(mut self, imported: HashSet<String>) -> Self {
        self.imported_names = Some(imported);
        self
    }

    /// Check a program with source text for autofix generation.
    pub fn check_with_source(mut self, program: &[SNode], source: &str) -> Vec<TypeDiagnostic> {
        self.source = Some(source.to_string());
        self.check_inner(program).0
    }

    /// Check a program with strict types mode and source text.
    pub fn check_strict_with_source(
        mut self,
        program: &[SNode],
        source: &str,
    ) -> Vec<TypeDiagnostic> {
        self.source = Some(source.to_string());
        self.check_inner(program).0
    }

    /// Check a program and return diagnostics.
    pub fn check(self, program: &[SNode]) -> Vec<TypeDiagnostic> {
        self.check_inner(program).0
    }

    /// Check whether a function call value is a boundary source that produces
    /// unvalidated data.  Returns `None` if the value is type-safe
    /// (e.g. llm_call with a schema option, or a non-boundary function).
    pub(in crate::typechecker) fn detect_boundary_source(
        value: &SNode,
        scope: &TypeScope,
    ) -> Option<String> {
        match &value.node {
            Node::FunctionCall { name, args } => {
                if !builtin_signatures::is_untyped_boundary_source(name) {
                    return None;
                }
                // llm_call/llm_completion with a schema option are type-safe
                if (name == "llm_call" || name == "llm_completion")
                    && Self::llm_call_has_typed_schema_option(args, scope)
                {
                    return None;
                }
                Some(name.clone())
            }
            Node::Identifier(name) => scope.is_untyped_source(name).map(|s| s.to_string()),
            _ => None,
        }
    }

    /// True if an `llm_call` / `llm_completion` options dict names a
    /// resolvable output schema. Used by the strict-types boundary checks
    /// to suppress "unvalidated" warnings when the call site is typed.
    /// Actual return-type narrowing is driven by the generic-builtin
    /// dispatch path in `infer_type`, not this helper.
    pub(in crate::typechecker) fn llm_call_has_typed_schema_option(
        args: &[SNode],
        scope: &TypeScope,
    ) -> bool {
        let Some(opts) = args.get(2) else {
            return false;
        };
        let Node::DictLiteral(entries) = &opts.node else {
            return false;
        };
        entries.iter().any(|entry| {
            let key = match &entry.key.node {
                Node::StringLiteral(k) | Node::Identifier(k) => k.as_str(),
                _ => return false,
            };
            (key == "schema" || key == "output_schema")
                && schema_type_expr_from_node(&entry.value, scope).is_some()
        })
    }

    /// Check whether a type annotation is a concrete shape/struct type
    /// (as opposed to bare `dict` or no annotation).
    pub(in crate::typechecker) fn is_concrete_type(ty: &TypeExpr) -> bool {
        matches!(
            ty,
            TypeExpr::Shape(_)
                | TypeExpr::Applied { .. }
                | TypeExpr::FnType { .. }
                | TypeExpr::List(_)
                | TypeExpr::Iter(_)
                | TypeExpr::DictType(_, _)
        ) || matches!(ty, TypeExpr::Named(n) if n != "dict" && n != "any" && n != "_")
    }

    /// Check a program and return both diagnostics and inlay hints.
    pub fn check_with_hints(
        mut self,
        program: &[SNode],
        source: &str,
    ) -> (Vec<TypeDiagnostic>, Vec<InlayHintInfo>) {
        self.source = Some(source.to_string());
        self.check_inner(program)
    }

    pub(in crate::typechecker) fn error_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: None,
            details: None,
        });
    }

    #[allow(dead_code)]
    pub(in crate::typechecker) fn error_at_with_help(
        &mut self,
        message: String,
        span: Span,
        help: String,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: Some(help),
            fix: None,
            details: None,
        });
    }

    pub(in crate::typechecker) fn error_at_with_fix(
        &mut self,
        message: String,
        span: Span,
        fix: Vec<FixEdit>,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: Some(fix),
            details: None,
        });
    }

    /// Diagnostic site for non-exhaustive `match` arms. Match arms must be
    /// exhaustive — a missing-variant `match` is a hard error. Authors who
    /// genuinely want partial coverage opt out with a wildcard `_` arm.
    /// Partial `if/elif/else` chains are intentionally allowed and are
    /// instead handled by `check_unknown_exhaustiveness`, which stays a
    /// warning so the `unreachable()` opt-in pattern continues to work.
    pub(in crate::typechecker) fn exhaustiveness_error_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: None,
            details: None,
        });
    }

    /// Like `exhaustiveness_error_at` but additionally attaches the
    /// missing-variant list as structured details. LSP code-actions
    /// read this to synthesise an "Add missing match arms" quick-fix
    /// without string-parsing the message.
    pub(in crate::typechecker) fn exhaustiveness_error_with_missing(
        &mut self,
        message: String,
        span: Span,
        missing: Vec<String>,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: None,
            details: Some(DiagnosticDetails::NonExhaustiveMatch { missing }),
        });
    }

    pub(in crate::typechecker) fn warning_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: None,
            fix: None,
            details: None,
        });
    }

    #[allow(dead_code)]
    pub(in crate::typechecker) fn warning_at_with_help(
        &mut self,
        message: String,
        span: Span,
        help: String,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: Some(help),
            fix: None,
            details: None,
        });
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
