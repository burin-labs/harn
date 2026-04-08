use std::collections::HashSet;

use harn_lexer::{FixEdit, Span, StringSegment};
use harn_parser::diagnostic::find_closest_match;
use harn_parser::{BindingPattern, Node, SNode};

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
        for name in Self::pattern_names(pattern) {
            self.declare_variable(&name, span, is_mutable);
        }
    }

    /// Declare a variable in the current scope, checking for shadowing.
    fn declare_variable(&mut self, name: &str, span: Span, is_mutable: bool) {
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
                self.lint_block(body);
                self.pop_scope();
            }

            Node::FnDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => {
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
                body,
                is_pub,
                ..
            } => {
                self.known_functions.insert(name.clone());
                self.fn_declarations.push(FnDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    is_pub: *is_pub,
                    is_method: false,
                });
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

            Node::ImplBlock { methods, .. } => {
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
                self.lint_node(object);
            }

            Node::SubscriptAccess { object, index } => {
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
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.lint_node(condition);
                // Check empty then-block.
                if then_body.is_empty() {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "if block has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty if or add a body".to_string()),
                        fix: None,
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
                    self.diagnostics.push(LintDiagnostic {
                        rule: "empty-block",
                        message: "for loop has an empty body".to_string(),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the empty for loop or add a body".to_string()),
                        fix: None,
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
                        if arm.pattern.node == earlier.pattern.node {
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
                count,
                body,
                variable,
                ..
            } => {
                self.lint_node(count);
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

            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                self.lint_node(list);
                self.push_scope();
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(variable.clone());
                }
                self.references.insert(variable.clone());
                self.lint_block(body);
                self.pop_scope();
            }

            Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                self.lint_node(list);
                self.push_scope();
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(variable.clone());
                }
                self.references.insert(variable.clone());
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

            Node::AskExpr { fields } => {
                for entry in fields {
                    self.lint_node(&entry.key);
                    self.lint_node(&entry.value);
                }
            }

            Node::YieldExpr { value } => {
                if let Some(v) = value {
                    self.lint_node(v);
                }
            }

            Node::EnumConstruct { args, .. } => {
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::StructConstruct { fields, .. } => {
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

            Node::StructDecl { name, .. } => {
                self.known_functions.insert(name.clone());
            }
            Node::EnumDecl { name, .. } => {
                self.known_functions.insert(name.clone());
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

            // Leaf nodes and declarations that don't need recursion.
            Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::DurationLiteral(_)
            | Node::InterfaceDecl { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. }
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

            if matches!(
                node.node,
                Node::ReturnStmt { .. }
                    | Node::ThrowStmt { .. }
                    | Node::BreakStmt
                    | Node::ContinueStmt
            ) {
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
                self.diagnostics.push(LintDiagnostic {
                    rule: "unused-variable",
                    message: format!("variable `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("prefix with underscore: `_{}`", decl.name)),
                    fix: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn lint_source(source: &str) -> Vec<LintDiagnostic> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        lint_with_source(&program, source)
    }

    fn has_rule(diagnostics: &[LintDiagnostic], rule: &str) -> bool {
        diagnostics.iter().any(|d| d.rule == rule)
    }

    fn count_rule(diagnostics: &[LintDiagnostic], rule: &str) -> usize {
        diagnostics.iter().filter(|d| d.rule == rule).count()
    }

    #[test]
    fn test_clean_code() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    log(x)
}
"#,
        );
        // x is used, task is a pipeline param -- should be clean.
        assert!(
            !has_rule(&diags, "unused-variable"),
            "expected no unused-variable, got: {diags:?}"
        );
    }

    #[test]
    fn test_public_function_requires_harndoc() {
        let diags = lint_source(
            r#"
pub fn exposed() -> string {
  return "x"
}
"#,
        );
        assert!(has_rule(&diags, "missing-harndoc"));
    }

    #[test]
    fn test_public_function_with_harndoc_is_clean() {
        let diags = lint_source(
            r#"
/// Explain the public API.
pub fn exposed() -> string {
  return "x"
}
"#,
        );
        assert!(!has_rule(&diags, "missing-harndoc"));
    }

    #[test]
    fn test_plain_comment_does_not_satisfy_harndoc() {
        let diags = lint_source(
            r#"
// Not HarnDoc.
pub fn exposed() -> string {
  return "x"
}
"#,
        );
        assert!(has_rule(&diags, "missing-harndoc"));
    }

    #[test]
    fn test_private_function_does_not_require_harndoc() {
        let diags = lint_source(
            r#"
fn helper() -> string {
  return "x"
}
"#,
        );
        assert!(!has_rule(&diags, "missing-harndoc"));
    }

    #[test]
    fn test_unused_variable() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let unused = 42
    log("hello")
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-variable"),
            "expected unused-variable warning, got: {diags:?}"
        );
    }

    #[test]
    fn test_unused_underscore_ignored() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let _ = 42
    log("hello")
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-variable"),
            "underscore variables should not trigger unused-variable: {diags:?}"
        );
    }

    #[test]
    fn test_unreachable_code() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    return 1
    log("never reached")
}
"#,
        );
        assert!(
            has_rule(&diags, "unreachable-code"),
            "expected unreachable-code warning, got: {diags:?}"
        );
    }

    #[test]
    fn test_no_unreachable_when_return_is_last() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    log("hello")
    return 1
}
"#,
        );
        assert!(
            !has_rule(&diags, "unreachable-code"),
            "return at end should not trigger unreachable-code: {diags:?}"
        );
    }

    #[test]
    fn test_mutable_never_reassigned() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    var x = 1
    log(x)
}
"#,
        );
        assert!(
            has_rule(&diags, "mutable-never-reassigned"),
            "expected mutable-never-reassigned warning, got: {diags:?}"
        );
    }

    #[test]
    fn test_mutable_reassigned_ok() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    var x = 1
    x = 2
    log(x)
}
"#,
        );
        assert!(
            !has_rule(&diags, "mutable-never-reassigned"),
            "reassigned var should not trigger mutable-never-reassigned: {diags:?}"
        );
    }

    #[test]
    fn test_empty_block_if() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    if true {
    }
}
"#,
        );
        assert!(
            has_rule(&diags, "empty-block"),
            "expected empty-block warning for if, got: {diags:?}"
        );
    }

    #[test]
    fn test_shadow_variable() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    if true {
        let x = 2
        log(x)
    }
    log(x)
}
"#,
        );
        assert!(
            has_rule(&diags, "shadow-variable"),
            "expected shadow-variable warning, got: {diags:?}"
        );
    }

    #[test]
    fn test_no_shadow_same_scope() {
        // Re-declaration in the same scope is not shadowing (it may be a
        // parser error, but the linter only checks outer-scope shadows).
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    log(x)
}
"#,
        );
        assert!(
            !has_rule(&diags, "shadow-variable"),
            "same-scope should not trigger shadow-variable: {diags:?}"
        );
    }

    #[test]
    fn test_unreachable_after_throw() {
        let diags = lint_source("pipeline t(task) { throw \"err\"\nlog(\"unreachable\") }");
        assert!(
            diags.iter().any(|d| d.rule == "unreachable-code"),
            "expected unreachable-code after throw, got: {diags:?}"
        );
    }

    #[test]
    fn test_unused_fn_param() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn greet(name, unused) {
        log(name)
    }
    greet("hi", "there")
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-parameter"),
            "expected unused-parameter for unused fn param, got: {diags:?}"
        );
        // Should NOT trigger unused-variable (parameters are tracked separately).
        assert!(
            !has_rule(&diags, "unused-variable"),
            "unused fn param should not trigger unused-variable: {diags:?}"
        );
    }

    #[test]
    fn test_unused_closure_param() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let f = { x, y -> log(x) }
    f(1, 2)
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-parameter"),
            "expected unused-parameter for unused closure param, got: {diags:?}"
        );
    }

    #[test]
    fn test_unused_param_underscore_prefix_ignored() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn greet(name, _unused) {
        log(name)
    }
    greet("hi", "there")
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-parameter"),
            "underscore-prefixed params should not trigger unused-parameter: {diags:?}"
        );
    }

    #[test]
    fn test_used_fn_param_ok() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn add(a, b) {
        return a + b
    }
    log(add(1, 2))
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-parameter"),
            "used params should not trigger unused-parameter: {diags:?}"
        );
    }

    #[test]
    fn test_multiple_rules() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    var unused = 1
    return 0
    log("dead")
}
"#,
        );
        assert!(has_rule(&diags, "unused-variable"));
        assert!(has_rule(&diags, "mutable-never-reassigned"));
        assert!(has_rule(&diags, "unreachable-code"));
        assert_eq!(count_rule(&diags, "unreachable-code"), 1);
    }

    #[test]
    fn test_comparison_to_bool_true() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = true
    if x == true { log("yes") }
}
"#,
        );
        assert!(
            has_rule(&diags, "comparison-to-bool"),
            "expected comparison-to-bool, got: {diags:?}"
        );
    }

    #[test]
    fn test_comparison_to_bool_false() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = true
    if x == false { log("no") }
}
"#,
        );
        assert!(
            has_rule(&diags, "comparison-to-bool"),
            "expected comparison-to-bool, got: {diags:?}"
        );
    }

    #[test]
    fn test_no_comparison_to_bool_for_normal() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    if x == 1 { log("one") }
}
"#,
        );
        assert!(
            !has_rule(&diags, "comparison-to-bool"),
            "should not trigger for non-bool comparison: {diags:?}"
        );
    }

    #[test]
    fn test_unnecessary_else_return() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    if x == 1 {
        return "one"
    } else {
        return "other"
    }
}
"#,
        );
        assert!(
            has_rule(&diags, "unnecessary-else-return"),
            "expected unnecessary-else-return, got: {diags:?}"
        );
    }

    #[test]
    fn test_no_unnecessary_else_return_when_no_return() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    if x == 1 {
        log("one")
    } else {
        log("other")
    }
}
"#,
        );
        assert!(
            !has_rule(&diags, "unnecessary-else-return"),
            "should not trigger when branches don't return: {diags:?}"
        );
    }

    #[test]
    fn test_duplicate_match_arm() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1
    match x {
        1 -> { log("one") }
        1 -> { log("also one") }
        _ -> { log("other") }
    }
}
"#,
        );
        assert!(
            has_rule(&diags, "duplicate-match-arm"),
            "expected duplicate-match-arm, got: {diags:?}"
        );
    }

    #[test]
    fn test_break_outside_loop() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    break
}
"#,
        );
        assert!(
            has_rule(&diags, "break-outside-loop"),
            "expected break-outside-loop, got: {diags:?}"
        );
    }

    #[test]
    fn test_break_inside_loop_ok() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    while true {
        break
    }
}
"#,
        );
        assert!(
            !has_rule(&diags, "break-outside-loop"),
            "break inside loop should be fine: {diags:?}"
        );
    }

    #[test]
    fn test_unreachable_after_break() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    while true {
        break
        log("unreachable")
    }
}
"#,
        );
        assert!(
            has_rule(&diags, "unreachable-code"),
            "expected unreachable-code after break, got: {diags:?}"
        );
    }

    // ===== unused-function tests =====

    #[test]
    fn test_unused_function_basic() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn helper() {
        return 42
    }
    log("hello")
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-function"),
            "expected unused-function warning, got: {diags:?}"
        );
    }

    #[test]
    fn test_used_function_no_warning() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn helper() {
        return 42
    }
    log(helper())
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "used function should not trigger unused-function: {diags:?}"
        );
    }

    #[test]
    fn test_pub_function_exempt() {
        let diags = lint_source(
            r#"
/// Documented public function.
pub fn api_endpoint() {
    return "ok"
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "pub functions should be exempt: {diags:?}"
        );
    }

    #[test]
    fn test_function_passed_as_value() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn transformer(x) {
        return x * 2
    }
    let f = transformer
    log(f(5))
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "function referenced as value should not trigger: {diags:?}"
        );
    }

    #[test]
    fn test_function_called_from_another_function() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn inner() {
        return 42
    }
    fn outer() {
        return inner()
    }
    log(outer())
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "function called from another function should not trigger: {diags:?}"
        );
    }

    #[test]
    fn test_pipeline_not_flagged_as_unused() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    log("hello")
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "pipelines should never trigger unused-function: {diags:?}"
        );
    }

    #[test]
    fn test_impl_methods_exempt() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    struct Point {
        x: int
        y: int
    }
    impl Point {
        fn distance(self) {
            return self.x + self.y
        }
    }
    let p = Point({x: 3, y: 4})
    log(p)
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "impl methods should be exempt: {diags:?}"
        );
    }

    #[test]
    fn test_recursive_function_called_externally() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn factorial(n) {
        if n <= 1 {
            return 1
        }
        return n * factorial(n - 1)
    }
    log(factorial(5))
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "recursive function called externally should not trigger: {diags:?}"
        );
    }

    #[test]
    fn test_mutually_recursive_functions_one_called() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn is_even(n) {
        if n == 0 { return true }
        return is_odd(n - 1)
    }
    fn is_odd(n) {
        if n == 0 { return false }
        return is_even(n - 1)
    }
    log(is_even(4))
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "mutually recursive functions where one is called should not trigger: {diags:?}"
        );
    }

    #[test]
    fn test_underscore_prefixed_function_exempt() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn _unused_helper() {
        return 42
    }
    log("hello")
}
"#,
        );
        assert!(
            !has_rule(&diags, "unused-function"),
            "underscore-prefixed functions should be exempt: {diags:?}"
        );
    }

    #[test]
    fn test_unused_function_suggestion_message() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn helper() {
        return 42
    }
    log("hello")
}
"#,
        );
        let unused = diags
            .iter()
            .find(|d| d.rule == "unused-function")
            .expect("expected unused-function diagnostic");
        assert!(unused.message.contains("helper"));
        assert!(unused.suggestion.as_ref().unwrap().contains("_helper"));
    }

    #[test]
    fn test_multiple_unused_functions() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    fn helper1() { return 1 }
    fn helper2() { return 2 }
    fn used() { return 3 }
    log(used())
}
"#,
        );
        assert_eq!(
            count_rule(&diags, "unused-function"),
            2,
            "expected 2 unused-function warnings, got: {diags:?}"
        );
    }

    #[test]
    fn test_top_level_unused_function() {
        let diags = lint_source(
            r#"
fn orphan() {
    return 42
}
pipeline default(task) {
    log("hello")
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-function"),
            "top-level unused function should trigger: {diags:?}"
        );
    }

    #[test]
    fn test_unused_function_with_wildcard_import() {
        // Wildcard imports shouldn't suppress unused-function checks —
        // external code can't call local non-pub functions.
        let diags = lint_source(
            r#"
import "some_module"
pipeline default(task) {
    fn helper() { return 1 }
    log("hello")
}
"#,
        );
        assert!(
            has_rule(&diags, "unused-function"),
            "unused-function should still fire with wildcard imports: {diags:?}"
        );
    }

    #[test]
    fn test_unused_function_suppressed_by_cross_file_imports() {
        // When another file imports a function by name, it should not be
        // flagged as unused even if it has no local references.
        let source = r###"
fn done_sentinel() { return "##DONE##" }
fn truly_unused() { return 1 }
"###;
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();

        // Without cross-file info: both flagged
        let diags = lint_with_config_and_source(&program, &[], Some(source));
        assert_eq!(
            count_rule(&diags, "unused-function"),
            2,
            "both functions should be flagged without cross-file info: {diags:?}"
        );

        // With cross-file info: only truly_unused flagged
        let mut imported = HashSet::new();
        imported.insert("done_sentinel".to_string());
        let diags = lint_with_cross_file_imports(&program, &[], Some(source), &imported);
        assert_eq!(
            count_rule(&diags, "unused-function"),
            1,
            "only truly_unused should be flagged: {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.rule == "unused-function" && d.message.contains("truly_unused")),
            "the remaining warning should be for truly_unused: {diags:?}"
        );
    }

    #[test]
    fn test_invalid_binary_op_literal_bool() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = true + 1
    log(x)
}
"#,
        );
        assert!(
            has_rule(&diags, "invalid-binary-op-literal"),
            "expected invalid-binary-op-literal for bool in arithmetic: {diags:?}"
        );
    }

    #[test]
    fn test_invalid_binary_op_literal_nil() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = nil - 5
    log(x)
}
"#,
        );
        assert!(
            has_rule(&diags, "invalid-binary-op-literal"),
            "expected invalid-binary-op-literal for nil in arithmetic: {diags:?}"
        );
    }

    #[test]
    fn test_no_invalid_binary_op_for_valid_types() {
        let diags = lint_source(
            r#"
pipeline default(task) {
    let x = 1 + 2
    let y = "a" + "b"
    log(x)
    log(y)
}
"#,
        );
        assert!(
            !has_rule(&diags, "invalid-binary-op-literal"),
            "should not fire for valid operand types: {diags:?}"
        );
    }

    #[test]
    fn test_collect_selective_import_names() {
        let source = r#"
import { foo, bar } from "module_a"
import { baz } from "module_b"
import "wildcard_module"
fn local() { return foo() + bar() + baz() }
"#;
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();

        let names = collect_selective_import_names(&program);
        assert!(names.contains("foo"), "should contain foo");
        assert!(names.contains("bar"), "should contain bar");
        assert!(names.contains("baz"), "should contain baz");
        assert_eq!(names.len(), 3, "should have exactly 3 names: {names:?}");
    }

    // -----------------------------------------------------------------------
    // Autofix tests
    // -----------------------------------------------------------------------

    /// Get the first fix for a given rule, or None.
    fn get_fix(diagnostics: &[LintDiagnostic], rule: &str) -> Option<Vec<FixEdit>> {
        diagnostics
            .iter()
            .find(|d| d.rule == rule)
            .and_then(|d| d.fix.clone())
    }

    /// Apply all non-overlapping fixes to the source (reverse order).
    fn apply_fixes(source: &str, diagnostics: &[LintDiagnostic]) -> String {
        let mut edits: Vec<&FixEdit> = diagnostics
            .iter()
            .filter_map(|d| d.fix.as_ref())
            .flatten()
            .collect();
        edits.sort_by(|a, b| b.span.start.cmp(&a.span.start));
        let mut accepted: Vec<&FixEdit> = Vec::new();
        for edit in &edits {
            let overlaps = accepted
                .iter()
                .any(|prev| edit.span.start < prev.span.end && edit.span.end > prev.span.start);
            if !overlaps {
                accepted.push(edit);
            }
        }
        let mut result = source.to_string();
        for edit in &accepted {
            let before = &result[..edit.span.start];
            let after = &result[edit.span.end..];
            result = format!("{before}{}{after}", edit.replacement);
        }
        result
    }

    #[test]
    fn test_fix_mutable_never_reassigned() {
        let source = "pipeline default(task) {\n  var x = 10\n  log(x)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "mutable-never-reassigned");
        assert!(fix.is_some(), "expected fix for mutable-never-reassigned");
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("let x = 10"),
            "expected var→let, got: {result}"
        );
        assert!(
            !result.contains("var x"),
            "var should be replaced, got: {result}"
        );
    }

    #[test]
    fn test_fix_comparison_to_bool_true() {
        let source = "pipeline default(task) {\n  let x = true\n  let y = x == true\n  log(y)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "comparison-to-bool");
        assert!(fix.is_some(), "expected fix for comparison-to-bool");
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("let y = x"),
            "expected simplified comparison, got: {result}"
        );
        assert!(
            !result.contains("== true"),
            "should remove == true, got: {result}"
        );
    }

    #[test]
    fn test_fix_comparison_to_bool_false() {
        let source = "pipeline default(task) {\n  let x = true\n  let y = x == false\n  log(y)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "comparison-to-bool");
        assert!(fix.is_some(), "expected fix for comparison-to-bool");
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("let y = !x"),
            "expected negated, got: {result}"
        );
    }

    #[test]
    fn test_fix_comparison_to_bool_ne_true() {
        let source = "pipeline default(task) {\n  let x = true\n  let y = x != true\n  log(y)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "comparison-to-bool");
        assert!(fix.is_some(), "expected fix for comparison-to-bool");
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("let y = !x"),
            "!= true should become !x, got: {result}"
        );
    }

    #[test]
    fn test_fix_unused_import_all_unused() {
        let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(task)\n}";
        let diags = lint_source(source);
        assert!(
            count_rule(&diags, "unused-import") >= 1,
            "expected unused-import warnings"
        );
        // When all names are unused, the fix should remove the entire import line
        let fix = get_fix(&diags, "unused-import");
        assert!(fix.is_some(), "expected fix for unused-import");
        let edits = fix.unwrap();
        assert_eq!(edits.len(), 1);
        assert!(
            edits[0].replacement.is_empty(),
            "expected deletion, got: {:?}",
            edits[0].replacement
        );
    }

    #[test]
    fn test_fix_unused_import_partial() {
        let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(foo)\n}";
        let diags = lint_source(source);
        // bar is unused, foo is used
        assert_eq!(
            count_rule(&diags, "unused-import"),
            1,
            "expected 1 unused-import warning"
        );
        let fix = get_fix(&diags, "unused-import");
        assert!(fix.is_some(), "expected fix for unused-import");
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("{ foo }") || result.contains("{foo}"),
            "expected bar removed from import, got: {result}"
        );
        assert!(
            !result.contains("bar"),
            "bar should be removed, got: {result}"
        );
    }

    #[test]
    fn test_fix_invalid_binop_string_plus_bool() {
        let source = "pipeline default(task) {\n  let x = \"hello\" + true\n  log(x)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "invalid-binary-op-literal");
        assert!(
            fix.is_some(),
            "expected interpolation fix for string + bool"
        );
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("\"hello${true}\""),
            "expected interpolation, got: {result}"
        );
    }

    #[test]
    fn test_fix_invalid_binop_no_fix_for_non_string() {
        let source = "pipeline default(task) {\n  let x = true + 1\n  log(x)\n}";
        let diags = lint_source(source);
        let fix = get_fix(&diags, "invalid-binary-op-literal");
        assert!(
            fix.is_none(),
            "should not offer fix for non-string binop, got: {fix:?}"
        );
    }

    #[test]
    fn test_fix_multiple_fixes_applied() {
        let source = "pipeline default(task) {\n  var x = 10\n  let y = x == true\n  log(y)\n}";
        let diags = lint_source(source);
        let result = apply_fixes(source, &diags);
        assert!(
            result.contains("let x = 10"),
            "var should be fixed to let, got: {result}"
        );
        assert!(
            result.contains("let y = x"),
            "comparison should be simplified, got: {result}"
        );
    }

    #[test]
    fn test_no_fix_when_source_unavailable() {
        // lint without source — fixes should be None
        let source = "pipeline default(task) {\n  var x = 10\n  log(x)\n}";
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let diags = lint(&program); // no source
        let fix = get_fix(&diags, "mutable-never-reassigned");
        assert!(
            fix.is_none(),
            "without source, fix should be None, got: {fix:?}"
        );
    }
}
