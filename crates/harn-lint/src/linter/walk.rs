//! The main AST dispatch for the linter walk. Kept in its own submodule
//! so the giant `lint_node` match doesn't dominate the surrounding
//! state-tracking plumbing.

use harn_lexer::{FixEdit, Span, StringSegment};
use harn_parser::{Node, SNode};

use super::Linter;
use crate::decls::{FnDeclaration, ImportInfo, TypeDeclaration};
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::{
    empty_statement_removal_fix, is_pure_expression, nil_fallback_ternary_parts,
    unnecessary_cast_fix,
};
use crate::harndoc::extract_harndoc;
use crate::naming::simplify_bool_comparison;

impl<'a> Linter<'a> {
    pub(super) fn lint_node(&mut self, snode: &SNode) {
        match &snode.node {
            Node::Pipeline {
                params,
                return_type,
                body,
                name,
                is_pub,
                ..
            } => {
                self.known_functions.insert(name.clone());
                if return_type.is_none()
                    && *is_pub
                    && !Self::is_entry_pipeline_name(name)
                    && !Self::is_test_pipeline_name(name)
                {
                    self.diagnostics.push(LintDiagnostic {
                        rule: "pipeline-return-type",
                        message: format!(
                            "public pipeline `{name}` has no declared return type; \
                             explicit return types will be required in a future release"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "declare a return type: `pub pipeline {name}(...) -> TypeExpr {{ ... }}`"
                        )),
                        fix: None,
                    });
                }
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
                let _ = self.analyze_secret_scan_block(body, false);
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
                self.check_cyclomatic_complexity(name, body, snode.span);
                self.push_scope();
                let saved_loop_depth = self.loop_depth;
                self.loop_depth = 0;
                for p in params {
                    self.declare_parameter(&p.name, snode.span);
                }
                self.return_type_stack.push(return_type.clone());
                if harn_vm::connector_export_effect_class(name).is_some() {
                    self.connector_effect_export_stack.push(name.clone());
                }
                let _ = self.analyze_secret_scan_block(body, false);
                self.lint_block(body);
                if harn_vm::connector_export_effect_class(name).is_some() {
                    self.connector_effect_export_stack.pop();
                }
                self.return_type_stack.pop();
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
                self.check_cyclomatic_complexity(name, body, snode.span);
                self.push_scope();
                let saved_loop_depth = self.loop_depth;
                self.loop_depth = 0;
                for p in params {
                    self.declare_parameter(&p.name, snode.span);
                }
                self.return_type_stack.push(return_type.clone());
                let _ = self.analyze_secret_scan_block(body, false);
                self.lint_block(body);
                self.return_type_stack.pop();
                self.loop_depth = saved_loop_depth;
                self.pop_scope();
            }

            Node::SkillDecl {
                name,
                fields,
                is_pub,
            } => {
                self.known_functions.insert(name.clone());
                self.fn_declarations.push(FnDeclaration {
                    name: name.clone(),
                    span: snode.span,
                    is_pub: *is_pub,
                    is_method: false,
                });
                for (_k, value) in fields {
                    self.lint_node(value);
                }
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

            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.lint_node(value);
                if let Some(ann) = type_ann {
                    self.check_eager_collection_conversion(ann, value);
                }
                self.declare_pattern_variables(pattern, snode.span, false);
            }

            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.lint_node(value);
                if let Some(ann) = type_ann {
                    self.check_eager_collection_conversion(ann, value);
                }
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
                if let Some(export) = self.connector_effect_export_stack.last() {
                    if let Some(reason) =
                        harn_vm::connector_export_denied_builtin_reason(export, name)
                    {
                        self.diagnostics.push(LintDiagnostic {
                            rule: "connector-effect-policy",
                            message: format!(
                                "connector export `{export}` calls disallowed builtin `{name}`: {reason}"
                            ),
                            span: snode.span,
                            severity: LintSeverity::Warning,
                            suggestion: Some(format!(
                                "move `{name}` out of `{export}` or configure a trusted connector effect-policy override"
                            )),
                            fix: None,
                        });
                    }
                }
                if let Some(target) = unnecessary_cast_target(name, args) {
                    let inner = &args[0];
                    let fix = unnecessary_cast_fix(self.source, snode.span, inner.span);
                    let article = if matches!(target, "int") { "an" } else { "a" };
                    self.diagnostics.push(LintDiagnostic {
                        rule: "unnecessary-cast",
                        message: format!(
                            "`{name}` is a no-op here — its argument is already {article} {target}"
                        ),
                        span: snode.span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!("remove the redundant `{name}(...)` wrapper")),
                        fix,
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

            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
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
                if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
                    let has_bad_literal =
                        matches!(left.node, Node::BoolLiteral(_) | Node::NilLiteral)
                            || matches!(right.node, Node::BoolLiteral(_) | Node::NilLiteral);
                    if has_bad_literal {
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

                // Detect ternary fallbacks over a nil check where the non-nil
                // branch is identical to the checked variable:
                //   x == nil ? fallback : x   →   x ?? fallback
                //   x != nil ? x : fallback   →   x ?? fallback
                // Only fires when the checked variable is a bare identifier so
                // it is evaluated exactly once in both forms.
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
                if then_body.is_empty() {
                    // Skip autofix when an `else` branch exists (dropping the
                    // whole if-else would silently drop the else body) or when
                    // the condition has observable side effects.
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
                if let Some(else_b) = else_body {
                    let then_returns = then_body
                        .last()
                        .is_some_and(|s| matches!(s.node, Node::ReturnStmt { .. }));
                    let else_returns = else_b
                        .last()
                        .is_some_and(|s| matches!(s.node, Node::ReturnStmt { .. }));
                    if then_returns && else_returns {
                        // Rewrite `} else { <body> }` as `}\n<indent><body>`.
                        let fix = self.source.and_then(|src| {
                            let then_last = then_body.last()?;
                            let else_first = else_b.first()?;
                            let else_last = else_b.last()?;
                            let search_start = then_last.span.end;
                            let body_text = src.get(else_first.span.start..else_last.span.end)?;
                            let else_block_end = snode.span.end;
                            let between = src.get(search_start..else_first.span.start)?;
                            let else_kw_off = between.find("else")?;
                            let else_start = search_start + else_kw_off;
                            let line_start =
                                src[..snode.span.start].rfind('\n').map_or(0, |p| p + 1);
                            let indent = &src[line_start..snode.span.start];
                            let close_brace = src.get(search_start..else_start)?.rfind('}')?;
                            let replace_start = search_start + close_brace + 1;
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
                    // A pure iterable makes the whole loop a removable no-op.
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
                self.value_block_depth += 1;
                self.lint_block(body);
                self.value_block_depth -= 1;
                self.pop_scope();
            }

            Node::MatchExpr { value, arms } => {
                self.lint_node(value);
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
                    if let Some(Some(ret_ty)) = self.return_type_stack.last().cloned() {
                        self.check_eager_collection_conversion(&ret_ty, v);
                    }
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
                self.loop_depth = 0;
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
                        // Record any identifier tokens inside `${...}` as
                        // references so lints like `unused-variable` see them.
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

            Node::TryOperator { operand } | Node::TryStar { operand } => {
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
                if !self.use_module_graph_for_wildcards {
                    self.has_wildcard_import = true;
                }
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

            Node::TypeDecl {
                name,
                type_params: _,
                type_expr,
            } => {
                self.lint_type_name("type", name, snode.span);
                self.record_type_expr_references(type_expr);
            }

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

            Node::AttributedDecl { attributes, inner } => {
                let suppresses_complexity = attributes.iter().any(|a| {
                    a.name == "complexity"
                        && a.args.iter().any(|arg| {
                            arg.name.is_none()
                                && matches!(&arg.value.node, Node::Identifier(s) if s == "allow")
                        })
                });
                if suppresses_complexity {
                    self.complexity_suppression_depth += 1;
                }
                self.lint_node(inner);
                if suppresses_complexity {
                    self.complexity_suppression_depth -= 1;
                }
            }

            Node::OrPattern(alternatives) => {
                for alt in alternatives {
                    self.lint_node(alt);
                }
            }
        }
    }
}

/// If `name` is one of the conversion builtins (`to_string`, `to_int`,
/// `to_float`, `to_list`, `to_dict`) and `args` is exactly one expression
/// already syntactically known to be of the target type, return the
/// human-readable target name (`"string"`, `"int"`, ...). Returns `None`
/// otherwise — including for valid conversions like `to_int("42")` and for
/// calls with the wrong arity, both of which the lint must leave alone.
fn unnecessary_cast_target(name: &str, args: &[SNode]) -> Option<&'static str> {
    if args.len() != 1 {
        return None;
    }
    let arg = &args[0].node;
    let target = match name {
        "to_string" => "string",
        "to_int" => "int",
        "to_float" => "float",
        "to_list" => "list",
        "to_dict" => "dict",
        _ => return None,
    };
    if expr_has_known_type(arg, name) {
        Some(target)
    } else {
        None
    }
}

/// Static-shape check: does `node` already produce a value of the type
/// that `cast` would yield? Conservative — only literals of matching shape
/// and a chained call to the same conversion builtin count.
fn expr_has_known_type(node: &Node, cast: &str) -> bool {
    // Chained `to_X(to_X(...))` — outer is always redundant regardless of
    // what the inner expression is.
    if let Node::FunctionCall {
        name: inner_name,
        args: inner_args,
    } = node
    {
        if inner_name == cast && inner_args.len() == 1 {
            return true;
        }
    }
    matches!(
        (cast, node),
        (
            "to_string",
            Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_),
        ) | ("to_int", Node::IntLiteral(_))
            | ("to_float", Node::FloatLiteral(_))
            | ("to_list", Node::ListLiteral(_))
            | ("to_dict", Node::DictLiteral(_))
    )
}
