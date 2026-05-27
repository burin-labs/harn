use std::collections::HashSet;
use std::rc::Rc;

use crate::ast::*;
use crate::builtin_signatures;
use crate::diagnostic_codes::{Code, Repair};
use harn_lexer::{FixEdit, Span};

type TypeMismatchEvidence = (Option<(Span, String)>, Option<Span>);

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
    pub code: Code,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Option<Span>,
    pub help: Option<String>,
    pub related: Vec<RelatedDiagnostic>,
    /// Machine-applicable fix edits.
    pub fix: Option<Vec<FixEdit>>,
    /// Optional structured payload that higher-level tooling (e.g. the
    /// LSP code-action provider) can consume to synthesise fixes that
    /// need more than a static `FixEdit`. Out-of-band from `fix` so the
    /// string-based rendering pipeline doesn't have to care.
    pub details: Option<DiagnosticDetails>,
    /// Structured repair classifier — id, summary, and safety class.
    /// Agents and IDEs dispatch on `repair.safety` to decide whether to
    /// auto-apply, propose, or escalate. `None` when no repair shape is
    /// registered for this code; populated automatically from
    /// [`Code::repair_template`] by the builder helpers.
    pub repair: Option<Repair>,
}

#[derive(Debug, Clone)]
pub struct RelatedDiagnostic {
    pub span: Span,
    pub message: String,
}

/// Optional structured companion data on a `TypeDiagnostic`. The
/// variants map one-to-one with diagnostics that have specific
/// tooling-consumable state beyond the human-readable message; each
/// variant is attached only by the sites that produce its
/// corresponding diagnostic, so a consumer can pattern-match on the
/// variant without parsing the error string.
#[derive(Debug, Clone)]
pub enum DiagnosticDetails {
    /// A concrete expected/found mismatch. Renderers can use this to
    /// provide stable labels without scraping human-readable text.
    TypeMismatch,
    /// A `match` expression with missing variant coverage. `missing`
    /// holds the formatted literal values of each uncovered variant
    /// (quoted for strings, bare for ints), ready to drop into a new
    /// arm prefix. The diagnostic's `span` covers the whole `match`
    /// expression, so a code-action can locate the closing `}` by
    /// reading the source at `span.end`.
    NonExhaustiveMatch { missing: Vec<String> },
    /// A type-aware lint diagnostic. These diagnostics are produced by
    /// the type checker because the rule depends on flow-sensitive type
    /// information, but `harn lint` should surface and filter them like
    /// ordinary lint rules.
    LintRule { rule: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// The static type checker.
pub struct TypeChecker {
    diagnostics: Vec<TypeDiagnostic>,
    /// Root scope shared by every child scope created during the walk.
    /// `Rc` lets fn/pipeline body entries take a refcount bump instead of
    /// deep-cloning the entire scope chain. Mutations during the pre-pass
    /// (and the top-level non-callable arm) go through `Rc::make_mut`,
    /// which is O(1) while the refcount is 1.
    scope: Rc<TypeScope>,
    source: Option<String>,
    hints: Vec<InlayHintInfo>,
    /// When true, flag unvalidated boundary-API values used in field access.
    strict_types: bool,
    /// Lexical depth of enclosing function-like bodies (fn/tool/pipeline/closure).
    /// `try*` requires `fn_depth > 0` so the rethrow has a body to live in.
    fn_depth: usize,
    /// Lexical depth of enclosing `gen fn` bodies. `emit` is only valid here.
    stream_fn_depth: usize,
    /// Expected emitted value type for each enclosing `gen fn`.
    stream_emit_types: Vec<Option<TypeExpr>>,
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
    /// Type-like declarations imported from other modules. These are registered
    /// into the scope before local checking so imported type aliases and tagged
    /// unions participate in normal field access and narrowing.
    imported_type_decls: Vec<SNode>,
    /// Callable declarations imported from other modules. Only their
    /// signatures are registered; bodies stay owned by the defining module.
    imported_callable_decls: Vec<SNode>,
    /// Compile-time environment populated by every successfully folded
    /// `const` binding. Later const initializers see earlier values so
    /// expressions like `const Y = X + 1` work.
    const_env: crate::const_eval::ConstEnv,
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
            scope: Rc::new(TypeScope::new()),
            source: None,
            hints: Vec::new(),
            strict_types: false,
            fn_depth: 0,
            stream_fn_depth: 0,
            stream_emit_types: Vec::new(),
            deprecated_fns: std::collections::HashMap::new(),
            imported_names: None,
            imported_type_decls: Vec::new(),
            imported_callable_decls: Vec::new(),
            const_env: crate::const_eval::ConstEnv::new(),
        }
    }

    /// Create a type checker with strict types mode.
    /// When enabled, flags unvalidated boundary-API values used in field access.
    pub fn with_strict_types(strict: bool) -> Self {
        Self {
            diagnostics: Vec::new(),
            scope: Rc::new(TypeScope::new()),
            source: None,
            hints: Vec::new(),
            strict_types: strict,
            fn_depth: 0,
            stream_fn_depth: 0,
            stream_emit_types: Vec::new(),
            deprecated_fns: std::collections::HashMap::new(),
            imported_names: None,
            imported_type_decls: Vec::new(),
            imported_callable_decls: Vec::new(),
            const_env: crate::const_eval::ConstEnv::new(),
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

    /// Attach imported type / struct / enum / interface declarations. The
    /// caller is responsible for resolving module imports and filtering the
    /// visible declarations before passing them in.
    pub fn with_imported_type_decls(mut self, imported: Vec<SNode>) -> Self {
        self.imported_type_decls = imported;
        self
    }

    /// Attach imported function / pipeline / tool declarations. The checker
    /// registers only call signatures so imported pure-Harn functions enforce
    /// their parameter annotations at the caller without checking the imported
    /// body in the caller's scope.
    pub fn with_imported_callable_decls(mut self, imported: Vec<SNode>) -> Self {
        self.imported_callable_decls = imported;
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
        self.strict_types = true;
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
            Node::FunctionCall { name, args, .. } => {
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
                | TypeExpr::Generator(_)
                | TypeExpr::Stream(_)
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

    pub(in crate::typechecker) fn error_at(&mut self, code: Code, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            related: Vec::new(),
            fix: None,
            details: None,
            repair: default_repair(code),
        });
    }

    #[allow(dead_code)]
    pub(in crate::typechecker) fn error_at_with_help(
        &mut self,
        code: Code,
        message: String,
        span: Span,
        help: String,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: Some(help),
            related: Vec::new(),
            fix: None,
            details: None,
            repair: default_repair(code),
        });
    }

    pub(in crate::typechecker) fn type_mismatch_at(
        &mut self,
        code: Code,
        context: impl Into<String>,
        expected: &TypeExpr,
        actual: &TypeExpr,
        span: Span,
        evidence: TypeMismatchEvidence,
        scope: &TypeScope,
    ) {
        let (expected_origin, value_span) = evidence;
        let nested_mismatch = first_nested_mismatch(expected, actual, scope);
        let mut message = format!(
            "{}: expected {}, found {}",
            context.into(),
            format_type(expected),
            format_type(actual)
        );
        if let Some(detail) = shape_mismatch_detail(expected, actual)
            .or_else(|| nested_mismatch.as_ref().map(|note| note.message.clone()))
        {
            message.push_str(&format!(" ({detail})"));
        }

        let mut related = Vec::new();
        if let Some((span, message)) = expected_origin {
            related.push(RelatedDiagnostic { span, message });
        }
        if let Some(note) = nested_mismatch {
            related.push(RelatedDiagnostic {
                span,
                message: format!("nested mismatch: {}", note.message),
            });
        }

        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: coercion_suggestion(expected, actual, value_span, self.source.as_deref()),
            related,
            fix: None,
            details: Some(DiagnosticDetails::TypeMismatch),
            repair: default_repair(code),
        });
    }

    pub(in crate::typechecker) fn error_at_with_fix(
        &mut self,
        code: Code,
        message: String,
        span: Span,
        fix: Vec<FixEdit>,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            related: Vec::new(),
            fix: Some(fix),
            details: None,
            repair: default_repair(code),
        });
    }

    /// Diagnostic site for non-exhaustive `match` arms. Match arms must be
    /// exhaustive — a missing-variant `match` is a hard error. Authors who
    /// genuinely want partial coverage opt out with a wildcard `_` arm.
    /// Partial `if/elif/else` chains are intentionally allowed and are
    /// instead handled by `check_unknown_exhaustiveness`, which stays a
    /// warning so the `unreachable()` opt-in pattern continues to work.
    pub(in crate::typechecker) fn exhaustiveness_error_at(
        &mut self,
        code: Code,
        message: String,
        span: Span,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            related: Vec::new(),
            fix: None,
            details: None,
            repair: default_repair(code),
        });
    }

    /// Like `exhaustiveness_error_at` but additionally attaches the
    /// missing-variant list as structured details. LSP code-actions
    /// read this to synthesise an "Add missing match arms" quick-fix
    /// without string-parsing the message.
    pub(in crate::typechecker) fn exhaustiveness_error_with_missing(
        &mut self,
        code: Code,
        message: String,
        span: Span,
        missing: Vec<String>,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            related: Vec::new(),
            fix: None,
            details: Some(DiagnosticDetails::NonExhaustiveMatch { missing }),
            repair: default_repair(code),
        });
    }

    pub(in crate::typechecker) fn warning_at(&mut self, code: Code, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: None,
            related: Vec::new(),
            fix: None,
            details: None,
            repair: default_repair(code),
        });
    }

    #[allow(dead_code)]
    pub(in crate::typechecker) fn warning_at_with_help(
        &mut self,
        code: Code,
        message: String,
        span: Span,
        help: String,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: Some(help),
            related: Vec::new(),
            fix: None,
            details: None,
            repair: default_repair(code),
        });
    }

    pub(in crate::typechecker) fn lint_warning_at_with_fix(
        &mut self,
        code: Code,
        rule: &'static str,
        message: String,
        span: Span,
        help: String,
        fix: Vec<FixEdit>,
    ) {
        self.diagnostics.push(TypeDiagnostic {
            code,
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: Some(help),
            related: Vec::new(),
            fix: Some(fix),
            details: Some(DiagnosticDetails::LintRule { rule }),
            repair: default_repair(code),
        });
    }
}

/// Materialize the default [`Repair`] for a diagnostic code, or `None`
/// if no static repair shape is registered. Cheap (one pointer
/// dereference plus an allocation for the summary string); call sites
/// pay nothing when the code has no repair template.
pub(crate) fn default_repair(code: Code) -> Option<Repair> {
    code.repair_template().map(Repair::from_template)
}

#[derive(Debug)]
struct MismatchNote {
    message: String,
}

fn first_nested_mismatch(
    expected: &TypeExpr,
    actual: &TypeExpr,
    scope: &TypeScope,
) -> Option<MismatchNote> {
    let expected = resolve_type_for_diagnostic(expected, scope);
    let actual = resolve_type_for_diagnostic(actual, scope);
    match (&expected, &actual) {
        (TypeExpr::Shape(expected_fields), TypeExpr::Shape(actual_fields)) => {
            for expected_field in expected_fields {
                if expected_field.optional {
                    continue;
                }
                let Some(actual_field) = actual_fields
                    .iter()
                    .find(|actual_field| actual_field.name == expected_field.name)
                else {
                    return Some(MismatchNote {
                        message: format!(
                            "field `{}` is missing; expected {}",
                            expected_field.name,
                            format_type(&expected_field.type_expr)
                        ),
                    });
                };
                if !types_compatible_for_diagnostic(
                    &expected_field.type_expr,
                    &actual_field.type_expr,
                    scope,
                ) {
                    return Some(MismatchNote {
                        message: format!(
                            "field `{}` expected {}, found {}",
                            expected_field.name,
                            format_type(&expected_field.type_expr),
                            format_type(&actual_field.type_expr)
                        ),
                    });
                }
            }
            None
        }
        (TypeExpr::List(expected_inner), TypeExpr::List(actual_inner)) => {
            if !types_compatible_for_diagnostic(expected_inner, actual_inner, scope)
                || !types_compatible_for_diagnostic(actual_inner, expected_inner, scope)
            {
                Some(MismatchNote {
                    message: format!(
                        "list element expected {}, found {}",
                        format_type(expected_inner),
                        format_type(actual_inner)
                    ),
                })
            } else {
                None
            }
        }
        (
            TypeExpr::DictType(expected_key, expected_value),
            TypeExpr::DictType(actual_key, actual_value),
        ) => {
            if !types_compatible_for_diagnostic(expected_key, actual_key, scope)
                || !types_compatible_for_diagnostic(actual_key, expected_key, scope)
            {
                Some(MismatchNote {
                    message: format!(
                        "dict key expected {}, found {}",
                        format_type(expected_key),
                        format_type(actual_key)
                    ),
                })
            } else if !types_compatible_for_diagnostic(expected_value, actual_value, scope)
                || !types_compatible_for_diagnostic(actual_value, expected_value, scope)
            {
                Some(MismatchNote {
                    message: format!(
                        "dict value expected {}, found {}",
                        format_type(expected_value),
                        format_type(actual_value)
                    ),
                })
            } else {
                None
            }
        }
        (
            TypeExpr::Applied {
                name: expected_name,
                args: expected_args,
            },
            TypeExpr::Applied {
                name: actual_name,
                args: actual_args,
            },
        ) if expected_name == actual_name => expected_args
            .iter()
            .zip(actual_args.iter())
            .enumerate()
            .find_map(|(idx, (expected_arg, actual_arg))| {
                if types_compatible_for_diagnostic(expected_arg, actual_arg, scope)
                    && types_compatible_for_diagnostic(actual_arg, expected_arg, scope)
                {
                    None
                } else {
                    Some(MismatchNote {
                        message: format!(
                            "{} type argument {} expected {}, found {}",
                            expected_name,
                            idx + 1,
                            format_type(expected_arg),
                            format_type(actual_arg)
                        ),
                    })
                }
            }),
        (
            TypeExpr::FnType {
                params: expected_params,
                return_type: expected_return,
            },
            TypeExpr::FnType {
                params: actual_params,
                return_type: actual_return,
            },
        ) => {
            for (idx, (expected_param, actual_param)) in
                expected_params.iter().zip(actual_params.iter()).enumerate()
            {
                if !types_compatible_for_diagnostic(actual_param, expected_param, scope) {
                    return Some(MismatchNote {
                        message: format!(
                            "function parameter {} expected {}, found {}",
                            idx + 1,
                            format_type(expected_param),
                            format_type(actual_param)
                        ),
                    });
                }
            }
            if !types_compatible_for_diagnostic(expected_return, actual_return, scope) {
                Some(MismatchNote {
                    message: format!(
                        "function return expected {}, found {}",
                        format_type(expected_return),
                        format_type(actual_return)
                    ),
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn types_compatible_for_diagnostic(
    expected: &TypeExpr,
    actual: &TypeExpr,
    scope: &TypeScope,
) -> bool {
    TypeChecker::new().types_compatible(expected, actual, scope)
}

fn resolve_type_for_diagnostic(ty: &TypeExpr, scope: &TypeScope) -> TypeExpr {
    TypeChecker::new().resolve_alias(ty, scope)
}

fn coercion_suggestion(
    expected: &TypeExpr,
    actual: &TypeExpr,
    value_span: Option<Span>,
    source: Option<&str>,
) -> Option<String> {
    let expr = value_span
        .and_then(|span| source.and_then(|source| source.get(span.start..span.end)))
        .map(str::trim)
        .filter(|expr| !expr.is_empty());
    if is_nilable(actual) {
        return Some("handle `nil` first or provide a default with `??`".to_string());
    }
    let expected_ty = expected;
    let expected = simple_type_name(expected)?;
    let actual_name = simple_type_name(actual)?;
    let with_expr = |template: &str| {
        expr.map(|expr| template.replace("{}", expr))
            .unwrap_or_else(|| template.replace("{}", "value"))
    };

    match (expected, actual_name) {
        ("string", "int" | "float" | "bool" | "nil" | "duration") => {
            Some(format!("did you mean `{}`?", with_expr("to_string({})")))
        }
        ("int", "string") => Some(format!("did you mean `{}`?", with_expr("to_int({})"))),
        ("float", "string" | "int") => {
            Some(format!("did you mean `{}`?", with_expr("to_float({})")))
        }
        (_, "nil") => Some("handle `nil` first or provide a default with `??`".to_string()),
        _ if actual_is_result_of(expected_ty, actual) => Some(format!(
            "did you mean `{}` or `{}`?",
            with_expr("{}?"),
            with_expr("unwrap_or({}, default)")
        )),
        _ => None,
    }
}

fn simple_type_name(ty: &TypeExpr) -> Option<&str> {
    match ty {
        TypeExpr::Named(name) => Some(name.as_str()),
        TypeExpr::LitString(_) => Some("string"),
        TypeExpr::LitInt(_) => Some("int"),
        _ => None,
    }
}

fn is_nilable(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Union(members) if members.len() == 2 => members
            .iter()
            .any(|member| matches!(member, TypeExpr::Named(name) if name == "nil")),
        _ => false,
    }
}

fn actual_is_result_of(expected: &TypeExpr, actual: &TypeExpr) -> bool {
    matches!(
        actual,
        TypeExpr::Applied { name, args }
            if name == "Result" && args.first().is_some_and(|ok| ok == expected)
    )
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
