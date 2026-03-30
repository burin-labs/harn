use std::collections::HashSet;

use harn_lexer::{Span, StringSegment};
use harn_parser::diagnostic::find_closest_match;
use harn_parser::{BindingPattern, Node, SNode};

/// A lint diagnostic reported by the linter.
#[derive(Debug)]
pub struct LintDiagnostic {
    pub rule: &'static str,
    pub message: String,
    pub span: Span,
    pub severity: LintSeverity,
    pub suggestion: Option<String>,
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

/// The linter walks the AST and collects diagnostics.
struct Linter {
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
}

impl Linter {
    fn new() -> Self {
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
                name, params, body, ..
            } => {
                self.known_functions.insert(name.clone());
                self.references.insert(name.clone());
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

            Node::ImplBlock { methods, .. } => {
                for method in methods {
                    self.lint_node(method);
                }
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
            }

            Node::FunctionCall { name, args } => {
                self.references.insert(name.clone());
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
                        self.diagnostics.push(LintDiagnostic {
                            rule: "comparison-to-bool",
                            message: msg.to_string(),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(suggestion.to_string()),
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
                        self.diagnostics.push(LintDiagnostic {
                            rule: "unnecessary-else-return",
                            message: "both if and else branches return — else is unnecessary"
                                .to_string(),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(
                                "remove the else and place its body after the if".to_string(),
                            ),
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
                    if let StringSegment::Expression(expr) = seg {
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
                });
            }
        }

        // Rule: unused-import
        for import in &self.imports {
            for name in &import.names {
                if !self.references.contains(name) {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "unused-import",
                        message: format!("imported name `{name}` is never used"),
                        span: import.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!("remove `{name}` from the import")),
                    });
                }
            }
        }

        // Rule: mutable-never-reassigned
        for decl in &self.declarations {
            if !decl.is_mutable {
                continue;
            }
            if !self.assignments.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    rule: "mutable-never-reassigned",
                    message: format!(
                        "variable `{}` is declared as `var` but never reassigned",
                        decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("use `let` instead of `var`".to_string()),
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
            });
        }
    }
}

/// Lint an AST program and return all diagnostics.
pub fn lint(program: &[SNode]) -> Vec<LintDiagnostic> {
    lint_with_config(program, &[])
}

/// Lint an AST program, filtering out diagnostics for disabled rules.
pub fn lint_with_config(program: &[SNode], disabled_rules: &[String]) -> Vec<LintDiagnostic> {
    let mut linter = Linter::new();
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
        lint(&program)
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
}
