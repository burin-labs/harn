//! The stateful linter walk: a [`Linter`] collects diagnostics while
//! traversing the AST, then finalizes post-walk checks
//! (unused/undefined symbols, etc.). The large `lint_node` match lives
//! in the [`walk`] submodule.

use std::collections::HashSet;

use harn_lexer::{FixEdit, Span};
use harn_parser::diagnostic::find_closest_match;
use harn_parser::{BindingPattern, Node, SNode, TypeExpr, TypedParam};

use crate::complexity::cyclomatic_complexity;
use crate::decls::{Declaration, FnDeclaration, ImportInfo, ParamDeclaration, TypeDeclaration};
use crate::diagnostic::{LintDiagnostic, LintSeverity, DEFAULT_COMPLEXITY_THRESHOLD};
use crate::fixes::{append_sink_fix, simple_ident_rename_fix};
use crate::harndoc::check_legacy_doc_comments;
use crate::naming::{is_pascal_case, is_snake_case, to_pascal_case, to_snake_case};
use crate::rules::blank_lines::check_blank_line_between_items;
use crate::rules::import_order::check_import_order;
use crate::rules::trailing_comma::check_trailing_comma;

mod walk;

/// The linter walks the AST and collects diagnostics.
pub(crate) struct Linter<'a> {
    pub(super) diagnostics: Vec<LintDiagnostic>,
    pub(super) scopes: Vec<HashSet<String>>,
    pub(super) declarations: Vec<Declaration>,
    pub(super) param_declarations: Vec<ParamDeclaration>,
    pub(super) references: HashSet<String>,
    pub(super) assignments: HashSet<String>,
    pub(super) imports: Vec<ImportInfo>,
    /// Track whether we are inside a loop (for break/continue validation).
    pub(super) loop_depth: usize,
    /// Track all declared/known function names for undefined-function detection.
    pub(super) known_functions: HashSet<String>,
    /// Track function call sites for undefined-function checking.
    pub(super) function_calls: Vec<(String, Span)>,
    /// Whether the file has wildcard imports (import "module").
    /// If true, skip undefined-function checks since we can't know what was imported.
    pub(super) has_wildcard_import: bool,
    /// Whether wildcard imports were resolved using [`harn_modules`] and we can
    /// choose between known/wildcard modes explicitly.
    pub(crate) use_module_graph_for_wildcards: bool,
    /// Wildcard export names resolved from [`harn_modules`]. `None` means
    /// unknown, so conservative behavior should skip undefined-function checks.
    pub(crate) module_graph_wildcard_exports: Option<HashSet<String>>,
    /// Track function declarations for unused-function detection.
    pub(super) fn_declarations: Vec<FnDeclaration>,
    /// Track actual function usage sites (calls + value references).
    /// Separate from `references` so FnDecl doesn't self-count.
    pub(super) function_references: HashSet<String>,
    /// Whether the current function is inside an impl block.
    pub(super) in_impl_block: bool,
    pub(super) source: Option<&'a str>,
    /// Function names imported by other files (cross-module analysis).
    /// Functions in this set are not flagged as unused even if they have
    /// no local references, because another file explicitly imports them.
    pub(crate) externally_imported_names: HashSet<String>,
    /// Track whether the current traversal is inside a test pipeline body.
    pub(super) test_pipeline_depth: usize,
    /// Track type declarations for the `unused-type` lint rule.
    pub(super) type_declarations: Vec<TypeDeclaration>,
    /// Track type names referenced anywhere in the file.
    pub(super) type_references: HashSet<String>,
    /// Stack of declared return types for the current function nesting.
    /// Used by the `eager-collection-conversion` lint rule to flag
    /// `return <iter-chain>` inside a function declared to return a
    /// concrete collection.
    pub(super) return_type_stack: Vec<Option<TypeExpr>>,
    /// Tracks how many enclosing `@complexity(allow)` attributes are
    /// active. When > 0, the cyclomatic-complexity rule is suppressed
    /// for the contained function.
    pub(super) complexity_suppression_depth: usize,
    /// Threshold above which the cyclomatic-complexity rule fires.
    /// Configurable via `[lint].complexity_threshold` in `harn.toml`.
    pub(crate) complexity_threshold: usize,
    /// Suppress the discarded-approval-result lint for the final expression
    /// in value-producing blocks such as `try { ... }`.
    pub(super) value_block_depth: usize,
    /// Stack of connector exports whose default effect policy restricts
    /// direct builtin calls.
    pub(super) connector_effect_export_stack: Vec<String>,
}

impl<'a> Linter<'a> {
    pub(crate) fn new(source: Option<&'a str>) -> Self {
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
            use_module_graph_for_wildcards: false,
            module_graph_wildcard_exports: None,
            fn_declarations: Vec::new(),
            function_references: HashSet::new(),
            in_impl_block: false,
            source,
            externally_imported_names: HashSet::new(),
            test_pipeline_depth: 0,
            type_declarations: Vec::new(),
            type_references: HashSet::new(),
            return_type_stack: Vec::new(),
            complexity_suppression_depth: 0,
            complexity_threshold: DEFAULT_COMPLEXITY_THRESHOLD,
            value_block_depth: 0,
            connector_effect_export_stack: Vec::new(),
        }
    }

    /// Return set of known builtin function names, derived from the VM's
    /// live stdlib registration so there is no separate list to maintain.
    fn builtin_names() -> HashSet<String> {
        harn_vm::stdlib::stdlib_builtin_names()
            .into_iter()
            .collect()
    }

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(super) fn in_test_pipeline(&self) -> bool {
        self.test_pipeline_depth > 0
    }

    pub(super) fn is_test_pipeline_name(name: &str) -> bool {
        name == "test" || name.starts_with("test_")
    }

    pub(super) fn is_entry_pipeline_name(name: &str) -> bool {
        matches!(name, "default" | "main" | "auto")
    }

    pub(super) fn is_assert_builtin(name: &str) -> bool {
        matches!(name, "assert" | "assert_eq" | "assert_ne")
    }

    pub(super) fn is_approval_record_builtin(name: &str) -> bool {
        name == "request_approval"
    }

    /// Score the body of a function/tool and emit a
    /// `cyclomatic-complexity` warning if it exceeds the configured
    /// threshold. No-op when the enclosing decl carries
    /// `@complexity(allow)`.
    pub(super) fn check_cyclomatic_complexity(&mut self, name: &str, body: &[SNode], span: Span) {
        if self.complexity_suppression_depth > 0 {
            return;
        }
        let complexity = cyclomatic_complexity(body);
        let threshold = self.complexity_threshold;
        if complexity <= threshold {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            rule: "cyclomatic-complexity",
            message: format!(
                "function `{name}` has cyclomatic complexity {complexity} (> {threshold})"
            ),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "split `{name}` into smaller helpers, or mark it `@complexity(allow)` if the branching is intrinsic; threshold configurable via `[lint].complexity_threshold` in `harn.toml`"
            )),
            fix: None,
        });
    }

    pub(super) fn lint_function_name(&mut self, name: &str, span: Span) {
        if is_snake_case(name) {
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

    pub(super) fn lint_type_name(&mut self, kind: &'static str, name: &str, span: Span) {
        if is_pascal_case(name) {
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

    pub(super) fn record_type_expr_references(&mut self, type_expr: &TypeExpr) {
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
            TypeExpr::Iter(inner) => self.record_type_expr_references(inner),
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
            TypeExpr::LitString(_) | TypeExpr::LitInt(_) => {}
        }
    }

    /// Map a type annotation to the matching iterator sink method name when
    /// the annotation is a concrete collection type. Returns `None` for
    /// non-collection annotations (including `Iter<T>` itself, which is
    /// already the expression's inferred shape).
    pub(super) fn expected_collection_sink(type_expr: &TypeExpr) -> Option<&'static str> {
        match type_expr {
            TypeExpr::List(_) => Some("to_list"),
            TypeExpr::DictType(_, _) => Some("to_dict"),
            TypeExpr::Applied { name, .. } => match name.as_str() {
                "list" => Some("to_list"),
                "set" => Some("to_set"),
                "dict" => Some("to_dict"),
                _ => None,
            },
            TypeExpr::Named(name) => match name.as_str() {
                "list" => Some("to_list"),
                "set" => Some("to_set"),
                "dict" => Some("to_dict"),
                _ => None,
            },
            _ => None,
        }
    }

    /// Heuristic: does this expression look like a lazy iterator chain that
    /// would yield an `Iter<T>` rather than a concrete collection? We flag
    /// method calls whose outermost (tail) method is a known lazy
    /// combinator or `iter` lift. Sink-terminated chains (e.g.
    /// `...to_list()`) return false.
    pub(super) fn expr_yields_iter(node: &Node) -> bool {
        match node {
            Node::MethodCall { method, .. } | Node::OptionalMethodCall { method, .. } => {
                matches!(
                    method.as_str(),
                    "iter"
                        | "map"
                        | "filter"
                        | "flat_map"
                        | "take"
                        | "skip"
                        | "take_while"
                        | "skip_while"
                        | "zip"
                        | "enumerate"
                        | "chain"
                        | "chunks"
                        | "windows"
                )
            }
            Node::FunctionCall { name, .. } => {
                matches!(name.as_str(), "iter")
            }
            _ => false,
        }
    }

    pub(super) fn check_eager_collection_conversion(&mut self, expected: &TypeExpr, value: &SNode) {
        let Some(sink) = Self::expected_collection_sink(expected) else {
            return;
        };
        if !Self::expr_yields_iter(&value.node) {
            return;
        }
        let (kind_word, collection_label) = match sink {
            "to_list" => ("list", "list"),
            "to_set" => ("set", "set"),
            "to_dict" => ("dict", "dict"),
            _ => return,
        };
        let _ = kind_word;
        let message = format!(
            "expression is an iterator; expected {collection_label}. \
             Add .{sink}() to materialize."
        );
        let fix = append_sink_fix(value.span, sink);
        self.diagnostics.push(LintDiagnostic {
            rule: "eager-collection-conversion",
            message,
            span: value.span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!("append `.{sink}()` to materialize the iterator")),
            fix: Some(fix),
        });
    }

    pub(super) fn record_param_type_references(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                self.record_type_expr_references(type_expr);
            }
        }
    }

    pub(super) fn has_interpolation(node: &SNode) -> bool {
        use harn_lexer::StringSegment;
        matches!(&node.node, Node::InterpolatedString(segments) if segments.iter().any(|segment| matches!(segment, StringSegment::Expression(_, _, _))))
    }

    /// Returns true if the function is a boundary API that returns untyped/opaque data.
    pub(super) fn is_boundary_api(name: &str) -> bool {
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
                | "http_session_request"
                | "sse_receive"
                | "sse_server_mock_receive"
                | "sse_server_response"
                | "sse_server_status"
                | "websocket_accept"
                | "websocket_receive"
                | "host_call"
                | "mcp_call"
        )
    }

    /// Extract the root variable name from an assignment target.
    /// For `x = ...` returns `x`, for `x.foo = ...` or `x[i] = ...` returns `x`.
    pub(super) fn root_var_name(node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. }
            | Node::SliceAccess { object, .. } => Self::root_var_name(object),
            _ => None,
        }
    }

    pub(super) fn is_secret_scan_call(name: &str, args: &[SNode]) -> bool {
        if name == "secret_scan" {
            return true;
        }
        matches!(
            (name, args.get(1).and_then(Self::string_literal_value)),
            ("mcp_call", Some("harn.secret_scan" | "harn::secret_scan"))
        ) || matches!(
            (name, args.first().and_then(Self::string_literal_value)),
            (
                "host_tool_call",
                Some("harn.secret_scan" | "harn::secret_scan")
            )
        )
    }

    pub(super) fn is_pr_open_call(name: &str, args: &[SNode]) -> bool {
        matches!(
            (name, args.get(1).and_then(Self::string_literal_value)),
            (
                "mcp_call",
                Some("git::push_pr" | "git.push_pr" | "create_pr")
            )
        ) || matches!(
            (name, args.first().and_then(Self::string_literal_value)),
            (
                "host_tool_call",
                Some("git::push_pr" | "git.push_pr" | "create_pr")
            )
        )
    }

    fn string_literal_value(node: &SNode) -> Option<&str> {
        match &node.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn warn_missing_secret_scan(&mut self, span: Span) {
        self.diagnostics.push(LintDiagnostic {
            rule: "pr-open-without-secret-scan",
            message: "PR-open flow calls `git::push_pr` without a preceding `secret_scan(...)` in the same handler".to_string(),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "run `secret_scan(content)` first and gate the PR-open call on an empty findings list"
                    .to_string(),
            ),
            fix: None,
        });
    }

    fn analyze_secret_scan_expr(&mut self, node: &SNode, scanned: bool) -> bool {
        match &node.node {
            Node::FunctionCall { name, args } => {
                let mut state = scanned;
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                if Self::is_secret_scan_call(name, args) {
                    return true;
                }
                if Self::is_pr_open_call(name, args) && !state {
                    self.warn_missing_secret_scan(node.span);
                }
                state
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                let mut state = self.analyze_secret_scan_expr(object, scanned);
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                state
            }
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::Spread(object)
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::UnaryOp {
                operand: object, ..
            } => self.analyze_secret_scan_expr(object, scanned),
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                let state = self.analyze_secret_scan_expr(object, scanned);
                self.analyze_secret_scan_expr(index, state)
            }
            Node::SliceAccess { object, start, end } => {
                let mut state = self.analyze_secret_scan_expr(object, scanned);
                if let Some(start) = start {
                    state = self.analyze_secret_scan_expr(start, state);
                }
                if let Some(end) = end {
                    state = self.analyze_secret_scan_expr(end, state);
                }
                state
            }
            Node::BinaryOp { left, right, .. } => {
                let state = self.analyze_secret_scan_expr(left, scanned);
                self.analyze_secret_scan_expr(right, state)
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let then_state = self.analyze_secret_scan_expr(true_expr, state);
                let else_state = self.analyze_secret_scan_expr(false_expr, state);
                then_state && else_state
            }
            Node::ListLiteral(items) | Node::OrPattern(items) => {
                items.iter().fold(scanned, |state, item| {
                    self.analyze_secret_scan_expr(item, state)
                })
            }
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => {
                let mut state = scanned;
                for entry in entries {
                    state = self.analyze_secret_scan_expr(&entry.key, state);
                    state = self.analyze_secret_scan_expr(&entry.value, state);
                }
                state
            }
            Node::EnumConstruct { args, .. } => args.iter().fold(scanned, |state, arg| {
                self.analyze_secret_scan_expr(arg, state)
            }),
            Node::Block(body) => self.analyze_secret_scan_block(body, scanned),
            Node::Closure { body, .. } => {
                let _ = self.analyze_secret_scan_block(body, false);
                scanned
            }
            _ => scanned,
        }
    }

    fn analyze_secret_scan_node(&mut self, node: &SNode, scanned: bool) -> bool {
        match &node.node {
            Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
                self.analyze_secret_scan_expr(value, scanned)
            }
            Node::Assignment { target, value, .. } => {
                let state = self.analyze_secret_scan_expr(target, scanned);
                self.analyze_secret_scan_expr(value, state)
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let then_state = self.analyze_secret_scan_block(then_body, state);
                let Some(else_body) = else_body.as_ref() else {
                    return state;
                };
                let else_state = self.analyze_secret_scan_block(else_body, state);
                then_state && else_state
            }
            Node::ForIn { iterable, body, .. } => {
                let state = self.analyze_secret_scan_expr(iterable, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::WhileLoop { condition, body } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::Retry { count, body } => {
                let state = self.analyze_secret_scan_expr(count, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::TryCatch {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                let try_state = self.analyze_secret_scan_block(body, scanned);
                let catch_state = self.analyze_secret_scan_block(catch_body, scanned);
                let finally_state = finally_body
                    .as_ref()
                    .map(|body| self.analyze_secret_scan_block(body, scanned))
                    .unwrap_or(scanned);
                if finally_state {
                    true
                } else {
                    try_state && catch_state
                }
            }
            Node::TryExpr { body } => self.analyze_secret_scan_block(body, scanned),
            Node::MatchExpr { value, arms } => {
                let state = self.analyze_secret_scan_expr(value, scanned);
                if arms.is_empty() {
                    return state;
                }
                let mut all_arms_scanned = true;
                for arm in arms {
                    let mut arm_state = self.analyze_secret_scan_expr(&arm.pattern, state);
                    if let Some(guard) = arm.guard.as_ref() {
                        arm_state = self.analyze_secret_scan_expr(guard, arm_state);
                    }
                    all_arms_scanned &= self.analyze_secret_scan_block(&arm.body, arm_state);
                }
                all_arms_scanned
            }
            Node::Parallel { expr, body, .. } => {
                let state = self.analyze_secret_scan_expr(expr, scanned);
                let _ = self.analyze_secret_scan_block(body, false);
                state
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let mut all_cases_scanned = !cases.is_empty();
                for case in cases {
                    let state = self.analyze_secret_scan_expr(&case.channel, scanned);
                    all_cases_scanned &= self.analyze_secret_scan_block(&case.body, state);
                }
                if let Some((timeout_expr, timeout_body)) = timeout {
                    let state = self.analyze_secret_scan_expr(timeout_expr, scanned);
                    all_cases_scanned &= self.analyze_secret_scan_block(timeout_body, state);
                }
                if let Some(default_body) = default_body {
                    all_cases_scanned &= self.analyze_secret_scan_block(default_body, scanned);
                }
                all_cases_scanned
            }
            Node::ReturnStmt { value } => value
                .as_ref()
                .map(|value| self.analyze_secret_scan_expr(value, scanned))
                .unwrap_or(scanned),
            Node::ThrowStmt { value } => self.analyze_secret_scan_expr(value, scanned),
            _ => self.analyze_secret_scan_expr(node, scanned),
        }
    }

    fn analyze_secret_scan_block(&mut self, nodes: &[SNode], scanned: bool) -> bool {
        let mut state = scanned;
        for node in nodes {
            state = self.analyze_secret_scan_node(node, state);
        }
        state
    }

    /// Extract all variable names from a binding pattern.
    pub(super) fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
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
    pub(super) fn declare_pattern_variables(
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
    pub(super) fn declare_variable(
        &mut self,
        name: &str,
        span: Span,
        is_mutable: bool,
        is_simple_ident: bool,
    ) {
        if name == "_" {
            return;
        }

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
    pub(super) fn declare_parameter(&mut self, name: &str, span: Span) {
        if name == "_" {
            return;
        }

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

    pub(crate) fn lint_program(&mut self, nodes: &[SNode]) {
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

    /// Lint a block of statements, flagging unreachable code after a
    /// terminator (`return`/`throw`).
    pub(super) fn lint_block(&mut self, nodes: &[SNode]) {
        use harn_parser::stmt_definitely_exits;

        let mut found_terminator = false;

        for (idx, node) in nodes.iter().enumerate() {
            if found_terminator {
                self.diagnostics.push(LintDiagnostic {
                    rule: "unreachable-code",
                    message: "unreachable code after return or throw".to_string(),
                    span: node.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("remove the unreachable code".to_string()),
                    fix: None,
                });
                // Only report once per block.
                break;
            }

            let final_value_expr = self.value_block_depth > 0 && idx + 1 == nodes.len();
            if !final_value_expr {
                self.check_discarded_approval_result(node);
            }

            self.lint_node(node);

            if stmt_definitely_exits(node) {
                found_terminator = true;
            }
        }
    }

    fn check_discarded_approval_result(&mut self, node: &SNode) {
        let Node::FunctionCall { name, .. } = &node.node else {
            return;
        };
        if !Self::is_approval_record_builtin(name) {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            rule: "unhandled-approval-result",
            message: format!("approval result from `{name}` is discarded"),
            span: node.span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "bind the result, inspect its signed approver receipts, or explicitly assign it to `_`"
                    .to_string(),
            ),
            fix: None,
        });
    }

    /// Run post-walk analysis and finalize diagnostics.
    pub(crate) fn finalize(&mut self) {
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
                        // Remove this name from the import list, plus the
                        // adjacent comma/space so the list stays well-formed.
                        let region = src.get(import.span.start..import.span.end)?;
                        let name_pos = region.find(name.as_str())?;
                        let abs_start = import.span.start + name_pos;
                        let abs_end = abs_start + name.len();
                        let after = src.get(abs_end..import.span.end)?;
                        let before = src.get(import.span.start..abs_start)?;
                        let (rm_start, rm_end) = if after.starts_with(',') {
                            let extra = if after.get(1..2) == Some(" ") { 2 } else { 1 };
                            (abs_start, abs_end + extra)
                        } else if before.ends_with(", ") {
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

        // Variables and parameters may hold closures, so treat them as
        // callable when checking for undefined functions below.
        let all_vars: HashSet<String> = self
            .declarations
            .iter()
            .map(|d| d.name.clone())
            .chain(self.param_declarations.iter().map(|p| p.name.clone()))
            .collect();

        // Wildcard imports hide the real name set unless we can fully
        // resolve them to exports.
        if self.use_module_graph_for_wildcards {
            match &self.module_graph_wildcard_exports {
                Some(names) => {
                    self.known_functions.extend(names.iter().cloned());
                }
                None => {
                    return;
                }
            }
        } else if self.has_wildcard_import {
            return;
        }
        for (name, span) in &self.function_calls {
            if self.known_functions.contains(name) {
                continue;
            }
            if all_vars.contains(name) {
                continue;
            }
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
