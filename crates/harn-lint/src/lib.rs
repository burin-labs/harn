use std::collections::HashSet;

use harn_lexer::{FixEdit, Span, StringSegment};
use harn_parser::diagnostic::find_closest_match;
use harn_parser::{stmt_definitely_exits, BindingPattern, Node, SNode, TypeExpr, TypedParam};

mod complexity;
mod fixes;
#[cfg(test)]
mod tests;

use complexity::cyclomatic_complexity;
use fixes::{
    empty_statement_removal_fix, is_pure_expression, nil_fallback_ternary_parts,
    simple_ident_rename_fix,
};

/// A lint diagnostic reported by the linter.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub rule: &'static str,
    pub message: String,
    pub span: Span,
    pub severity: LintSeverity,
    pub suggestion: Option<String>,
    /// Machine-applicable fix edits (applied in order, non-overlapping).
    pub fix: Option<Vec<FixEdit>>,
}

/// Severity level for lint diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LintSeverity {
    Warning,
    Error,
}

/// A variable declaration tracked during linting.
struct Declaration {
    name: String,
    span: Span,
    is_mutable: bool,
    /// True when this declaration came from a simple `let x = ...` or
    /// `var x = ...` binding pattern. False for destructuring patterns like
    /// `let { a, b } = ...` or `let [x, y] = ...`. The `unused-variable`
    /// autofix only rewrites identifiers when this is true, since
    /// destructuring renames need per-field spans that we do not currently
    /// track.
    is_simple_ident: bool,
}

/// An import tracked during linting.
struct ImportInfo {
    names: Vec<String>,
    span: Span,
}

/// A parameter declaration tracked during linting.
struct ParamDeclaration {
    name: String,
    span: Span,
}

/// A function declaration tracked for unused-function detection.
struct FnDeclaration {
    name: String,
    span: Span,
    is_pub: bool,
    is_method: bool,
}

/// A type declaration tracked for unused-type detection.
struct TypeDeclaration {
    name: String,
    span: Span,
    kind: &'static str,
}

/// The linter walks the AST and collects diagnostics.
struct Linter<'a> {
    diagnostics: Vec<LintDiagnostic>,
    scopes: Vec<HashSet<String>>,
    declarations: Vec<Declaration>,
    param_declarations: Vec<ParamDeclaration>,
    references: HashSet<String>,
    assignments: HashSet<String>,
    imports: Vec<ImportInfo>,
    /// Track whether we are inside a loop (for break/continue validation).
    loop_depth: usize,
    /// Track all declared/known function names for undefined-function detection.
    known_functions: HashSet<String>,
    /// Track function call sites for undefined-function checking.
    function_calls: Vec<(String, Span)>,
    /// Whether the file has wildcard imports (import "module").
    /// If true, skip undefined-function checks since we can't know what was imported.
    has_wildcard_import: bool,
    /// Track function declarations for unused-function detection.
    fn_declarations: Vec<FnDeclaration>,
    /// Track actual function usage sites (calls + value references).
    /// Separate from `references` because FnDecl used to self-insert into `references`.
    function_references: HashSet<String>,
    /// Whether the current function is inside an impl block.
    in_impl_block: bool,
    source: Option<&'a str>,
    /// Function names imported by other files (cross-module analysis).
    /// Functions in this set are not flagged as unused even if they have
    /// no local references, because another file explicitly imports them.
    externally_imported_names: HashSet<String>,
    /// Track whether the current traversal is inside a test pipeline body.
    test_pipeline_depth: usize,
    /// Track type declarations for the `unused-type` lint rule.
    type_declarations: Vec<TypeDeclaration>,
    /// Track type names referenced anywhere in the file.
    type_references: HashSet<String>,
}

impl<'a> Linter<'a> {
    fn new(source: Option<&'a str>) -> Self {
        Self {
            diagnostics: Vec::new(),
            scopes: vec![HashSet::new()],
            declarations: Vec::new(),
            param_declarations: Vec::new(),
            references: HashSet::new(),
            assignments: HashSet::new(),
            imports: Vec::new(),
            loop_depth: 0,
            known_functions: Self::builtin_names(),
            function_calls: Vec::new(),
            has_wildcard_import: false,
            fn_declarations: Vec::new(),
            function_references: HashSet::new(),
            in_impl_block: false,
            source,
            externally_imported_names: HashSet::new(),
            test_pipeline_depth: 0,
            type_declarations: Vec::new(),
            type_references: HashSet::new(),
        }
    }

    /// Return set of known builtin function names.
    /// Derived from the VM's actual stdlib registration — no hardcoded list to maintain.
    fn builtin_names() -> HashSet<String> {
        harn_vm::stdlib::stdlib_builtin_names()
            .into_iter()
            .collect()
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn in_test_pipeline(&self) -> bool {
        self.test_pipeline_depth > 0
    }

    fn is_test_pipeline_name(name: &str) -> bool {
        name == "test" || name.starts_with("test_")
    }

    fn is_assert_builtin(name: &str) -> bool {
        matches!(name, "assert" | "assert_eq" | "assert_ne")
    }

    fn is_snake_case(name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_lowercase() || first == '_') {
            return false;
        }
        name.chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    }

    fn is_pascal_case(name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !first.is_ascii_uppercase() {
            return false;
        }
        name.chars().all(|ch| ch.is_ascii_alphanumeric())
    }

    fn lint_function_name(&mut self, name: &str, span: Span) {
        if Self::is_snake_case(name) {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            rule: "naming-convention",
            message: format!("function `{name}` should use snake_case"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rename `{name}` to snake_case (for example `{}`)",
                to_snake_case(name)
            )),
            fix: None,
        });
    }

    fn lint_type_name(&mut self, kind: &'static str, name: &str, span: Span) {
        if Self::is_pascal_case(name) {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            rule: "naming-convention",
            message: format!("{kind} `{name}` should use PascalCase"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rename `{name}` to PascalCase (for example `{}`)",
                to_pascal_case(name)
            )),
            fix: None,
        });
    }

    fn record_type_expr_references(&mut self, type_expr: &TypeExpr) {
        match type_expr {
            TypeExpr::Named(name) => {
                self.type_references.insert(name.clone());
            }
            TypeExpr::Union(types) => {
                for inner in types {
                    self.record_type_expr_references(inner);
                }
            }
            TypeExpr::Shape(fields) => {
                for field in fields {
                    self.record_type_expr_references(&field.type_expr);
                }
            }
            TypeExpr::List(inner) => self.record_type_expr_references(inner),
            TypeExpr::DictType(key, value) => {
                self.record_type_expr_references(key);
                self.record_type_expr_references(value);
            }
            TypeExpr::Applied { name, args } => {
                self.type_references.insert(name.clone());
                for arg in args {
                    self.record_type_expr_references(arg);
                }
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                for param in params {
                    self.record_type_expr_references(param);
                }
                self.record_type_expr_references(return_type);
            }
            TypeExpr::Never => {}
        }
    }

    fn record_param_type_references(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                self.record_type_expr_references(type_expr);
            }
        }
    }

    fn has_interpolation(node: &SNode) -> bool {
        matches!(&node.node, Node::InterpolatedString(segments) if segments.iter().any(|segment| matches!(segment, StringSegment::Expression(_, _, _))))
    }

    /// Returns true if the function is a boundary API that returns untyped/opaque data.
    fn is_boundary_api(name: &str) -> bool {
        matches!(
            name,
            "json_parse"
                | "json_extract"
                | "yaml_parse"
                | "toml_parse"
                | "llm_call"
                | "llm_completion"
                | "http_get"
                | "http_post"
                | "http_put"
                | "http_patch"
                | "http_delete"
                | "http_request"
                | "host_call"
                | "mcp_call"
        )
    }

    /// Extract the root variable name from an assignment target.
    /// For `x = ...` returns `x`, for `x.foo = ...` or `x[i] = ...` returns `x`.
    fn root_var_name(node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::SliceAccess { object, .. } => Self::root_var_name(object),
            _ => None,
        }
    }

    /// Extract all variable names from a binding pattern.
    fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
        match pattern {
            BindingPattern::Identifier(name) => vec![name.clone()],
            BindingPattern::Dict(fields) => fields
                .iter()
                .map(|f| f.alias.as_deref().unwrap_or(&f.key).to_string())
                .collect(),
            BindingPattern::List(elements) => elements.iter().map(|e| e.name.clone()).collect(),
            BindingPattern::Pair(a, b) => vec![a.clone(), b.clone()],
        }
    }

    /// Declare all variables in a binding pattern.
    fn declare_pattern_variables(
        &mut self,
        pattern: &BindingPattern,
        span: Span,
        is_mutable: bool,
    ) {
        let is_simple_ident = matches!(pattern, BindingPattern::Identifier(_));
        for name in Self::pattern_names(pattern) {
            self.declare_variable(&name, span, is_mutable, is_simple_ident);
        }
    }

    /// Declare a variable in the current scope, checking for shadowing.
    fn declare_variable(
        &mut self,
        name: &str,
        span: Span,
        is_mutable: bool,
        is_simple_ident: bool,
    ) {
        if name == "_" {
            return;
        }

        // Check same-scope redeclaration of immutable binding.
        if !is_mutable {
            if let Some(scope) = self.scopes.last() {
                if scope.contains(name) {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "shadow-variable",
                        message: format!(
                            "cannot redeclare immutable variable `{name}` in the same scope"
                        ),
                        span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "use `var {name}` for a mutable binding, or choose a different name"
                        )),
                        fix: None,
                    });
                }
            }
        }

        // Check shadowing against outer scopes.
        if self.scopes.len() > 1 {
            let outer = &self.scopes[..self.scopes.len() - 1];
            if outer.iter().any(|s| s.contains(name)) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "shadow-variable",
                    message: format!("variable `{name}` shadows a variable in an outer scope"),
                    span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("consider renaming to avoid shadowing `{name}`")),
                    fix: None,
                });
            }
        }

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.declarations.push(Declaration {
            name: name.to_string(),
            span,
            is_mutable,
            is_simple_ident,
        });
    }

    /// Declare a function/closure parameter in the current scope.
    /// Tracked separately from variables for the `unused-parameter` lint rule.
    fn declare_parameter(&mut self, name: &str, span: Span) {
        if name == "_" {
            return;
        }

        // Check shadowing against outer scopes (not current scope).
        if self.scopes.len() > 1 {
            let outer = &self.scopes[..self.scopes.len() - 1];
            if outer.iter().any(|s| s.contains(name)) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "shadow-variable",
                    message: format!("variable `{name}` shadows a variable in an outer scope"),
                    span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("consider renaming to avoid shadowing `{name}`")),
                    fix: None,
                });
            }
        }

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.param_declarations.push(ParamDeclaration {
            name: name.to_string(),
            span,
        });
    }

    fn lint_program(&mut self, nodes: &[SNode]) {
        for node in nodes {
            self.lint_node(node);
        }
    }

    fn lint_node(&mut self, snode: &SNode) {
        match &snode.node {
            Node::Pipeline {
                params, body, name, ..
            } => {
                self.known_functions.insert(name.clone());
                self.push_scope();
                for p in params {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(p.clone());
                    }
                    self.references.insert(p.clone());
                }
                self.references.insert(name.clone());
                if Self::is_test_pipeline_name(name) {
                    self.test_pipeline_depth += 1;
                }
                self.lint_block(body);
                if Self::is_test_pipeline_name(name) {
                    self.test_pipeline_depth -= 1;
                }
                self.pop_scope();
            }

            Node::FnDecl {
                name,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
                ..
            } => {
                self.lint_function_name(name, snode.span);
                self.known_functions.insert(name.clone());
                self.fn_declarations.push(FnDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    is_pub: *is_pub,
                    is_method: self.in_impl_block,
                });
                if *is_pub
                    && self
                        .source
                        .and_then(|source| extract_harndoc(source, &snode.span))
                        .is_none()
                {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "missing-harndoc",
                        message: format!("public function `{name}` is missing a `///` doc comment"),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "add a contiguous `///` HarnDoc block above `pub fn {name}`"
                        )),
                        fix: None,
                    });
                }
                self.record_param_type_references(params);
                if let Some(type_expr) = return_type {
                    self.record_type_expr_references(type_expr);
                }
                for clause in where_clauses {
                    self.type_references.insert(clause.bound.clone());
                }
                let complexity = cyclomatic_complexity(body);
                if complexity > 10 {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "cyclomatic-complexity",
                        message: format!(
                            "function `{name}` has cyclomatic complexity {complexity} (> 10)"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(
                            "split the function into smaller helpers or simplify branching"
                                .to_string(),
                        ),
                        fix: None,
                    });
                }
                self.push_scope();
                let saved_loop_depth = self.loop_depth;
                self.loop_depth = 0; // Functions are a new scope
                for p in params {
                    self.declare_parameter(&p.name, snode.span);
                }
                self.lint_block(body);
                self.loop_depth = saved_loop_depth;
                self.pop_scope();
            }

            Node::ToolDecl {
                name,
                params,
                return_type,
                body,
                is_pub,
                ..
            } => {
                self.lint_function_name(name, snode.span);
                self.known_functions.insert(name.clone());
                self.fn_declarations.push(FnDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    is_pub: *is_pub,
                    is_method: false,
                });
                self.record_param_type_references(params);
                if let Some(type_expr) = return_type {
                    self.record_type_expr_references(type_expr);
                }
                let complexity = cyclomatic_complexity(body);
                if complexity > 10 {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "cyclomatic-complexity",
                        message: format!(
                            "function `{name}` has cyclomatic complexity {complexity} (> 10)"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(
                            "split the function into smaller helpers or simplify branching"
                                .to_string(),
                        ),
                        fix: None,
                    });
                }
                self.push_scope();
                let saved_loop_depth = self.loop_depth;
                self.loop_depth = 0;
                for p in params {
                    self.declare_parameter(&p.name, snode.span);
                }
                self.lint_block(body);
                self.loop_depth = saved_loop_depth;
                self.pop_scope();
            }

            Node::ImplBlock { type_name, methods } => {
                self.type_references.insert(type_name.clone());
                let saved = self.in_impl_block;
                self.in_impl_block = true;
                for method in methods {
                    self.lint_node(method);
                }
                self.in_impl_block = saved;
            }

            Node::LetBinding { pattern, value, .. } => {
                self.lint_node(value);
                self.declare_pattern_variables(pattern, snode.span, false);
            }

            Node::VarBinding { pattern, value, .. } => {
                self.lint_node(value);
                self.declare_pattern_variables(pattern, snode.span, true);
            }

            Node::Assignment { target, value, .. } => {
                if let Some(name) = Self::root_var_name(target) {
                    self.assignments.insert(name);
                }
                self.lint_node(target);
                self.lint_node(value);
            }

            Node::Identifier(name) => {
                self.references.insert(name.clone());
                self.function_references.insert(name.clone());
            }

            Node::FunctionCall { name, args } => {
                self.references.insert(name.clone());
                self.function_references.insert(name.clone());
                self.function_calls.push((name.clone(), snode.span));
                if Self::is_assert_builtin(name) && !self.in_test_pipeline() {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "assert-outside-test",
                        message: format!(
                            "`{name}` is intended for test pipelines, not production control flow"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(
                            "use `require` for invariants in non-test code".to_string(),
                        ),
                        fix: None,
                    });
                }
                if name == "llm_call" && args.get(1).is_some_and(Self::has_interpolation) {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "prompt-injection-risk",
                        message:
                            "interpolated data in the `llm_call` system prompt can smuggle untrusted instructions"
                                .to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(
                            "keep the system prompt static and pass dynamic data in the user prompt or options"
                                .to_string(),
                        ),
                        fix: None,
                    });
                }
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                self.lint_node(object);
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                if let Node::FunctionCall { name, .. } = &object.node {
                    if Self::is_boundary_api(name) {
                        self.diagnostics.push(LintDiagnostic {
                            rule: "untyped-dict-access",
                            message: format!(
                                "property access on raw `{}()` result without schema validation",
                                name
                            ),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(
                                "assign to a variable and validate with schema_expect() or a type annotation first"
                                    .to_string(),
                            ),
                            fix: None,
                        });
                    }
                }
                self.lint_node(object);
            }

            Node::SubscriptAccess { object, index } => {
                if let Node::FunctionCall { name, .. } = &object.node {
                    if Self::is_boundary_api(name) {
                        self.diagnostics.push(LintDiagnostic {
                            rule: "untyped-dict-access",
                            message: format!(
                                "subscript access on raw `{}()` result without schema validation",
                                name
                            ),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(
                                "assign to a variable and validate with schema_expect() or a type annotation first"
                                    .to_string(),
                            ),
                            fix: None,
                        });
                    }
                }
                self.lint_node(object);
                self.lint_node(index);
            }

            Node::SliceAccess { object, start, end } => {
                self.lint_node(object);
                if let Some(s) = start {
                    self.lint_node(s);
                }
                if let Some(e) = end {
                    self.lint_node(e);
                }
            }

            Node::BinaryOp { op, left, right } => {
                // Rule: comparison-to-bool
                if op == "==" || op == "!=" {
                    let is_bool_left = matches!(left.node, Node::BoolLiteral(_));
                    let is_bool_right = matches!(right.node, Node::BoolLiteral(_));
                    if is_bool_left || is_bool_right {
                        let (suggestion, msg) = if op == "==" {
                            if matches!(right.node, Node::BoolLiteral(true))
                                || matches!(left.node, Node::BoolLiteral(true))
                            {
                                (
                                    "remove the comparison, use the expression directly",
                                    "comparison to `true` is redundant",
                                )
                            } else {
                                ("use `!expr` instead", "comparison to `false` is redundant")
                            }
                        } else if matches!(right.node, Node::BoolLiteral(true))
                            || matches!(left.node, Node::BoolLiteral(true))
                        {
                            ("use `!expr` instead", "`!= true` is redundant")
                        } else {
                            (
                                "remove the comparison, use the expression directly",
                                "`!= false` is redundant",
                            )
                        };
                        let fix = self.source.and_then(|src| {
                            let expr_text = src.get(snode.span.start..snode.span.end)?;
                            let replacement = simplify_bool_comparison(expr_text)?;
                            Some(vec![FixEdit {
                                span: snode.span,
                                replacement,
                            }])
                        });
                        self.diagnostics.push(LintDiagnostic {
                            rule: "comparison-to-bool",
                            message: msg.to_string(),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(suggestion.to_string()),
                            fix,
                        });
                    }
                }
                // Rule: invalid-binary-op-literal
                if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
                    let has_bad_literal =
                        matches!(left.node, Node::BoolLiteral(_) | Node::NilLiteral)
                            || matches!(right.node, Node::BoolLiteral(_) | Node::NilLiteral);
                    if has_bad_literal {
                        // Offer interpolation fix when op is `+` and one side is a string literal
                        let fix = if op == "+" {
                            self.source.and_then(|src| {
                                let is_left_str = matches!(&left.node, Node::StringLiteral(_));
                                let is_right_str = matches!(&right.node, Node::StringLiteral(_));
                                if !is_left_str && !is_right_str {
                                    return None;
                                }
                                let (str_node, other_node) = if is_left_str {
                                    (&**left, &**right)
                                } else {
                                    (&**right, &**left)
                                };
                                let str_text = src.get(str_node.span.start..str_node.span.end)?;
                                let other_text =
                                    src.get(other_node.span.start..other_node.span.end)?;
                                // Strip quotes from string literal
                                let inner = str_text.strip_prefix('"')?.strip_suffix('"')?;
                                let replacement = if is_left_str {
                                    format!("\"{inner}${{{other_text}}}\"")
                                } else {
                                    format!("\"${{{other_text}}}{inner}\"")
                                };
                                Some(vec![FixEdit {
                                    span: snode.span,
                                    replacement,
                                }])
                            })
                        } else {
                            None
                        };
                        self.diagnostics.push(LintDiagnostic {
                            rule: "invalid-binary-op-literal",
                            message: format!(
                                "operator '{}' used with boolean or nil literal — this will cause a runtime error",
                                op
                            ),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(
                                "use to_string() or string interpolation to convert values explicitly".to_string(),
                            ),
                            fix,
                        });
                    }
                }
                self.lint_node(left);
                self.lint_node(right);
            }

            Node::UnaryOp { operand, .. } => {
                self.lint_node(operand);
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.lint_node(condition);
                self.lint_node(true_expr);
                self.lint_node(false_expr);

                // Rule: redundant-nil-ternary
                //
                // Detect ternary fallbacks over a nil check where the
                // non-nil branch is identical to the checked variable:
                //
                //   x == nil ? fallback : x   →   x ?? fallback
                //   x != nil ? x : fallback   →   x ?? fallback
                //
                // Only fires when the checked variable is a bare identifier
                // and appears syntactically identical on both sides, so the
                // rewrite is always safe regardless of the ??-side being a
                // pure value (the checked variable is evaluated exactly
                // once in both the original and rewritten forms).
                if let Some((ident, fallback)) =
                    nil_fallback_ternary_parts(condition, true_expr, false_expr)
                {
                    let fix = self.source.and_then(|src| {
                        let fallback_text = src.get(fallback.span.start..fallback.span.end)?;
                        let replacement = format!("{ident} ?? {fallback_text}");
                        Some(vec![FixEdit {
                            span: snode.span,
                            replacement,
                        }])
                    });
                    self.diagnostics.push(LintDiagnostic {
                        rule: "redundant-nil-ternary",
                        message: format!(
                            "ternary nil check over `{ident}` can be replaced with `{ident} ?? <fallback>`"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!("use `{ident} ?? <fallback>` instead")),
                        fix,
                    });
                }
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.lint_node(condition);
                // Check empty then-block.
                if then_body.is_empty() {
                    // Only autofix when there is no `else` branch (removing
                    // the whole if-else would silently drop the else body)
                    // and when the condition has no observable side effects.
                    let fix = if else_body.is_none() && is_pure_expression(&condition.node) {
                        empty_statement_removal_fix(self.source, snode.span)
                    } else {
                        None
                    };
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "if block has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty if or add a body".to_string()),
                        fix,
                    });
                }
                // Rule: unnecessary-else-return
                if let Some(else_b) = else_body {
                    let then_returns = then_body
                        .last()
                        .is_some_and(|s| matches!(s.node, Node::ReturnStmt { .. }));
                    let else_returns = else_b
                        .last()
                        .is_some_and(|s| matches!(s.node, Node::ReturnStmt { .. }));
                    if then_returns && else_returns {
                        // Build fix: replace `} else { <body> }` with `}\n<body>`
                        let fix = self.source.and_then(|src| {
                            let then_last = then_body.last()?;
                            let else_first = else_b.first()?;
                            let else_last = else_b.last()?;
                            // Find the `} else {` region between then-body end and else-body start
                            let search_start = then_last.span.end;
                            // The else body content in source
                            let body_text = src.get(else_first.span.start..else_last.span.end)?;
                            // Find closing `}` of the else block (end of the whole if-else node)
                            let else_block_end = snode.span.end;
                            // Find where `else` starts: scan backwards from else_first for `else`
                            let between = src.get(search_start..else_first.span.start)?;
                            let else_kw_off = between.find("else")?;
                            let else_start = search_start + else_kw_off;
                            // Determine indentation from the if statement's line
                            let line_start =
                                src[..snode.span.start].rfind('\n').map_or(0, |p| p + 1);
                            let indent = &src[line_start..snode.span.start];
                            // Replace from `} else { ... }` to `}\n<indent><body>`
                            // The span to replace: from the `}` before else to the final `}`
                            let close_brace = src.get(search_start..else_start)?.rfind('}')?;
                            let replace_start = search_start + close_brace + 1; // after the `}`
                            Some(vec![FixEdit {
                                span: Span::with_offsets(
                                    replace_start,
                                    else_block_end,
                                    then_last.span.end_line,
                                    1,
                                ),
                                replacement: format!("\n{indent}{body_text}"),
                            }])
                        });
                        self.diagnostics.push(LintDiagnostic {
                            rule: "unnecessary-else-return",
                            message: "both if and else branches return — else is unnecessary"
                                .to_string(),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(
                                "remove the else and place its body after the if".to_string(),
                            ),
                            fix,
                        });
                    }
                }
                self.push_scope();
                self.lint_block(then_body);
                self.pop_scope();
                if let Some(else_b) = else_body {
                    self.push_scope();
                    self.lint_block(else_b);
                    self.pop_scope();
                }
            }

            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.lint_node(iterable);
                if body.is_empty() {
                    // Only autofix when the iterable has no observable side
                    // effects; a pure iterable means the whole statement is
                    // a no-op and can be removed.
                    let fix = if is_pure_expression(&iterable.node) {
                        empty_statement_removal_fix(self.source, snode.span)
                    } else {
                        None
                    };
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "for loop has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty for loop or add a body".to_string()),
                        fix,
                    });
                }
                self.push_scope();
                // Register all pattern variables in scope and mark as referenced
                for name in Self::pattern_names(pattern) {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(name.clone());
                    }
                    self.references.insert(name);
                }
                self.loop_depth += 1;
                self.lint_block(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }

            Node::WhileLoop { condition, body } => {
                self.lint_node(condition);
                if body.is_empty() {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "while loop has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty while loop or add a body".to_string()),
                        fix: None,
                    });
                }
                self.push_scope();
                self.loop_depth += 1;
                self.lint_block(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }

            Node::TryCatch {
                body,
                error_var,
                catch_body,
                finally_body,
                ..
            } => {
                if body.is_empty() {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "try block has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty try/catch or add a body".to_string()),
                        fix: None,
                    });
                }
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
                self.push_scope();
                if let Some(ev) = error_var {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(ev.clone());
                    }
                    self.references.insert(ev.clone());
                }
                self.lint_block(catch_body);
                self.pop_scope();
                if let Some(fb) = finally_body {
                    self.push_scope();
                    self.lint_block(fb);
                    self.pop_scope();
                }
            }

            Node::TryExpr { body } => {
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
            }

            Node::MatchExpr { value, arms } => {
                self.lint_node(value);
                // Rule: duplicate-match-arm (uses PartialEq on Node)
                for (i, arm) in arms.iter().enumerate() {
                    for earlier in &arms[..i] {
                        if arm.pattern.node == earlier.pattern.node && arm.guard == earlier.guard {
                            self.diagnostics.push(LintDiagnostic {
                                rule: "duplicate-match-arm",
                                message: "duplicate match arm pattern".to_string(),
                                span: arm.pattern.span,
                                severity: LintSeverity::Warning,
                                suggestion: Some("remove the duplicate arm".to_string()),
                                fix: None,
                            });
                            break;
                        }
                    }
                    self.lint_node(&arm.pattern);
                    if let Some(ref guard) = arm.guard {
                        self.lint_node(guard);
                    }
                    self.push_scope();
                    self.lint_block(&arm.body);
                    self.pop_scope();
                }
            }

            Node::Retry { count, body } => {
                self.lint_node(count);
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
            }

            Node::ReturnStmt { value } => {
                if let Some(v) = value {
                    self.lint_node(v);
                }
            }

            Node::ThrowStmt { value } => {
                self.lint_node(value);
            }

            Node::Block(nodes) => {
                self.push_scope();
                self.lint_block(nodes);
                self.pop_scope();
            }

            Node::Closure { params, body, .. } => {
                self.push_scope();
                let saved_loop_depth = self.loop_depth;
                self.loop_depth = 0; // Closures are a new scope — break/continue invalid
                for p in params {
                    self.declare_parameter(&p.name, snode.span);
                }
                self.lint_block(body);
                self.loop_depth = saved_loop_depth;
                self.pop_scope();
            }

            Node::SpawnExpr { body } => {
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
            }

            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.lint_node(condition);
                self.push_scope();
                self.lint_block(else_body);
                self.pop_scope();
            }

            Node::RequireStmt { condition, message } => {
                if self.in_test_pipeline() {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "require-in-test",
                        message: "`require` in a test pipeline should usually be an assertion"
                            .to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(
                            "prefer `assert(...)` or `assert_eq(...)` in test pipelines"
                                .to_string(),
                        ),
                        fix: None,
                    });
                }
                self.lint_node(condition);
                if let Some(message) = message {
                    self.lint_node(message);
                }
            }

            Node::DeadlineBlock { duration, body } => {
                self.lint_node(duration);
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
            }

            Node::MutexBlock { body } => {
                self.push_scope();
                self.lint_block(body);
                self.pop_scope();
            }

            Node::Parallel {
                expr,
                body,
                variable,
                ..
            } => {
                self.lint_node(expr);
                self.push_scope();
                if let Some(v) = variable {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(v.clone());
                    }
                    self.references.insert(v.clone());
                }
                self.lint_block(body);
                self.pop_scope();
            }

            Node::ListLiteral(items) => {
                for item in items {
                    self.lint_node(item);
                }
            }

            Node::DictLiteral(entries) => {
                for entry in entries {
                    self.lint_node(&entry.key);
                    self.lint_node(&entry.value);
                }
            }

            Node::InterpolatedString(segments) => {
                for seg in segments {
                    if let StringSegment::Expression(expr, _, _) = seg {
                        // Extract the root identifier from expressions like
                        // "name", "opts.host", "x + 1", etc.
                        // We split on non-identifier chars and record any
                        // leading identifier tokens as references.
                        for token in expr.split(|c: char| !c.is_alphanumeric() && c != '_') {
                            if !token.is_empty()
                                && token
                                    .chars()
                                    .next()
                                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                            {
                                self.references.insert(token.to_string());
                                self.function_references.insert(token.to_string());
                            }
                        }
                    }
                }
            }

            Node::RangeExpr { start, end, .. } => {
                self.lint_node(start);
                self.lint_node(end);
            }

            Node::DeferStmt { body } => {
                self.lint_block(body);
            }

            Node::YieldExpr { value } => {
                if let Some(v) = value {
                    self.lint_node(v);
                }
            }

            Node::EnumConstruct {
                enum_name, args, ..
            } => {
                self.type_references.insert(enum_name.clone());
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                self.type_references.insert(struct_name.clone());
                for entry in fields {
                    self.lint_node(&entry.key);
                    self.lint_node(&entry.value);
                }
            }

            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.lint_node(&case.channel);
                    self.push_scope();
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(case.variable.clone());
                    }
                    self.lint_block(&case.body);
                    self.pop_scope();
                }
                if let Some((dur, body)) = timeout {
                    self.lint_node(dur);
                    self.push_scope();
                    self.lint_block(body);
                    self.pop_scope();
                }
                if let Some(body) = default_body {
                    self.push_scope();
                    self.lint_block(body);
                    self.pop_scope();
                }
            }

            Node::Spread(inner) => {
                self.lint_node(inner);
            }

            Node::TryOperator { operand } => {
                self.lint_node(operand);
            }

            Node::StructDecl { name, fields, .. } => {
                self.lint_type_name("struct", name, snode.span);
                self.known_functions.insert(name.clone());
                self.type_declarations.push(TypeDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    kind: "struct",
                });
                for field in fields {
                    if let Some(type_expr) = &field.type_expr {
                        self.record_type_expr_references(type_expr);
                    }
                }
            }
            Node::EnumDecl { name, variants, .. } => {
                self.lint_type_name("enum", name, snode.span);
                self.known_functions.insert(name.clone());
                self.type_declarations.push(TypeDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    kind: "enum",
                });
                for variant in variants {
                    self.record_param_type_references(&variant.fields);
                }
            }
            Node::SelectiveImport { names, .. } => {
                for name in names {
                    self.known_functions.insert(name.clone());
                }
                self.imports.push(ImportInfo {
                    names: names.clone(),
                    span: snode.span,
                });
            }

            Node::ImportDecl { .. } => {
                self.has_wildcard_import = true;
            }

            Node::InterfaceDecl {
                name,
                associated_types,
                methods,
                ..
            } => {
                self.lint_type_name("interface", name, snode.span);
                self.type_declarations.push(TypeDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    kind: "interface",
                });
                for (_, default_type) in associated_types {
                    if let Some(type_expr) = default_type {
                        self.record_type_expr_references(type_expr);
                    }
                }
                for method in methods {
                    self.record_param_type_references(&method.params);
                    if let Some(type_expr) = &method.return_type {
                        self.record_type_expr_references(type_expr);
                    }
                }
            }

            Node::TypeDecl { name, type_expr } => {
                self.lint_type_name("type", name, snode.span);
                self.record_type_expr_references(type_expr);
            }

            // Leaf nodes and declarations that don't need recursion.
            Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::DurationLiteral(_)
            | Node::OverrideDecl { .. }
            | Node::BreakStmt
            | Node::ContinueStmt => {
                // Rule: break/continue outside loop
                if matches!(snode.node, Node::BreakStmt | Node::ContinueStmt)
                    && self.loop_depth == 0
                {
                    let keyword = if matches!(snode.node, Node::BreakStmt) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.diagnostics.push(LintDiagnostic {
                        rule: "break-outside-loop",
                        message: format!("`{keyword}` used outside of a loop"),
                        span: snode.span,
                        severity: LintSeverity::Error,
                        suggestion: Some(format!(
                            "`{keyword}` can only be used inside for or while loops"
                        )),
                        fix: None,
                    });
                }
            }
        }
    }

    /// Lint a block of statements, checking for unreachable code.
    fn lint_block(&mut self, nodes: &[SNode]) {
        let mut found_terminator = false;

        for node in nodes {
            if found_terminator {
                self.diagnostics.push(LintDiagnostic {
                    rule: "unreachable-code",
                    message: "unreachable code after return or throw".to_string(),
                    span: node.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("remove the unreachable code".to_string()),
                    fix: None,
                });
                // Only report the first unreachable statement per block.
                break;
            }

            self.lint_node(node);

            if stmt_definitely_exits(node) {
                found_terminator = true;
            }
        }
    }

    /// Run post-walk analysis and finalize diagnostics.
    fn finalize(&mut self) {
        // Rule: unused-variable
        for decl in &self.declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.references.contains(&decl.name) {
                let fix = if decl.is_simple_ident {
                    simple_ident_rename_fix(self.source, decl.span, &decl.name)
                } else {
                    None
                };
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-variable",
                    message: format!("variable `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("prefix with underscore: `_{}`", decl.name)),
                    fix,
                });
            }
        }

        // Rule: unused-parameter
        for decl in &self.param_declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-parameter",
                    message: format!("parameter `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("prefix with underscore: `_{}`", decl.name)),
                    fix: None,
                });
            }
        }

        // Rule: unused-import
        for import in &self.imports {
            let unused: Vec<&String> = import
                .names
                .iter()
                .filter(|n| !self.references.contains(*n))
                .collect();
            let all_unused = unused.len() == import.names.len();
            for name in &unused {
                let fix = self.source.and_then(|src| {
                    if all_unused {
                        // Remove the entire import statement including trailing newline
                        let end = if src.get(import.span.end..import.span.end + 1) == Some("\n") {
                            import.span.end + 1
                        } else {
                            import.span.end
                        };
                        Some(vec![FixEdit {
                            span: Span::with_offsets(
                                import.span.start,
                                end,
                                import.span.line,
                                import.span.column,
                            ),
                            replacement: String::new(),
                        }])
                    } else {
                        // Remove just this name from the import list
                        let region = src.get(import.span.start..import.span.end)?;
                        // Find the name in the { ... } block
                        let name_pos = region.find(name.as_str())?;
                        let abs_start = import.span.start + name_pos;
                        let abs_end = abs_start + name.len();
                        // Also remove surrounding comma and whitespace
                        let after = src.get(abs_end..import.span.end)?;
                        let before = src.get(import.span.start..abs_start)?;
                        let (rm_start, rm_end) = if after.starts_with(',') {
                            // Remove name + comma + optional space after
                            let extra = if after.get(1..2) == Some(" ") { 2 } else { 1 };
                            (abs_start, abs_end + extra)
                        } else if before.ends_with(", ") {
                            // Last item: remove preceding ", " + name
                            (abs_start - 2, abs_end)
                        } else if before.ends_with(',') {
                            (abs_start - 1, abs_end)
                        } else {
                            (abs_start, abs_end)
                        };
                        Some(vec![FixEdit {
                            span: Span::with_offsets(
                                rm_start,
                                rm_end,
                                import.span.line,
                                import.span.column,
                            ),
                            replacement: String::new(),
                        }])
                    }
                });
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-import",
                    message: format!("imported name `{name}` is never used"),
                    span: import.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("remove `{name}` from the import")),
                    fix,
                });
            }
        }

        // Rule: mutable-never-reassigned
        for decl in &self.declarations {
            if !decl.is_mutable {
                continue;
            }
            if !self.assignments.contains(&decl.name) {
                let fix = self.source.and_then(|src| {
                    let region = src.get(decl.span.start..decl.span.end)?;
                    let var_off = region.find("var")?;
                    let abs = decl.span.start + var_off;
                    Some(vec![FixEdit {
                        span: Span::with_offsets(
                            abs,
                            abs + 3,
                            decl.span.line,
                            decl.span.column + var_off,
                        ),
                        replacement: "let".to_string(),
                    }])
                });
                self.diagnostics.push(LintDiagnostic {
                    rule: "mutable-never-reassigned",
                    message: format!(
                        "variable `{}` is declared as `var` but never reassigned",
                        decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("use `let` instead of `var`".to_string()),
                    fix,
                });
            }
        }

        // Rule: unused-function
        for decl in &self.fn_declarations {
            if decl.is_pub || decl.is_method || decl.name.starts_with('_') {
                continue;
            }
            if self.externally_imported_names.contains(&decl.name) {
                continue;
            }
            if !self.function_references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-function",
                    message: format!("function `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!(
                        "remove the function or prefix with underscore: `_{}`",
                        decl.name
                    )),
                    fix: None,
                });
            }
        }

        // Rule: unused-type
        for decl in &self.type_declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.type_references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-type",
                    message: format!(
                        "{} `{}` is declared but never referenced",
                        decl.kind, decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!(
                        "remove the unused {} or reference `{}` from a signature or constructor",
                        decl.kind, decl.name
                    )),
                    fix: None,
                });
            }
        }

        // Variables and parameters that could hold closures
        let all_vars: HashSet<String> = self
            .declarations
            .iter()
            .map(|d| d.name.clone())
            .chain(self.param_declarations.iter().map(|p| p.name.clone()))
            .collect();

        // Rule: undefined-function
        // Skip entirely if the file has wildcard imports — we can't know what they provide
        if self.has_wildcard_import {
            return;
        }
        for (name, span) in &self.function_calls {
            if self.known_functions.contains(name) {
                continue;
            }
            // Skip if it's a known variable (could be a closure)
            if all_vars.contains(name) {
                continue;
            }
            // Skip internal names starting with __
            if name.starts_with("__") {
                continue;
            }
            let suggestion = if let Some(closest) =
                find_closest_match(name, self.known_functions.iter().map(|s| s.as_str()), 2)
            {
                format!("did you mean `{closest}`?")
            } else {
                format!("check the spelling or import `{name}`")
            };
            self.diagnostics.push(LintDiagnostic {
                rule: "undefined-function",
                message: format!("function `{name}` is not defined"),
                span: *span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix: None,
            });
        }
    }
}

fn extract_harndoc(source: &str, span: &Span) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let def_line = span.line.saturating_sub(1);
    if def_line == 0 {
        return None;
    }
    let mut comment_lines = Vec::new();
    let mut line_idx = def_line - 1;
    loop {
        let line = lines.get(line_idx)?;
        let trimmed = line.trim();
        if trimmed.starts_with("///") {
            comment_lines.push(trimmed.trim_start_matches("///").trim_start().to_string());
        } else {
            break;
        }
        if line_idx == 0 {
            break;
        }
        line_idx -= 1;
    }
    if comment_lines.is_empty() {
        None
    } else {
        comment_lines.reverse();
        Some(comment_lines.join("\n"))
    }
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn to_pascal_case(name: &str) -> String {
    let mut out = String::new();
    let mut uppercase_next = true;
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            out.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// Lint an AST program and return all diagnostics.
pub fn lint(program: &[SNode]) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, &[], None)
}

/// Lint an AST program with source-aware rules enabled.
pub fn lint_with_source(program: &[SNode], source: &str) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, &[], Some(source))
}

/// Lint an AST program, filtering out diagnostics for disabled rules.
pub fn lint_with_config(program: &[SNode], disabled_rules: &[String]) -> Vec<LintDiagnostic> {
    lint_with_config_and_source(program, disabled_rules, None)
}

/// Lint an AST program, optionally using the original source for source-aware rules.
pub fn lint_with_config_and_source(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
) -> Vec<LintDiagnostic> {
    lint_full(program, disabled_rules, source, &HashSet::new())
}

/// Lint with cross-file import awareness.  `externally_imported_names` is the
/// set of function names that other files import from this file — these are
/// exempt from the unused-function lint even without local references.
pub fn lint_with_cross_file_imports(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
) -> Vec<LintDiagnostic> {
    lint_full(program, disabled_rules, source, externally_imported_names)
}

fn lint_full(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
) -> Vec<LintDiagnostic> {
    let mut linter = Linter::new(source);
    linter
        .externally_imported_names
        .clone_from(externally_imported_names);
    linter.lint_program(program);
    linter.finalize();
    if disabled_rules.is_empty() {
        linter.diagnostics
    } else {
        linter
            .diagnostics
            .into_iter()
            .filter(|d| !disabled_rules.iter().any(|r| r == d.rule))
            .collect()
    }
}

/// Extract all function names that appear in selective import statements
/// (`import { foo, bar } from "module"`).  Used by the CLI to build a
/// cross-file "externally imported" name set before linting.
pub fn collect_selective_import_names(program: &[SNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for snode in program {
        if let harn_parser::Node::SelectiveImport {
            names: imported, ..
        } = &snode.node
        {
            names.extend(imported.iter().cloned());
        }
    }
    names
}

/// Simplify a boolean comparison expression like `x == true` → `x`.
pub fn simplify_bool_comparison(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    for op in &["==", "!="] {
        if let Some(idx) = trimmed.find(op) {
            let lhs = trimmed[..idx].trim();
            let rhs = trimmed[idx + op.len()..].trim();
            let (bool_val, other) = if rhs == "true" || rhs == "false" {
                (rhs, lhs)
            } else if lhs == "true" || lhs == "false" {
                (lhs, rhs)
            } else {
                continue;
            };
            let is_eq = *op == "==";
            let is_true = bool_val == "true";
            return if is_eq == is_true {
                Some(other.to_string())
            } else {
                Some(format!("!{other}"))
            };
        }
    }
    None
}
