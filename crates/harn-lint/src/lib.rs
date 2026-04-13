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
        if let Some(src) = self.source {
            check_legacy_doc_comments(src, nodes, &mut self.diagnostics);
            check_blank_line_between_items(src, nodes, &mut self.diagnostics);
            check_trailing_comma(src, &mut self.diagnostics);
            check_import_order(src, nodes, &mut self.diagnostics);
        }
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
                        message: format!(
                            "public function `{name}` is missing a `/** */` doc comment"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "add a `/** ... */` HarnDoc block directly above `pub fn {name}`"
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

/// Return true when this node kind is a top-level item for the purposes of
/// the `blank-line-between-items` rule. This is the slightly-broader set
/// that includes let/var bindings at module scope.
fn is_top_level_item(node: &Node) -> bool {
    matches!(
        node,
        Node::FnDecl { .. }
            | Node::Pipeline { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ToolDecl { .. }
            | Node::ImplBlock { .. }
            | Node::OverrideDecl { .. }
            | Node::LetBinding { .. }
            | Node::VarBinding { .. }
    )
}

fn is_import_item(node: &Node) -> bool {
    matches!(
        node,
        Node::ImportDecl { .. } | Node::SelectiveImport { .. }
    )
}

/// Return true when this node kind participates in the `legacy-doc-comment`
/// rule (i.e. is a documented item whose preceding comments should use the
/// canonical `/** */` form). Mirrors the plan's whitelist.
fn is_documentable_item(node: &Node) -> bool {
    matches!(
        node,
        Node::FnDecl { .. }
            | Node::Pipeline { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ToolDecl { .. }
            | Node::ImplBlock { .. }
            | Node::OverrideDecl { .. }
    )
}

fn item_is_pub(node: &Node) -> bool {
    match node {
        Node::FnDecl { is_pub, .. }
        | Node::Pipeline { is_pub, .. }
        | Node::StructDecl { is_pub, .. }
        | Node::EnumDecl { is_pub, .. }
        | Node::ToolDecl { is_pub, .. } => *is_pub,
        // InterfaceDecl / ImplBlock / TypeDecl / OverrideDecl have no
        // is_pub flag — treat them as always-eligible when they appear at
        // the top level.
        Node::InterfaceDecl { .. }
        | Node::ImplBlock { .. }
        | Node::TypeDecl { .. }
        | Node::OverrideDecl { .. } => true,
        _ => false,
    }
}

/// A comment token recovered from a re-lex of the source.
#[derive(Clone)]
struct LegacyCommentTok {
    line: usize,
    start_byte: usize,
    end_byte: usize,
    is_line: bool,
    is_doc: bool,
    text: String,
}

/// Walk the source with the lexer and return a vector of line-comment and
/// block-comment tokens, in source order.
fn collect_comment_tokens(source: &str) -> Vec<LegacyCommentTok> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tok in tokens {
        match tok.kind {
            harn_lexer::TokenKind::LineComment { text, is_doc } => {
                out.push(LegacyCommentTok {
                    line: tok.span.line,
                    start_byte: tok.span.start,
                    end_byte: tok.span.end,
                    is_line: true,
                    is_doc,
                    text,
                });
            }
            harn_lexer::TokenKind::BlockComment { text, is_doc } => {
                out.push(LegacyCommentTok {
                    line: tok.span.line,
                    start_byte: tok.span.start,
                    end_byte: tok.span.end,
                    is_line: false,
                    is_doc,
                    text,
                });
            }
            _ => {}
        }
    }
    out
}

/// Produce the canonical `/** */` replacement text for a run of comment
/// tokens. `body_lines` contains one text line per collected comment (already
/// stripped of leading `//` / `///` markers). `indent` is the column indent
/// (in spaces) to use for continuation lines. The return value does NOT
/// include a trailing newline — the replacement span is expected to cover
/// exactly the original comment lines' textual range.
fn canonical_doc_block(body_lines: &[String], indent: usize, line_width: usize) -> String {
    let indent_str = " ".repeat(indent);
    // Trim leading/trailing blank lines.
    let mut start = 0;
    while start < body_lines.len() && body_lines[start].trim().is_empty() {
        start += 1;
    }
    let mut end = body_lines.len();
    while end > start && body_lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let body = &body_lines[start..end];
    if body.is_empty() {
        return format!("{indent_str}/** */");
    }
    if body.len() == 1 {
        let only = body[0].trim();
        let compact = format!("{indent_str}/** {only} */");
        if compact.len() <= line_width {
            return compact;
        }
    }
    let mut out = String::new();
    out.push_str(&indent_str);
    out.push_str("/**");
    for line in body {
        out.push('\n');
        if line.trim().is_empty() {
            out.push_str(&indent_str);
            out.push_str(" *");
        } else {
            out.push_str(&indent_str);
            out.push_str(" * ");
            out.push_str(line.trim_end());
        }
    }
    out.push('\n');
    out.push_str(&indent_str);
    out.push_str(" */");
    out
}

/// Collect and emit `legacy-doc-comment` diagnostics. Walks top-level items
/// plus `pub` methods inside `impl` blocks, looks for a contiguous run of
/// `///` lines (or `//` lines with no blank line between the run and the
/// item), and produces an autofix replacement with the canonical form.
fn check_legacy_doc_comments(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let comments = collect_comment_tokens(source);
    if comments.is_empty() {
        return;
    }
    // Index by starting line for fast upward walks.
    let by_line: std::collections::HashMap<usize, &LegacyCommentTok> = comments
        .iter()
        .map(|c| (c.line, c))
        .collect();

    fn visit(
        node: &SNode,
        comments: &[LegacyCommentTok],
        by_line: &std::collections::HashMap<usize, &LegacyCommentTok>,
        source: &str,
        diagnostics: &mut Vec<LintDiagnostic>,
        is_top_level: bool,
    ) {
        // Eligible if: the item is documentable AND it's either top-level
        // OR explicitly `pub`. Impl methods are only eligible when pub (since
        // impl itself is the "top-level" container for them).
        if is_documentable_item(&node.node) && (is_top_level || item_is_pub(&node.node)) {
            check_one_item(node, comments, by_line, source, diagnostics);
        }
        match &node.node {
            Node::Pipeline { body, .. }
            | Node::FnDecl { body, .. }
            | Node::ToolDecl { body, .. }
            | Node::OverrideDecl { body, .. } => {
                for child in body {
                    visit(child, comments, by_line, source, diagnostics, false);
                }
            }
            Node::ImplBlock { methods, .. } => {
                for m in methods {
                    visit(m, comments, by_line, source, diagnostics, false);
                }
            }
            _ => {}
        }
    }

    for node in program {
        visit(node, &comments, &by_line, source, diagnostics, true);
    }
}

fn check_one_item(
    node: &SNode,
    _comments: &[LegacyCommentTok],
    by_line: &std::collections::HashMap<usize, &LegacyCommentTok>,
    source: &str,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let item_line = node.span.line;
    if item_line == 0 {
        return;
    }
    // Walk upward. For each candidate previous line, require a line-comment
    // token there. Stop at the first non-comment line.
    let mut walked: Vec<&LegacyCommentTok> = Vec::new();
    let mut cursor = item_line.saturating_sub(1);
    while cursor > 0 {
        let Some(tok) = by_line.get(&cursor) else {
            break;
        };
        // Only `//`-style line comments participate in this heuristic. An
        // existing `/** */` block doesn't need rewriting.
        if !tok.is_line {
            break;
        }
        walked.push(*tok);
        cursor -= 1;
    }
    if walked.is_empty() {
        return;
    }
    walked.reverse(); // now in top-to-bottom order
    // Any contiguous run of `//` / `///` line comments directly above the
    // item (no blank-line gap) is treated as a doc comment and rewritten.
    // This covers both design triggers:
    //  - entirely `///` lines (the original HarnDoc form), and
    //  - entirely plain `//` lines adjacent to a def,
    // as well as the pragmatic mixed case (legacy hand-written `//` block
    // followed by an auto-generated `///` stub — both are the item's doc).
    let any_doc = walked.iter().any(|c| c.is_doc);
    let any_plain = walked.iter().any(|c| !c.is_doc);

    // Compute the byte range covering the entire comment run, including the
    // line's leading indentation. Replacement span starts at the start of
    // the first comment's line (so we can reset indentation).
    let first = walked.first().unwrap();
    let last = walked.last().unwrap();
    let line_start = line_start_byte(source, first.start_byte);
    let indent_cols = first.start_byte - line_start;
    // Build body text lines from the stripped comments.
    let mut body_lines: Vec<String> = Vec::with_capacity(walked.len());
    for c in &walked {
        let s = c.text.strip_prefix(' ').unwrap_or(&c.text);
        body_lines.push(s.trim_end().to_string());
    }
    let replacement = canonical_doc_block(&body_lines, indent_cols, 100);
    // Find the span we're replacing: from line_start of first comment up to
    // end of last comment byte. This preserves the trailing newline after
    // the run (we don't consume it).
    let replace_span = Span::with_offsets(
        line_start,
        last.end_byte,
        first.line,
        1,
    );
    let fix = vec![FixEdit {
        span: replace_span,
        replacement,
    }];
    let (prefix, suggestion_form): (&str, &str) = match (any_doc, any_plain) {
        (true, false) => ("`///`", "/// lines"),
        (false, true) => ("plain `//`", "// lines adjacent to the definition"),
        _ => (
            "adjacent `//` / `///`",
            "line-comment block adjacent to the definition",
        ),
    };
    diagnostics.push(LintDiagnostic {
        rule: "legacy-doc-comment",
        message: format!("{prefix} doc comment(s) above this item should use `/** */` form"),
        span: Span::with_offsets(first.start_byte, last.end_byte, first.line, 1),
        severity: LintSeverity::Warning,
        suggestion: Some(format!(
            "rewrite the {suggestion_form} as a canonical `/** ... */` block"
        )),
        fix: Some(fix),
    });
}

/// Given a byte offset, walk backward to find the start-of-line byte.
fn line_start_byte(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = offset;
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

fn extract_harndoc(source: &str, span: &Span) -> Option<String> {
    // Recognize only canonical `/** */` doc-block comments directly above the
    // item. The block must end on the line immediately preceding the item
    // (no blank line between). Legacy `///` forms are NOT accepted — they're
    // flagged by the `legacy-doc-comment` lint rule instead.
    let lines: Vec<&str> = source.lines().collect();
    let def_line = span.line.saturating_sub(1);
    if def_line == 0 {
        return None;
    }
    // `def_line` is the item's line, 0-indexed: lines[def_line - 1] is the
    // line directly above. That line must end with `*/`.
    let above_idx = def_line - 1;
    let above = lines.get(above_idx)?.trim_end();
    if !above.ends_with("*/") {
        return None;
    }
    // Walk upward to find the start `/**`. The opening `/**` may be on the
    // same line (compact `/** ... */`) or on an earlier line.
    let above_trim = above.trim_start();
    if above_trim.starts_with("/**") && above_trim.ends_with("*/") && above_trim.len() >= 5 {
        // Compact form: `/** body */`
        let inner = &above_trim[3..above_trim.len() - 2];
        let text = inner.trim();
        return Some(text.to_string());
    }
    let mut start_idx = above_idx;
    loop {
        let cur = lines.get(start_idx)?.trim_start();
        if cur.starts_with("/**") {
            break;
        }
        if start_idx == 0 {
            return None;
        }
        start_idx -= 1;
    }
    let mut body = Vec::new();
    for (i, line) in lines.iter().enumerate().take(above_idx + 1).skip(start_idx) {
        let t = line.trim();
        let stripped: &str = if i == start_idx {
            t.strip_prefix("/**").unwrap_or(t).trim_start()
        } else if i == above_idx {
            // Strip trailing `*/` plus optional leading ` * `.
            let without_tail = t.strip_suffix("*/").unwrap_or(t).trim_end();
            let without_star = without_tail
                .strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(without_tail);
            without_star
        } else {
            t.strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(t)
        };
        body.push(stripped.trim_end().to_string());
    }
    // Trim leading/trailing empty lines.
    while body.first().is_some_and(|s| s.is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|s| s.is_empty()) {
        body.pop();
    }
    if body.is_empty() {
        None
    } else {
        Some(body.join("\n"))
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

/// Extra options for source-aware lint rules. Keeps the main entry points
/// backward compatible while allowing callers that know about the source
/// file's path or want opt-in rules (like `require-file-header`) to turn
/// them on.
#[derive(Debug, Default, Clone)]
pub struct LintOptions<'a> {
    /// Filesystem path of the source being linted (used by rules like
    /// `require-file-header` to derive a title from the basename).
    pub file_path: Option<&'a std::path::Path>,
    /// When true, the opt-in `require-file-header` rule runs. Default false.
    pub require_file_header: bool,
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
    lint_full(
        program,
        disabled_rules,
        source,
        &HashSet::new(),
        &LintOptions::default(),
    )
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
    lint_full(
        program,
        disabled_rules,
        source,
        externally_imported_names,
        &LintOptions::default(),
    )
}

/// Lint with cross-file import awareness plus extra options (path-aware and
/// opt-in rules). Thin wrapper over [`lint_full`].
pub fn lint_with_options(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    options: &LintOptions<'_>,
) -> Vec<LintDiagnostic> {
    lint_full(
        program,
        disabled_rules,
        source,
        externally_imported_names,
        options,
    )
}

fn lint_full(
    program: &[SNode],
    disabled_rules: &[String],
    source: Option<&str>,
    externally_imported_names: &HashSet<String>,
    options: &LintOptions<'_>,
) -> Vec<LintDiagnostic> {
    let mut linter = Linter::new(source);
    linter
        .externally_imported_names
        .clone_from(externally_imported_names);
    linter.lint_program(program);
    if let Some(src) = source {
        if options.require_file_header {
            check_require_file_header(src, options.file_path, &mut linter.diagnostics);
        }
    }
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

/// Emit `blank-line-between-items` diagnostics: whenever two top-level
/// items are adjacent in the source with no blank line between them, the
/// rule fires with an autofix that inserts the blank line at the correct
/// offset. Doc comments immediately preceding an item count as part of the
/// item — the blank line goes above the doc block, not between doc and
/// item.
fn check_blank_line_between_items(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if program.len() < 2 {
        return;
    }
    // Collect all comment tokens keyed by starting line (1-based).
    let comment_tokens = collect_comment_tokens(source);
    let comments_by_line: std::collections::HashMap<usize, &LegacyCommentTok> = comment_tokens
        .iter()
        .map(|c| (c.line, c))
        .collect();

    // Precompute line → byte offset of start of line (1-based lines).
    let line_starts = build_line_starts(source);

    for pair in program.windows(2) {
        let prev = &pair[0];
        let next = &pair[1];

        // Skip consecutive imports: they intentionally stay tight.
        if is_import_item(&prev.node) && is_import_item(&next.node) {
            continue;
        }
        if !is_top_level_item(&prev.node) && !is_import_item(&prev.node) {
            continue;
        }
        if !is_top_level_item(&next.node) && !is_import_item(&next.node) {
            continue;
        }
        if prev.span.line == 0 || next.span.line == 0 {
            continue;
        }

        // Walk upward from `next.span.line - 1` collecting contiguous
        // comment lines (no blank line between them and the item). The
        // "first line of the item-with-its-doc-block" is the topmost such
        // line.
        let mut first_line = next.span.line;
        let mut probe = next.span.line;
        while probe > 1 {
            let above = probe - 1;
            if comments_by_line.contains_key(&above) {
                first_line = above;
                probe = above;
                continue;
            }
            break;
        }

        let prev_end_line = prev.span.end_line.max(prev.span.line);
        // Number of source lines that sit strictly between the end of prev
        // and the start of (next + its glued comment block). If there is
        // at least one fully-blank line between them the rule is satisfied.
        if first_line <= prev_end_line + 1 {
            // Adjacent — no blank line. Check that the line literally
            // following prev is non-blank, otherwise we'd be flagging a
            // case that already has blank space.
            let insert_line = prev_end_line + 1;
            let Some(&insert_offset) = line_starts.get(insert_line.saturating_sub(1)) else {
                continue;
            };
            let span = Span::with_offsets(
                insert_offset,
                insert_offset,
                insert_line,
                1,
            );
            diagnostics.push(LintDiagnostic {
                rule: "blank-line-between-items",
                message: "top-level items should be separated by a blank line".to_string(),
                span,
                severity: LintSeverity::Warning,
                suggestion: Some(
                    "insert a blank line above the next item (doc comments \
                     stay glued to the item they describe)"
                        .to_string(),
                ),
                fix: Some(vec![FixEdit {
                    span,
                    replacement: "\n".to_string(),
                }]),
            });
        }
    }
}

/// Emit `trailing-comma` diagnostics by scanning the source's tokens for
/// multiline comma-separated lists that lack a trailing comma. Autofix
/// inserts a `,` at the byte offset immediately after the last item.
fn check_trailing_comma(source: &str, diagnostics: &mut Vec<LintDiagnostic>) {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return;
    };

    // Stack of open-bracket records. Each entry remembers the start-line of
    // the opener, the opener's byte position, which kind it is, and whether
    // we've seen any `,` at this depth (= "this is a comma-separated list").
    #[derive(Clone, Copy)]
    enum Opener {
        Paren,
        Bracket,
        Brace,
    }
    struct Frame {
        opener: Opener,
        open_line: usize,
        saw_comma: bool,
        /// True when we've decided this `{ ... }` is a dict/struct literal
        /// (applies only when `opener == Brace`). For Paren/Bracket this is
        /// always `true` — those are always comma-lists when they have
        /// commas.
        eligible: bool,
        /// For `{ ... }` we need to look at the first "meaningful" token to
        /// decide eligibility. Track whether we've made that decision yet.
        decision_made: bool,
        /// Track the first-seen identifier/string token inside, to combine
        /// with a subsequent `:` for the dict/struct decision.
        pending_key_token: bool,
    }
    let mut stack: Vec<Frame> = Vec::new();

    // Helper to find the byte offset of the last non-whitespace,
    // non-comment character strictly before `pos` in `source`.
    fn last_meaningful_byte_before(source: &str, pos: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        if pos == 0 {
            return None;
        }
        let mut i = pos;
        while i > 0 {
            i -= 1;
            let b = bytes[i];
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                continue;
            }
            // We don't strip comments here — the FixEdit is allowed to land
            // after the comment if a trailing comment sits above the close.
            return Some(i);
        }
        None
    }

    for tok in &tokens {
        // Ignore comments and newlines for the decision-making layer, but
        // we still look at everything for bracket tracking.
        match &tok.kind {
            harn_lexer::TokenKind::LineComment { .. }
            | harn_lexer::TokenKind::BlockComment { .. }
            | harn_lexer::TokenKind::Newline => continue,
            _ => {}
        }

        match &tok.kind {
            harn_lexer::TokenKind::LParen => {
                stack.push(Frame {
                    opener: Opener::Paren,
                    open_line: tok.span.line,
                    saw_comma: false,
                    eligible: true,
                    decision_made: true,
                    pending_key_token: false,
                });
            }
            harn_lexer::TokenKind::LBracket => {
                stack.push(Frame {
                    opener: Opener::Bracket,
                    open_line: tok.span.line,
                    saw_comma: false,
                    eligible: true,
                    decision_made: true,
                    pending_key_token: false,
                });
            }
            harn_lexer::TokenKind::LBrace => {
                stack.push(Frame {
                    opener: Opener::Brace,
                    open_line: tok.span.line,
                    saw_comma: false,
                    eligible: false,
                    decision_made: false,
                    pending_key_token: false,
                });
            }
            harn_lexer::TokenKind::RParen
            | harn_lexer::TokenKind::RBracket
            | harn_lexer::TokenKind::RBrace => {
                let Some(frame) = stack.pop() else { continue };
                // Only care if opener matches (sanity check).
                let matching = matches!(
                    (&frame.opener, &tok.kind),
                    (Opener::Paren, harn_lexer::TokenKind::RParen)
                        | (Opener::Bracket, harn_lexer::TokenKind::RBracket)
                        | (Opener::Brace, harn_lexer::TokenKind::RBrace)
                );
                if !matching {
                    continue;
                }
                if !frame.eligible || !frame.saw_comma {
                    continue;
                }
                // Must be multiline: closer on a later line than opener.
                if tok.span.line <= frame.open_line {
                    continue;
                }
                let close_pos = tok.span.start;
                let Some(last_byte) = last_meaningful_byte_before(source, close_pos) else {
                    continue;
                };
                if source.as_bytes()[last_byte] == b',' {
                    continue; // already has trailing comma
                }
                // Don't fire on genuinely-empty containers. We already
                // required `saw_comma == true`, so that's handled, but
                // defensively check that there's real content.
                let insert_pos = last_byte + 1;
                let span = Span::with_offsets(insert_pos, insert_pos, tok.span.line, 1);
                diagnostics.push(LintDiagnostic {
                    rule: "trailing-comma",
                    message: "multiline comma-separated list is missing a trailing comma"
                        .to_string(),
                    span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("add a trailing comma after the last item".to_string()),
                    fix: Some(vec![FixEdit {
                        span,
                        replacement: ",".to_string(),
                    }]),
                });
            }
            harn_lexer::TokenKind::Comma => {
                if let Some(top) = stack.last_mut() {
                    top.saw_comma = true;
                }
            }
            harn_lexer::TokenKind::Colon => {
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace)
                        && !top.decision_made
                        && top.pending_key_token
                    {
                        top.eligible = true;
                        top.decision_made = true;
                    }
                }
            }
            harn_lexer::TokenKind::Identifier(_)
            | harn_lexer::TokenKind::StringLiteral(_) => {
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace) && !top.decision_made {
                        top.pending_key_token = true;
                    }
                }
            }
            _ => {
                // Any other token inside a `{ ... }` before we decided —
                // means this isn't a dict/struct literal (probably a
                // block). Lock the decision as ineligible.
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace) && !top.decision_made {
                        top.decision_made = true;
                        top.eligible = false;
                    }
                }
            }
        }
    }
}

/// Emit `import-order` diagnostics when the program's import block is not
/// in canonical order (stdlib first, alphabetical by path, selective
/// imports after bare imports for the same path). Autofix rewrites the
/// whole import block in canonical order.
fn check_import_order(source: &str, program: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    let mut imports: Vec<&SNode> = Vec::new();
    for node in program {
        if is_import_item(&node.node) {
            imports.push(node);
        } else {
            break; // imports live at the top of the file
        }
    }
    if imports.len() < 2 {
        return;
    }
    let mut sorted = imports.clone();
    sorted.sort_by(|a, b| import_sort_key(a).cmp(&import_sort_key(b)));
    let already_sorted = imports
        .iter()
        .zip(sorted.iter())
        .all(|(a, b)| std::ptr::eq(*a, *b));
    if already_sorted {
        return;
    }

    // Autofix: replace the span from the first import's start to the last
    // import's end with the canonical sorted rendering. Preserve one
    // newline between imports. The existing formatter handles exact
    // formatting; we just need something functional.
    let first = imports.first().unwrap();
    let last = imports.last().unwrap();
    let replacement = sorted
        .iter()
        .map(|n| render_import_source(source, n))
        .collect::<Vec<_>>()
        .join("\n");
    let replace_span = Span::with_offsets(
        first.span.start,
        last.span.end,
        first.span.line,
        first.span.column,
    );
    diagnostics.push(LintDiagnostic {
        rule: "import-order",
        message: "imports are not in canonical order (stdlib first, then alphabetical by path)"
            .to_string(),
        span: replace_span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "reorder imports: std/ first, then third-party and local paths alphabetically"
                .to_string(),
        ),
        fix: Some(vec![FixEdit {
            span: replace_span,
            replacement,
        }]),
    });
}

fn import_sort_key(node: &SNode) -> (u8, String, u8, String) {
    match &node.node {
        Node::ImportDecl { path } => (
            u8::from(!path.starts_with("std/")),
            path.clone(),
            0,
            String::new(),
        ),
        Node::SelectiveImport { names, path } => {
            let mut sorted_names = names.clone();
            sorted_names.sort();
            (
                u8::from(!path.starts_with("std/")),
                path.clone(),
                1,
                sorted_names.join(","),
            )
        }
        _ => (2, String::new(), 2, String::new()),
    }
}

/// Slice the raw source covered by an import node's span. The formatter
/// will re-normalize these in a subsequent pass; for the autofix we just
/// need the existing text in the correct order.
fn render_import_source(source: &str, node: &SNode) -> String {
    source
        .get(node.span.start..node.span.end)
        .unwrap_or("")
        .to_string()
}

/// Emit `require-file-header` when the source does not begin with a
/// `/** */` doc block. Autofix inserts a canonical header at byte 0 with a
/// title derived from the filename (when a path is provided).
fn check_require_file_header(
    source: &str,
    file_path: Option<&std::path::Path>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // Find the first non-whitespace character in the source and verify
    // it's the start of a `/**` block. Any other content (including plain
    // `//` line comments, `/*` non-doc block comments, or code) is a
    // violation.
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i + 2 < bytes.len() && &bytes[i..i + 3] == b"/**" {
        return;
    }
    let title = derive_file_header_title(file_path);
    let header = format!("/**\n * {title}\n */\n\n");
    let span = Span::with_offsets(0, 0, 1, 1);
    diagnostics.push(LintDiagnostic {
        rule: "require-file-header",
        message: "file is missing a `/** */` header doc block".to_string(),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(format!(
            "add a `/** <title> */` block at the top of the file (e.g. `{title}`)"
        )),
        fix: Some(vec![FixEdit {
            span,
            replacement: header,
        }]),
    });
}

/// Derive the title shown inside the autofix's file-header block. Falls
/// back to a generic "Module." when no path is available.
pub fn derive_file_header_title(file_path: Option<&std::path::Path>) -> String {
    let stem = file_path
        .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
        .unwrap_or("module");
    // Replace `-` and `_` with spaces, then capitalize the first letter
    // only. We intentionally do NOT title-case every word — the plan
    // specifies "first letter capitalized" for readability.
    let mut cleaned = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch == '-' || ch == '_' {
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    // Normalize whitespace: collapse internal runs of spaces.
    let collapsed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut trimmed = collapsed.trim().to_string();
    if trimmed.is_empty() {
        trimmed.push_str("module");
    }
    // Capitalize the very first character.
    let mut chars = trimmed.chars();
    let head = chars.next().unwrap().to_ascii_uppercase();
    let tail: String = chars.collect();
    let mut out = String::new();
    out.push(head);
    out.push_str(&tail.to_lowercase());
    // Append a terminal period if the title does not already end in one
    // of `.`, `!`, or `?`.
    let last = out.chars().last().unwrap_or('.');
    if !matches!(last, '.' | '!' | '?') {
        out.push('.');
    }
    out
}

/// Build a Vec where index `i` is the byte offset of the start of line
/// `i + 1` (1-based lines). Convenient for constructing zero-width FixEdit
/// spans at "beginning of line N".
fn build_line_starts(source: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    for (idx, ch) in source.char_indices() {
        if ch == '\n' {
            starts.push(idx + 1);
        }
    }
    starts
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
