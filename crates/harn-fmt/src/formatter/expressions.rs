use harn_lexer::StringSegment;
use harn_parser::{Node, ParallelMode, SNode, TypeExpr, TypedParam};

use crate::helpers::*;

use super::Formatter;

impl Formatter<'_> {
    pub(super) fn format_require_stmt(
        &self,
        condition: &SNode,
        message: Option<&SNode>,
        indent: usize,
        column: usize,
    ) -> String {
        let cond = self.format_expr(condition, indent, column + 8);
        let Some(message) = message else {
            return format!("require {cond}");
        };
        let message_col = column_after(column + 8, &cond) + 2;
        let message = self.format_expr(message, indent, message_col);
        let inline = format!("require {cond}, {message}");
        if cond.contains('\n')
            || message.contains('\n')
            || column + text_width(&inline) > self.line_width
        {
            format!("require {cond},\n{}{}", "  ".repeat(indent + 1), message)
        } else {
            inline
        }
    }

    /// Comments that sat between a multi-line chain object and its
    /// `.method(...)` segment, rendered one per line with `pad` indentation
    /// (empty when there are none). Claims only full-line, non-doc comments
    /// strictly above the segment, so closure-body and trailing comments are
    /// untouched.
    pub(super) fn chain_segment_comments(
        &self,
        object: &SNode,
        args: &[SNode],
        node: &SNode,
        pad: &str,
    ) -> String {
        let boundary = args
            .first()
            .map(|arg| arg.span.line)
            .unwrap_or(node.span.end_line);
        let mut lead = String::new();
        for comment in self.claim_leading_comments_in_range(object.span.end_line + 1, boundary) {
            lead.push_str(pad);
            lead.push_str(&comment);
            lead.push('\n');
        }
        lead
    }

    /// Format `node` as if it sits at logical indent depth `indent`. When the
    /// node renders inline, `indent` does not show up in the output. When the
    /// node wraps onto multiple lines, its closing delimiter aligns to
    /// `indent` and its inner contents land at `indent + 1`.
    pub(crate) fn format_expr(&self, node: &SNode, indent: usize, column: usize) -> String {
        match &node.node {
            Node::StringLiteral(s) => {
                if node.span.line != node.span.end_line {
                    self.source_slice(node).to_string()
                } else {
                    let escaped = escape_string(s);
                    format!("\"{escaped}\"")
                }
            }
            Node::RawStringLiteral(s) => format_raw_string(s),
            Node::InterpolatedString(segments) => {
                if node.span.line != node.span.end_line {
                    return self.source_slice(node).to_string();
                }
                let mut result = String::from("\"");
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => result.push_str(&escape_string(s)),
                        StringSegment::Expression(e, _, _) => {
                            result.push_str(&format!("${{{e}}}"));
                        }
                    }
                }
                result.push('"');
                result
            }
            Node::IntLiteral(n) => n.to_string(),
            Node::FloatLiteral(f) => format_float(*f),
            Node::BoolLiteral(b) => b.to_string(),
            Node::NilLiteral => "nil".to_string(),
            Node::Identifier(name) => name.clone(),
            Node::DurationLiteral(ms) => format_duration(*ms),
            Node::BinaryOp { op, left, right } => {
                if op == "+" && plus_chain_depth(left) >= 64 {
                    return self.format_deep_plus_chain(node, indent, column);
                }
                let mut l = self.format_expr(left, indent, column);
                let mut r = self.format_expr(right, indent, column_after(column, &l));
                let op_str = if op == "not_in" {
                    "not in"
                } else {
                    op.as_str()
                };

                if child_needs_parens(op, &left.node, false) {
                    l = format!("({l})");
                }
                if child_needs_parens(op, &right.node, true) {
                    r = format!("({r})");
                }

                let inline = format!("{l} {op_str} {r}");
                let should_break = left.span.line < right.span.line
                    || column + text_width(&inline) > self.line_width;

                if should_break {
                    let pad = "  ".repeat(indent + 1);
                    if op_safe_after_newline(op_str) {
                        // Claim any trailing comment on the left operand's last
                        // line before we break, so it isn't orphaned. The
                        // statement-level trailing-comment pass only sees the
                        // whole expression's end line (the right operand), so
                        // without this the left operand's comment is dropped or
                        // relocated out of its block. Safe only here: a `//`
                        // comment before the `\` line-continuation below would
                        // comment out the continuation, so that branch is left
                        // to the statement-level pass.
                        let left_comment = self
                            .take_trailing_comment_for_line(left.span.end_line)
                            .map(|c| format!("  {c}"))
                            .unwrap_or_default();
                        format!("{l}{left_comment}\n{pad}{op_str} {r}")
                    } else {
                        format!("{l} \\\n{pad}{op_str} {r}")
                    }
                } else {
                    inline
                }
            }
            Node::UnaryOp { op, operand } => {
                let expr = self.format_expr(operand, indent, column + text_width(op));
                if needs_parens_as_unary_operand(&operand.node) {
                    format!("{op}({expr})")
                } else {
                    format!("{op}{expr}")
                }
            }
            Node::TryOperator { operand } => {
                let expr = self.format_expr(operand, indent, column);
                if needs_parens_as_postfix_object(&operand.node) {
                    format!("({expr})?")
                } else {
                    format!("{expr}?")
                }
            }
            Node::NonNullAssert { operand } => {
                let expr = self.format_expr(operand, indent, column);
                if needs_parens_as_postfix_object(&operand.node) {
                    format!("({expr})!")
                } else {
                    format!("{expr}!")
                }
            }
            Node::TryStar { operand } => {
                let expr = self.format_expr(operand, indent, column + 5);
                if needs_parens_as_unary_operand(&operand.node) {
                    format!("try* ({expr})")
                } else {
                    format!("try* {expr}")
                }
            }
            Node::FunctionCall {
                name,
                type_args,
                args,
            } => {
                let type_args_str = if type_args.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<{}>",
                        type_args
                            .iter()
                            .map(format_type_expr)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let args_str = self.format_call_args(
                    args,
                    column + text_width(name) + text_width(&type_args_str) + 1,
                    indent,
                );
                format!("{name}{type_args_str}({args_str})")
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let pad = "  ".repeat(indent + 1);
                let lead = self.chain_segment_comments(object, args, node, &pad);
                let wrap = self.chain_wraps(&obj, &lead, text_width(method) + 2, column);
                let prefix_len =
                    self.chain_prefix_len(&obj, text_width(method) + 2, column, indent, wrap);
                let args_str =
                    self.format_call_args(args, prefix_len, self.chain_args_indent(indent, wrap));
                if wrap {
                    format!("{obj}\n{lead}{pad}.{method}({args_str})")
                } else {
                    format!("{obj}.{method}({args_str})")
                }
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                // Same rule as `MethodCall`; `?.` is one column wider.
                let pad = "  ".repeat(indent + 1);
                let lead = self.chain_segment_comments(object, args, node, &pad);
                let wrap = self.chain_wraps(&obj, &lead, text_width(method) + 3, column);
                let prefix_len =
                    self.chain_prefix_len(&obj, text_width(method) + 3, column, indent, wrap);
                let args_str =
                    self.format_call_args(args, prefix_len, self.chain_args_indent(indent, wrap));
                if wrap {
                    format!("{obj}\n{lead}{pad}?.{method}({args_str})")
                } else {
                    format!("{obj}?.{method}({args_str})")
                }
            }
            Node::PropertyAccess { object, property } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let suffix = format!(".{property}");
                if column_after(column, &obj) + text_width(&suffix) > self.line_width {
                    format!("{obj}\n{}{}", "  ".repeat(indent + 1), suffix)
                } else {
                    format!("{obj}{suffix}")
                }
            }
            Node::OptionalPropertyAccess { object, property } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let suffix = format!("?.{property}");
                if column_after(column, &obj) + text_width(&suffix) > self.line_width {
                    format!("{obj}\n{}{}", "  ".repeat(indent + 1), suffix)
                } else {
                    format!("{obj}{suffix}")
                }
            }
            Node::SubscriptAccess { object, index } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let idx = self.format_expr(index, indent, column_after(column, &obj) + 1);
                format!("{obj}[{idx}]")
            }
            Node::OptionalSubscriptAccess { object, index } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let idx = self.format_expr(index, indent, column_after(column, &obj) + 3);
                format!("{obj}?.[{idx}]")
            }
            Node::SliceAccess { object, start, end } => {
                let mut obj = self.format_expr(object, indent, column);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let s = start
                    .as_ref()
                    .map(|n| self.format_expr(n, indent, column_after(column, &obj) + 1))
                    .unwrap_or_default();
                let e = end
                    .as_ref()
                    .map(|n| {
                        self.format_expr(n, indent, column_after(column, &obj) + text_width(&s) + 2)
                    })
                    .unwrap_or_default();
                format!("{obj}[{s}:{e}]")
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                // A range bound to a ternary part must be parenthesized: a bare
                // range as the condition would swallow the `?:` into its end
                // bound, and as a branch it simply fails to re-parse.
                let cond =
                    paren_if_range(self.format_expr(condition, indent, column), &condition.node);
                let t_col = column_after(column, &cond) + 3;
                let t = paren_if_range(self.format_expr(true_expr, indent, t_col), &true_expr.node);
                let f_col = column_after(t_col, &t) + 3;
                let f = paren_if_range(
                    self.format_expr(false_expr, indent, f_col),
                    &false_expr.node,
                );
                format!("{cond} ? {t} : {f}")
            }
            Node::Assignment {
                target, value, op, ..
            } => {
                let t = self.format_expr(target, indent, column);
                let v_col = column_after(column, &t)
                    + op.as_deref().map_or(3, |operator| text_width(operator) + 3);
                let v = self.format_expr(value, indent, v_col);
                if let Some(op) = op {
                    format!("{t} {op}= {v}")
                } else {
                    format!("{t} = {v}")
                }
            }
            Node::ListLiteral(elems) => {
                // Children land at `indent + 1` if the list wraps; render them
                // there so their own internal wrapping is at the right depth.
                let mut from_line = node.span.line + 1;
                let items = elems
                    .iter()
                    .map(|e| {
                        let body = self.format_expr(e, indent + 1, wrapped_item_column(indent));
                        let item = self.commented_item(
                            body,
                            from_line,
                            e.span.line,
                            e.span.end_line,
                            node.span.end_line,
                        );
                        from_line = e.span.end_line + 1;
                        item
                    })
                    .collect::<Vec<_>>();
                let items = self.format_comma_sequence_commented(items, column + 1, indent);
                format!("[{items}]")
            }
            Node::DictLiteral(entries) => {
                let items = self.format_dict_entry_list(
                    entries,
                    column + 1,
                    indent,
                    (node.span.line, node.span.end_line),
                );
                format!("{{{items}}}")
            }
            Node::RangeExpr {
                start,
                end,
                inclusive,
            } => {
                // A range bound that is itself a range needs parens; `a to b to c`
                // is non-associative and would not re-parse.
                let s = paren_if_range(self.format_expr(start, indent, column), &start.node);
                let end_col = column_after(column, &s) + 4;
                let e = paren_if_range(self.format_expr(end, indent, end_col), &end.node);
                if *inclusive {
                    format!("{s} to {e}")
                } else {
                    format!("{s} to {e} exclusive")
                }
            }
            Node::Closure {
                params,
                return_type,
                throws,
                body,
                fn_syntax,
            } => {
                if *fn_syntax || return_type.is_some() || throws.is_some() {
                    self.format_fn_closure(
                        params,
                        return_type.as_ref(),
                        throws.as_ref(),
                        body,
                        column,
                        indent,
                        node.span.line,
                    )
                } else {
                    self.format_arrow_closure(params, body, column, indent, node.span.line)
                }
            }
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                if args.is_empty() {
                    format!("{enum_name}.{variant}")
                } else {
                    let args_str = self.format_call_args(
                        args,
                        column + text_width(enum_name) + text_width(variant) + 2,
                        indent,
                    );
                    format!("{enum_name}.{variant}({args_str})")
                }
            }
            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                let items = self.format_dict_entry_list(
                    fields,
                    column + text_width(struct_name) + 2,
                    indent,
                    (node.span.line, node.span.end_line),
                );
                format!("{struct_name} {{{items}}}")
            }
            Node::DeferStmt { body } => {
                self.format_block_expr("defer {", body, indent, node.span.line)
            }
            Node::SpawnExpr { body } => {
                self.format_block_expr("spawn {", body, indent, node.span.line)
            }
            Node::ScopeBlock { body } => {
                self.format_block_expr("scope {", body, indent, node.span.line)
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    format!("yield {}", self.format_expr(val, indent, column + 6))
                } else {
                    "yield".to_string()
                }
            }
            Node::EmitExpr { value } => {
                format!("emit {}", self.format_expr(value, indent, column + 5))
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    format!("return {}", self.format_expr(val, indent, column + 7))
                } else {
                    "return".to_string()
                }
            }
            Node::ThrowStmt { value } => {
                format!("throw {}", self.format_expr(value, indent, column + 6))
            }
            Node::BreakStmt => "break".to_string(),
            Node::ContinueStmt => "continue".to_string(),
            Node::Block(stmts) => self.format_block_expr("block {", stmts, indent, node.span.line),
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value, indent, column + 6);
                let mut result = format!("match {val} {{\n");
                let arm_indent = indent + 1;
                let mut arm_from_line = node.span.line;
                for arm in arms {
                    let indent_str = "  ".repeat(arm_indent);
                    // An arm's own leading comments, above its pattern. Nothing
                    // else claims these: the arm BODY is bounded at the pattern
                    // precisely so a comment about the arm is not dragged inside
                    // it, which leaves them for the arm itself to render here.
                    result.push_str(&self.render_comments_in_range(
                        arm_from_line + 1,
                        arm.pattern.span.line,
                        arm_indent,
                    ));
                    let pattern = self.format_expr(&arm.pattern, arm_indent, arm_indent * 2);
                    let guard_str = if let Some(ref guard) = arm.guard {
                        format!(
                            " if {}",
                            self.format_expr(
                                guard,
                                arm_indent,
                                arm_indent * 2 + text_width(&pattern) + 4,
                            )
                        )
                    } else {
                        String::new()
                    };
                    if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
                        let expr = self.format_expr(
                            &arm.body[0],
                            arm_indent,
                            arm_indent * 2 + text_width(&pattern) + text_width(&guard_str) + 4,
                        );
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern}{guard_str} -> {{ {expr} }}"));
                        // Keep a same-line comment on the arm (`1 -> { x } // c`)
                        // attached; unclaimed it would be flushed to EOF.
                        if let Some(trail) =
                            self.take_trailing_comment_for_line(arm.body[0].span.end_line)
                        {
                            result.push_str("  ");
                            result.push_str(&trail);
                        }
                        result.push('\n');
                    } else {
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern}{guard_str} -> {{\n"));
                        // Bound at the arm's own pattern, not the `match`: a comment
                        // ABOVE the pattern documents the arm, not its first statement.
                        result.push_str(&self.format_body_string(
                            &arm.body,
                            arm_indent + 1,
                            arm.pattern.span.end_line,
                        ));
                        result.push_str(&indent_str);
                        result.push_str("}\n");
                    }
                    // The next arm's leading comments start after this one ends.
                    arm_from_line = arm
                        .body
                        .last()
                        .map(|n| n.span.end_line)
                        .unwrap_or(arm.pattern.span.end_line);
                }
                let close = "  ".repeat(indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition, indent, column + 6);
                self.format_block_expr(
                    &format!("guard {cond} else {{"),
                    else_body,
                    indent,
                    node.span.line,
                )
            }
            Node::RequireStmt { condition, message } => {
                self.format_require_stmt(condition, message.as_deref(), indent, column)
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration, indent, column + 9);
                self.format_block_expr(&format!("deadline {dur} {{"), body, indent, node.span.line)
            }
            Node::MutexBlock { key, body } => match key {
                Some(key_expr) => {
                    let k = self.format_expr(key_expr, indent, column + 6);
                    self.format_block_expr(&format!("mutex({k}) {{"), body, indent, node.span.line)
                }
                None => self.format_block_expr("mutex {", body, indent, node.span.line),
            },
            Node::IfElse {
                condition,
                then_body,
                then_span,
                else_body,
                else_span,
            } => {
                let cond = self.format_expr(condition, indent, column + 5);
                let inner = indent + 1;
                let mut result = format!("if {cond} {{\n");
                result.push_str(&self.format_body_string_bounded(
                    then_body,
                    inner,
                    then_span.line,
                    Some(then_span.end_line),
                ));
                let close = "  ".repeat(indent);
                if let Some(eb) = else_body {
                    // Render `else if` as a single chain when the else
                    // body is a sole nested IfElse — matches the source
                    // form and avoids gratuitously deepening indentation.
                    if eb.len() == 1 && matches!(eb[0].node, Node::IfElse { .. }) {
                        result.push_str(&close);
                        result.push_str("} else ");
                        result.push_str(&self.format_expr(&eb[0], indent, column + 7));
                    } else {
                        result.push_str(&close);
                        result.push_str("} else {\n");
                        let else_span = else_span.expect("braced else has a block span");
                        result.push_str(&self.format_body_string_bounded(
                            eb,
                            inner,
                            else_span.line,
                            Some(else_span.end_line),
                        ));
                        result.push_str(&close);
                        result.push('}');
                    }
                } else {
                    result.push_str(&close);
                    result.push('}');
                }
                result
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let pat = format_pattern(pattern);
                let iter_col = column + 4 + text_width(&pat) + 4;
                let iter_str = self.format_expr(iterable, indent, iter_col);
                self.format_block_expr(
                    &format!("for {pat} in {iter_str} {{"),
                    body,
                    indent,
                    node.span.line,
                )
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition, indent, column + 6);
                self.format_block_expr(&format!("while {cond} {{"), body, indent, node.span.line)
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count, indent, column + 6);
                self.format_block_expr(&format!("retry {cnt} {{"), body, indent, node.span.line)
            }
            Node::CostRoute { options, body } => {
                let inner = indent + 1;
                let close = "  ".repeat(indent);
                let mut result = String::from("cost_route {\n");
                for (key, value) in options {
                    let value = self.format_expr(value, inner, inner * 2 + text_width(key) + 2);
                    result.push_str(&"  ".repeat(inner));
                    result.push_str(&format!("{key}: {value}\n"));
                }
                if !options.is_empty() && !body.is_empty() {
                    result.push('\n');
                }
                result.push_str(&self.format_body_string(body, inner, node.span.line));
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::TryCatch {
                body,
                try_span,
                has_catch,
                error_var,
                error_type,
                catch_body,
                catch_span,
                finally_body,
                finally_span,
            } => {
                let inner = indent + 1;
                let close = "  ".repeat(indent);
                let mut result = String::from("try {\n");
                result.push_str(&self.format_body_string_bounded(
                    body,
                    inner,
                    try_span.line,
                    Some(try_span.end_line),
                ));
                if *has_catch {
                    let catch_param = format_catch_param(error_var, error_type);
                    result.push_str(&close);
                    result.push_str(&format!("}} catch{catch_param} {{\n"));
                    let catch_span = catch_span.expect("catch has a block span");
                    result.push_str(&self.format_body_string_bounded(
                        catch_body,
                        inner,
                        catch_span.line,
                        Some(catch_span.end_line),
                    ));
                }
                if let Some(fb) = finally_body {
                    result.push_str(&close);
                    result.push_str("} finally {\n");
                    let finally_span = finally_span.expect("finally has a block span");
                    result.push_str(&self.format_body_string_bounded(
                        fb,
                        inner,
                        finally_span.line,
                        Some(finally_span.end_line),
                    ));
                }
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::TryExpr { body } => self.format_block_expr("try {", body, indent, node.span.line),
            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                let keyword = match mode {
                    ParallelMode::Count => "parallel",
                    ParallelMode::Each | ParallelMode::EachStream => "parallel each",
                    ParallelMode::Settle => "parallel settle",
                };
                let e = self.format_expr(expr, indent, column + text_width(keyword) + 1);
                let options_clause = if options.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> = options
                        .iter()
                        .map(|(key, value)| {
                            format!(
                                "{key}: {}",
                                self.format_expr(value, indent, column + text_width(keyword) + 1)
                            )
                        })
                        .collect();
                    let inline = format!(" with {{ {} }}", formatted.join(", "));
                    let inline_opening = if let Some(var) = variable {
                        format!("{keyword} {e}{inline} {{ {var} ->")
                    } else {
                        format!("{keyword} {e}{inline} {{")
                    };
                    if column + text_width(&inline_opening) <= self.line_width {
                        inline
                    } else {
                        let item_indent = "  ".repeat(indent + 1);
                        let close_indent = "  ".repeat(indent);
                        format!(
                            " with {{\n{}\n{close_indent}}}",
                            formatted
                                .iter()
                                .map(|item| format!("{item_indent}{item},"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    }
                };
                let opening = if let Some(var) = variable {
                    format!("{keyword} {e}{options_clause} {{ {var} ->")
                } else {
                    format!("{keyword} {e}{options_clause} {{")
                };
                let mut formatted = self.format_block_expr(&opening, body, indent, node.span.line);
                if matches!(mode, ParallelMode::EachStream) {
                    formatted.push_str(" as stream");
                }
                formatted
            }
            // Declaration nodes rendered as placeholders when used in expr position.
            Node::Pipeline { name, .. } => format!("/* pipeline {name} */"),
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                throws,
                where_clauses,
                body,
                is_pub,
                is_stream,
            } => {
                let pub_prefix = match (*is_pub, *is_stream) {
                    (true, true) => "pub gen ",
                    (true, false) => "pub ",
                    (false, true) => "gen ",
                    (false, false) => "",
                };
                let sig = self.format_fn_signature(
                    pub_prefix,
                    name,
                    type_params,
                    params,
                    return_type,
                    throws,
                    where_clauses,
                    column,
                    indent,
                );
                self.format_block_expr(&format!("{sig} {{"), body, indent, node.span.line)
            }
            Node::ToolDecl {
                name,
                description,
                params,
                return_type,
                throws,
                body,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let ret_inline = return_type
                    .as_ref()
                    .map(|rt| format!(" -> {}", format_type_expr(rt)))
                    .unwrap_or_default();
                let throws_str = format_throws_clause(throws);
                let params_col = column + text_width(pub_prefix) + 5 + text_width(name) + 1;
                let suffix_len = text_width(&ret_inline) + text_width(&throws_str) + 2;
                let params_str =
                    self.format_typed_params_wrapped(params, params_col + suffix_len, indent);
                let ret = self.format_return_type(
                    return_type,
                    params_col,
                    &params_str,
                    indent,
                    text_width(&throws_str) + 2,
                );
                let mut effective_body = Vec::new();
                if let Some(desc) = description {
                    let escaped = escape_string(desc);
                    effective_body.push(harn_parser::Spanned::dummy(Node::FunctionCall {
                        name: "description".to_string(),
                        type_args: Vec::new(),
                        args: vec![harn_parser::Spanned::dummy(Node::StringLiteral(escaped))],
                    }));
                }
                effective_body.extend(body.iter().cloned());
                self.format_block_expr(
                    &format!("{pub_prefix}tool {name}({params_str}){ret}{throws_str} {{"),
                    &effective_body,
                    indent,
                    node.span.line,
                )
            }
            Node::SkillDecl {
                name,
                fields,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let item_indent_str = "  ".repeat(indent + 1);
                let close_indent_str = "  ".repeat(indent);
                let mut inner = String::new();
                for (field_name, field_expr) in fields {
                    let expr_str = self.format_expr(field_expr, indent + 1, (indent + 1) * 2);
                    inner.push_str(&item_indent_str);
                    inner.push_str(field_name);
                    inner.push(' ');
                    inner.push_str(&expr_str);
                    inner.push('\n');
                }
                format!("{pub_prefix}skill {name} {{\n{inner}{close_indent_str}}}")
            }
            Node::EvalPackDecl { .. } => self.format_expr_or_stmt(node, indent, column),
            Node::LetBinding {
                pattern,
                type_ann,
                value,
                ..
            } => {
                let pat = format_pattern(pattern);
                let type_col = column + text_width("let ") + text_width(&pat) + 2;
                let type_str = format_type_ann_wrapped(type_ann, indent, type_col, self.line_width);
                let prefix = format!("let {pat}{type_str} = ");
                let val = self.format_expr(value, indent, column_after(column, &prefix));
                self.format_prefixed_value(&prefix, &val, indent, column)
            }
            Node::ConstBinding {
                pattern,
                type_ann,
                value,
                ..
            } => {
                let pat = format_pattern(pattern);
                let type_col = column + text_width("const ") + text_width(&pat) + 2;
                let type_str = format_type_ann_wrapped(type_ann, indent, type_col, self.line_width);
                let prefix = format!("const {pat}{type_str} = ");
                let val = self.format_expr(value, indent, column_after(column, &prefix));
                self.format_prefixed_value(&prefix, &val, indent, column)
            }
            Node::ImportDecl { path, is_pub } => {
                let prefix = if *is_pub { "pub " } else { "" };
                format!("{prefix}import \"{path}\"")
            }
            Node::SelectiveImport {
                names,
                path,
                is_pub,
            } => {
                let prefix = if *is_pub { "pub " } else { "" };
                let line = self.format_selective_import_names(
                    names,
                    path,
                    column + text_width(prefix),
                    indent,
                );
                format!("{prefix}{line}")
            }
            Node::EnumDecl { name, .. } => format!("/* enum {name} */"),
            Node::StructDecl { name, .. } => format!("/* struct {name} */"),
            Node::InterfaceDecl { name, .. } => format!("/* interface {name} */"),
            Node::ImplBlock { type_name, .. } => format!("/* impl {type_name} */"),
            Node::OverrideDecl { name, .. } => format!("/* override {name} */"),
            Node::TypeDecl {
                name,
                type_params,
                type_expr,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let params = format_type_params(type_params);
                let te = format_type_expr(type_expr);
                format!("{pub_prefix}type {name}{params} = {te}")
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let mut result = String::from("select {\n");
                let case_indent = indent + 1;
                let body_indent = case_indent + 1;
                let case_pad = "  ".repeat(case_indent);
                for case in cases {
                    let ch = self.format_expr(
                        &case.channel,
                        case_indent,
                        case_indent * 2 + text_width(&case.variable) + 6,
                    );
                    result.push_str(&format!("{case_pad}{} from {ch} {{\n", case.variable));
                    result.push_str(&self.format_body_string(
                        &case.body,
                        body_indent,
                        case.channel.span.end_line,
                    ));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                if let Some((dur, body)) = timeout {
                    let d = self.format_expr(dur, case_indent, case_indent * 2 + 8);
                    result.push_str(&format!("{case_pad}timeout {d} {{\n"));
                    result.push_str(&self.format_body_string(body, body_indent, dur.span.end_line));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                if let Some(body) = default_body {
                    result.push_str(&format!("{case_pad}default {{\n"));
                    result.push_str(&self.format_body_string(body, body_indent, node.span.line));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                let close = "  ".repeat(indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::Spread(inner) => format!("...{}", self.format_expr(inner, indent, column + 3)),
            Node::AttributedDecl { attributes, inner } => {
                let attrs = format_attributes(attributes);
                format!(
                    "{}{}",
                    attrs,
                    self.format_expr(inner, indent, column + text_width(&attrs))
                )
            }
            Node::OrPattern(alternatives) => alternatives
                .iter()
                .map(|p| self.format_expr(p, indent, column))
                .collect::<Vec<_>>()
                .join(" | "),
            Node::HitlExpr { kind, args } => {
                let arg_strs: Vec<String> = args
                    .iter()
                    .map(|arg| {
                        let value = self.format_expr(
                            &arg.value,
                            indent,
                            column + text_width(kind.as_keyword()) + 1,
                        );
                        match &arg.name {
                            Some(name) => format!("{name}: {value}"),
                            None => value,
                        }
                    })
                    .collect();
                format!("{}({})", kind.as_keyword(), arg_strs.join(", "))
            }
        }
    }

    fn format_arrow_closure(
        &self,
        params: &[TypedParam],
        body: &[SNode],
        column: usize,
        indent: usize,
        block_from_line: usize,
    ) -> String {
        let params_str = format_typed_params(params);
        if body.len() == 1
            && is_simple_expr(&body[0])
            && !self.body_carries_comment(body, block_from_line)
        {
            let expr = self.format_expr(&body[0], indent, column + text_width(&params_str) + 4);
            let inline = if params.is_empty() {
                format!("{{ -> {expr} }}")
            } else {
                format!("{{ {params_str} -> {expr} }}")
            };
            if !inline.contains('\n') && column + text_width(&inline) <= self.line_width {
                inline
            } else {
                let opening = if params.is_empty() {
                    "{ ->".to_string()
                } else {
                    format!("{{ {params_str} ->")
                };
                self.format_block_expr(&opening, body, indent, block_from_line)
            }
        } else {
            let opening = if params.is_empty() {
                String::from("{ ->")
            } else {
                format!("{{ {params_str} ->")
            };
            self.format_block_expr(&opening, body, indent, block_from_line)
        }
    }

    /// Render unusually deep left-associative `+` chains without recursing once
    /// per operand. Generated Harn fixtures commonly concatenate source text;
    /// keeping that path iterative prevents a valid fixture from exhausting
    /// the host thread's default stack.
    fn format_deep_plus_chain(&self, root: &SNode, indent: usize, column: usize) -> String {
        let mut terms: Vec<&SNode> = Vec::new();
        let mut cursor = root;
        loop {
            match &cursor.node {
                Node::BinaryOp { op, left, right } if op == "+" => {
                    terms.push(right.as_ref());
                    cursor = left.as_ref();
                }
                _ => {
                    terms.push(cursor);
                    break;
                }
            }
        }
        terms.reverse();

        let mut output = self.format_expr(terms[0], indent, column);
        for term in terms.into_iter().skip(1) {
            let mut right = self.format_expr(term, indent, column_after(column, &output) + 3);
            if child_needs_parens("+", &term.node, true) {
                right = format!("({right})");
            }
            let current_col = column_after(column, &output);
            if current_col + 3 + last_line_width(&right) > self.line_width {
                output.push('\n');
                output.push_str(&"  ".repeat(indent + 1));
                output.push_str("+ ");
            } else {
                output.push_str(" + ");
            }
            output.push_str(&right);
        }
        output
    }

    fn format_fn_closure(
        &self,
        params: &[TypedParam],
        return_type: Option<&TypeExpr>,
        throws: Option<&TypeExpr>,
        body: &[SNode],
        column: usize,
        indent: usize,
        block_from_line: usize,
    ) -> String {
        let params_str = format_typed_params(params);
        let return_str = return_type
            .map(|ty| format!(" -> {}", format_type_expr(ty)))
            .unwrap_or_default();
        let throws_str = throws
            .map(|ty| format!(" throws {}", format_type_expr(ty)))
            .unwrap_or_default();
        if body.len() == 1
            && is_simple_expr(&body[0])
            && !self.body_carries_comment(body, block_from_line)
        {
            let expr = self.format_expr(&body[0], indent, column + text_width(&params_str) + 4);
            format!("fn({params_str}){return_str}{throws_str} {{ {expr} }}")
        } else {
            self.format_block_expr(
                &format!("fn({params_str}){return_str}{throws_str} {{"),
                body,
                indent,
                block_from_line,
            )
        }
    }

    fn format_dict_key(&self, node: &SNode, indent: usize, column: usize) -> String {
        match &node.node {
            Node::StringLiteral(s) if is_identifier(s) => s.clone(),
            Node::StringLiteral(s) => format!("\"{}\"", escape_string(s)),
            _ => format!("[{}]", self.format_expr(node, indent, column + 1)),
        }
    }

    pub(super) fn format_dict_entry_list(
        &self,
        entries: &[harn_parser::DictEntry],
        prefix_col: usize,
        indent: usize,
        (open_line, close_line): (usize, usize),
    ) -> String {
        // Each entry value (and computed key) may itself wrap; if it does, it
        // lands at `indent + 1`, so render children at that depth.
        let mut from_line = open_line + 1;
        let items = entries
            .iter()
            .map(|e| {
                let body = if let Node::Spread(inner) = &e.value.node {
                    format!(
                        "...{}",
                        self.format_expr(inner, indent + 1, wrapped_item_column(indent) + 2)
                    )
                } else {
                    let item_col = wrapped_item_column(indent);
                    let k = self.format_dict_key(&e.key, indent + 1, item_col);
                    let value_col = item_col + text_width(&k) + 2;
                    let v = self.format_expr(&e.value, indent + 1, value_col);
                    if !v.contains('\n') && value_col + text_width(&v) > self.line_width {
                        format!("{k}:\n{}{}", "  ".repeat(indent + 2), v)
                    } else {
                        format!("{k}: {v}")
                    }
                };
                let item = self.commented_item(
                    body,
                    from_line,
                    e.key.span.line.min(e.value.span.line),
                    e.value.span.end_line,
                    close_line,
                );
                from_line = e.value.span.end_line + 1;
                item
            })
            .collect::<Vec<_>>();
        self.format_comma_sequence_commented(items, prefix_col, indent)
    }
}

fn plus_chain_depth(node: &SNode) -> usize {
    let mut depth = 0;
    let mut cursor = node;
    while let Node::BinaryOp { op, left, .. } = &cursor.node {
        if op != "+" {
            break;
        }
        depth += 1;
        cursor = left.as_ref();
    }
    depth
}
