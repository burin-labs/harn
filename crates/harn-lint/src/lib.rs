use std::collections::HashSet;

use harn_lexer::Span;
use harn_parser::{Node, SNode};

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

/// The linter walks the AST and collects diagnostics.
struct Linter {
    diagnostics: Vec<LintDiagnostic>,
    scopes: Vec<HashSet<String>>,
    declarations: Vec<Declaration>,
    references: HashSet<String>,
    assignments: HashSet<String>,
}

impl Linter {
    fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            scopes: vec![HashSet::new()],
            declarations: Vec::new(),
            references: HashSet::new(),
            assignments: HashSet::new(),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declare a variable in the current scope, checking for shadowing.
    fn declare_variable(&mut self, name: &str, span: Span, is_mutable: bool) {
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

        self.declarations.push(Declaration {
            name: name.to_string(),
            span,
            is_mutable,
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
                self.push_scope();
                // Pipeline params are implicitly "used" -- don't report them.
                for p in params {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(p.clone());
                    }
                    // Mark pipeline params as referenced so they are never
                    // flagged as unused.
                    self.references.insert(p.clone());
                }
                // The pipeline name itself is a declaration in the outer scope,
                // but we don't lint pipeline names as unused.
                self.references.insert(name.clone());
                self.lint_block(body);
                self.pop_scope();
            }

            Node::FnDecl {
                name, params, body, ..
            } => {
                // The function name itself is referenced (callable).
                self.references.insert(name.clone());
                self.push_scope();
                for p in params {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(p.name.clone());
                    }
                    self.references.insert(p.name.clone());
                }
                self.lint_block(body);
                self.pop_scope();
            }

            Node::LetBinding { name, value, .. } => {
                self.lint_node(value);
                self.declare_variable(name, snode.span, false);
            }

            Node::VarBinding { name, value, .. } => {
                self.lint_node(value);
                self.declare_variable(name, snode.span, true);
            }

            Node::Assignment { target, value } => {
                if let Node::Identifier(name) = &target.node {
                    self.assignments.insert(name.clone());
                }
                self.lint_node(target);
                self.lint_node(value);
            }

            Node::Identifier(name) => {
                self.references.insert(name.clone());
            }

            Node::FunctionCall { name, args } => {
                self.references.insert(name.clone());
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::MethodCall { object, args, .. } => {
                self.lint_node(object);
                for arg in args {
                    self.lint_node(arg);
                }
            }

            Node::PropertyAccess { object, .. } => {
                self.lint_node(object);
            }

            Node::SubscriptAccess { object, index } => {
                self.lint_node(object);
                self.lint_node(index);
            }

            Node::BinaryOp { left, right, .. } => {
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
                variable,
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
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(variable.clone());
                }
                self.references.insert(variable.clone());
                self.lint_block(body);
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
                self.lint_block(body);
                self.pop_scope();
            }

            Node::TryCatch {
                body,
                error_var,
                catch_body,
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
            }

            Node::MatchExpr { value, arms } => {
                self.lint_node(value);
                for arm in arms {
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

            Node::Closure { params, body } => {
                self.push_scope();
                for p in params {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.insert(p.name.clone());
                    }
                    self.references.insert(p.name.clone());
                }
                self.lint_block(body);
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

            Node::InterpolatedString(_) => {}

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

            // Leaf nodes and declarations that don't need recursion.
            Node::StringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::DurationLiteral(_)
            | Node::ImportDecl { .. }
            | Node::EnumDecl { .. }
            | Node::StructDecl { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. } => {}
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

            if matches!(node.node, Node::ReturnStmt { .. } | Node::ThrowStmt { .. }) {
                found_terminator = true;
            }
        }
    }

    /// Run post-walk analysis and finalize diagnostics.
    fn finalize(&mut self) {
        // Rule: unused-variable
        for decl in &self.declarations {
            if decl.name == "_" {
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
    }
}

/// Lint an AST program and return all diagnostics.
pub fn lint(program: &[SNode]) -> Vec<LintDiagnostic> {
    let mut linter = Linter::new();
    linter.lint_program(program);
    linter.finalize();
    linter.diagnostics
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
}
