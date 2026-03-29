use std::collections::BTreeMap;

use harn_lexer::{Lexer, StringSegment, TokenKind};
use harn_parser::{
    BindingPattern, Node, Parser, SNode, TypeExpr, TypeParam, TypedParam, WhereClause,
};

/// Format a binding pattern to a string.
fn format_pattern(pattern: &BindingPattern) -> String {
    match pattern {
        BindingPattern::Identifier(name) => name.clone(),
        BindingPattern::Dict(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    if f.is_rest {
                        format!("...{}", f.key)
                    } else if let Some(alias) = &f.alias {
                        format!("{}: {}", f.key, alias)
                    } else {
                        f.key.clone()
                    }
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        BindingPattern::List(elements) => {
            let parts: Vec<String> = elements
                .iter()
                .map(|e| {
                    if e.is_rest {
                        format!("...{}", e.name)
                    } else {
                        e.name.clone()
                    }
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// Escape a string for embedding in double-quoted output.
fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// A captured comment with metadata.
#[derive(Debug, Clone)]
struct Comment {
    text: String,
    is_block: bool,
}

/// Format Harn source code to canonical style.
pub fn format_source(source: &str) -> Result<String, String> {
    // Lex once with comments, then partition
    let mut lexer = Lexer::new(source);
    let all_tokens = lexer.tokenize_with_comments().map_err(|e| e.to_string())?;

    // Extract comments by source line, and filter to parser tokens
    let mut comments: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut parser_tokens = Vec::with_capacity(all_tokens.len());
    for tok in all_tokens {
        match &tok.kind {
            TokenKind::LineComment(text) => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: false,
                });
            }
            TokenKind::BlockComment(text) => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: true,
                });
            }
            _ => parser_tokens.push(tok),
        }
    }

    let mut parser = Parser::new(parser_tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    let mut fmt = Formatter::new(comments);
    fmt.format_program(&program);
    Ok(fmt.finish())
}

struct Formatter {
    output: String,
    indent: usize,
    /// Line → comments on that line.
    comments: BTreeMap<usize, Vec<Comment>>,
    /// Track which comment lines have been emitted.
    emitted_lines: std::collections::HashSet<usize>,
}

impl Formatter {
    fn new(comments: BTreeMap<usize, Vec<Comment>>) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            comments,
            emitted_lines: std::collections::HashSet::new(),
        }
    }

    fn finish(mut self) -> String {
        // Trim trailing whitespace from each line, ensure single newline at end
        let trimmed: Vec<&str> = self.output.lines().map(|l| l.trim_end()).collect();
        self.output = trimmed.join("\n");
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }

    fn indent(&mut self) {
        self.indent += 1;
    }

    fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.output.push_str(s);
        self.output.push('\n');
    }

    /// Emit any comments on the given source line that haven't been emitted yet.
    fn emit_comments_for_line(&mut self, line: usize) {
        if self.emitted_lines.contains(&line) {
            return;
        }
        if let Some(comments) = self.comments.get(&line).cloned() {
            self.emitted_lines.insert(line);
            for c in &comments {
                if c.is_block {
                    self.writeln(&format!("/*{}*/", c.text));
                } else {
                    self.writeln(&format!("//{}", c.text));
                }
            }
        }
    }

    /// Emit any standalone comments whose line is between `from` and `to` (exclusive).
    fn emit_comments_in_range(&mut self, from: usize, to: usize) {
        let lines: Vec<usize> = self
            .comments
            .keys()
            .filter(|&&l| l >= from && l < to && !self.emitted_lines.contains(&l))
            .copied()
            .collect();
        for line in lines {
            self.emit_comments_for_line(line);
        }
    }

    fn format_program(&mut self, nodes: &[SNode]) {
        // Emit any leading comments before the first node
        if let Some(first) = nodes.first() {
            self.emit_comments_in_range(1, first.span.line);
        }
        for (i, node) in nodes.iter().enumerate() {
            if i > 0 {
                self.output.push('\n');
                // Emit comments between this node and the previous one
                let prev_end = nodes[i - 1].span.line + 1;
                self.emit_comments_in_range(prev_end, node.span.line);
            }
            self.format_node(node);
        }
        // Emit any trailing comments after the last node
        if !self.comments.is_empty() {
            let max_line = *self.comments.keys().max().unwrap_or(&0);
            let last_line = nodes.last().map(|n| n.span.line + 1).unwrap_or(1);
            self.emit_comments_in_range(last_line, max_line + 1);
        }
    }

    fn format_node(&mut self, node: &SNode) {
        let node_line = node.span.line;
        match &node.node {
            Node::Pipeline {
                name,
                params,
                body,
                extends,
            } => {
                let params_str = params.join(", ");
                let ext = if let Some(base) = extends {
                    format!(" extends {base}")
                } else {
                    String::new()
                };
                self.writeln(&format!("pipeline {name}({params_str}){ext} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                self.writeln(&format!("let {pat}{type_str} = {val}"));
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                self.writeln(&format!("var {pat}{type_str} = {val}"));
            }
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
            } => {
                let params_str = format_typed_params(params);
                let ret = if let Some(rt) = return_type {
                    format!(" -> {}", format_type_expr(rt))
                } else {
                    String::new()
                };
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let generics = format_type_params(type_params);
                let where_str = format_where_clauses(where_clauses);
                self.writeln(&format!(
                    "{pub_prefix}fn {name}{generics}({params_str}){ret}{where_str} {{"
                ));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("if {cond} {{"));
                self.indent();
                self.format_body(then_body, node_line);
                self.dedent();
                if let Some(eb) = else_body {
                    // Check if else body is a single if-else (else-if chain)
                    if eb.len() == 1 {
                        if let Node::IfElse { .. } = &eb[0].node {
                            self.write_indent();
                            self.output.push_str("} else ");
                            // Remove the indent that format_node would add
                            self.format_node_no_indent(&eb[0]);
                            return;
                        }
                    }
                    self.writeln("} else {");
                    self.indent();
                    self.format_body(eb, node_line);
                    self.dedent();
                    self.writeln("}");
                } else {
                    self.writeln("}");
                }
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let pat = format_pattern(pattern);
                let iter_str = self.format_expr(iterable);
                self.writeln(&format!("for {pat} in {iter_str} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("while {cond} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count);
                self.writeln(&format!("retry {cnt} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = match (error_var, error_type) {
                        (Some(var), Some(ty)) => {
                            format!(" ({var}: {})", format_type_expr(ty))
                        }
                        (Some(var), None) => format!(" ({var})"),
                        _ => String::new(),
                    };
                    self.writeln(&format!("}} catch{catch_param} {{"));
                    self.indent();
                    self.format_body(catch_body, node_line);
                    self.dedent();
                }
                if let Some(fb) = finally_body {
                    self.writeln("} finally {");
                    self.indent();
                    self.format_body(fb, node_line);
                    self.dedent();
                }
                self.writeln("}");
            }
            Node::TryExpr { body } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    self.writeln(&format!("return {v}"));
                } else {
                    self.writeln("return");
                }
            }
            Node::ThrowStmt { value } => {
                let v = self.format_expr(value);
                self.writeln(&format!("throw {v}"));
            }
            Node::BreakStmt => {
                self.writeln("break");
            }
            Node::ContinueStmt => {
                self.writeln("continue");
            }
            Node::ImportDecl { path } => {
                self.writeln(&format!("import \"{path}\""));
            }
            Node::SelectiveImport { names, path } => {
                let names_str = names.join(", ");
                self.writeln(&format!("import {{ {names_str} }} from \"{path}\""));
            }
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value);
                self.writeln(&format!("match {val} {{"));
                self.indent();
                for arm in arms {
                    self.format_match_arm(arm);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::EnumDecl { name, variants } => {
                self.writeln(&format!("enum {name} {{"));
                self.indent();
                for v in variants {
                    self.format_enum_variant(v);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::StructDecl { name, fields } => {
                self.writeln(&format!("struct {name} {{"));
                self.indent();
                for f in fields {
                    self.format_struct_field(f);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::InterfaceDecl { name, methods } => {
                self.writeln(&format!("interface {name} {{"));
                self.indent();
                for m in methods {
                    let params = format_typed_params(&m.params);
                    if let Some(ret) = &m.return_type {
                        self.writeln(&format!(
                            "fn {}({}) -> {}",
                            m.name,
                            params,
                            format_type_expr(ret)
                        ));
                    } else {
                        self.writeln(&format!("fn {}({})", m.name, params));
                    }
                }
                self.dedent();
                self.writeln("}");
            }
            Node::ImplBlock { type_name, methods } => {
                self.writeln(&format!("impl {type_name} {{"));
                self.indent();
                self.format_body(methods, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::Parallel {
                count,
                variable,
                body,
            } => {
                let cnt = self.format_expr(count);
                if let Some(var) = variable {
                    self.writeln(&format!("parallel({cnt}) {{ {var} ->"));
                } else {
                    self.writeln(&format!("parallel({cnt}) {{"));
                }
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.writeln(&format!("parallel_map({lst}) {{ {variable} ->"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.writeln(&format!("parallel_settle({lst}) {{ {variable} ->"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::SpawnExpr { body } => {
                self.writeln("spawn {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("guard {cond} else {{"));
                self.indent();
                self.format_body(else_body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration);
                self.writeln(&format!("deadline {dur} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::MutexBlock { body } => {
                self.writeln("mutex {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    self.writeln(&format!("yield {v}"));
                } else {
                    self.writeln("yield");
                }
            }
            Node::OverrideDecl { name, params, body } => {
                let params_str = params.join(", ");
                self.writeln(&format!("override {name}({params_str}) {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::TypeDecl { name, type_expr } => {
                let te = format_type_expr(type_expr);
                self.writeln(&format!("type {name} = {te}"));
            }
            Node::Block(stmts) => {
                self.writeln("{");
                self.indent();
                self.format_body(stmts, node_line);
                self.dedent();
                self.writeln("}");
            }
            // Everything else is an expression used as a statement
            _ => {
                let expr = self.format_expr(node);
                self.writeln(&expr);
            }
        }
    }

    /// Like format_node but without writing the leading indent (for else-if chains).
    fn format_node_no_indent(&mut self, node: &SNode) {
        let line = node.span.line;
        if let Node::IfElse {
            condition,
            then_body,
            else_body,
        } = &node.node
        {
            let cond = self.format_expr(condition);
            self.output.push_str(&format!("if {cond} {{\n"));
            self.indent();
            self.format_body(then_body, line);
            self.dedent();
            if let Some(eb) = else_body {
                if eb.len() == 1 {
                    if let Node::IfElse { .. } = &eb[0].node {
                        self.write_indent();
                        self.output.push_str("} else ");
                        self.format_node_no_indent(&eb[0]);
                        return;
                    }
                }
                self.writeln("} else {");
                self.indent();
                self.format_body(eb, line);
                self.dedent();
                self.writeln("}");
            } else {
                self.writeln("}");
            }
        }
    }

    fn format_body(&mut self, nodes: &[SNode], block_start_line: usize) {
        for (i, node) in nodes.iter().enumerate() {
            let range_start = if i > 0 {
                nodes[i - 1].span.line + 1
            } else {
                // For the first statement, emit comments between
                // the block opening and this node
                block_start_line + 1
            };
            self.emit_comments_in_range(range_start, node.span.line);
            self.format_node(node);
        }
    }

    fn format_expr(&self, node: &SNode) -> String {
        match &node.node {
            Node::StringLiteral(s) => {
                let escaped = escape_string(s);
                format!("\"{escaped}\"")
            }
            Node::InterpolatedString(segments) => {
                let mut result = String::from("\"");
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => {
                            result.push_str(&escape_string(s));
                        }
                        StringSegment::Expression(e) => {
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
                let l = self.format_expr(left);
                let r = self.format_expr(right);
                // Preserve multiline formatting when the operands span lines
                if left.span.line < right.span.line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{l}\n{pad}{op} {r}")
                } else {
                    format!("{l} {op} {r}")
                }
            }
            Node::UnaryOp { op, operand } => {
                let expr = self.format_expr(operand);
                format!("{op}{expr}")
            }
            Node::TryOperator { operand } => {
                let expr = self.format_expr(operand);
                format!("{expr}?")
            }
            Node::FunctionCall { name, args } => {
                let args_str = args
                    .iter()
                    .map(|a| self.format_expr(a))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}({args_str})")
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                let obj = self.format_expr(object);
                let args_str = args
                    .iter()
                    .map(|a| self.format_expr(a))
                    .collect::<Vec<_>>()
                    .join(", ");
                // Preserve multiline method chains: if the `.` was on a
                // later line than where the object expression ends, keep
                // it on its own line.
                if object.span.end_line > 0 && node.span.end_line > object.span.end_line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{obj}\n{pad}.{method}({args_str})")
                } else {
                    format!("{obj}.{method}({args_str})")
                }
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let obj = self.format_expr(object);
                let args_str = args
                    .iter()
                    .map(|a| self.format_expr(a))
                    .collect::<Vec<_>>()
                    .join(", ");
                if object.span.end_line > 0 && node.span.end_line > object.span.end_line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{obj}\n{pad}?.{method}({args_str})")
                } else {
                    format!("{obj}?.{method}({args_str})")
                }
            }
            Node::PropertyAccess { object, property } => {
                let obj = self.format_expr(object);
                format!("{obj}.{property}")
            }
            Node::OptionalPropertyAccess { object, property } => {
                let obj = self.format_expr(object);
                format!("{obj}?.{property}")
            }
            Node::SubscriptAccess { object, index } => {
                let obj = self.format_expr(object);
                let idx = self.format_expr(index);
                format!("{obj}[{idx}]")
            }
            Node::SliceAccess { object, start, end } => {
                let obj = self.format_expr(object);
                let s = start
                    .as_ref()
                    .map(|n| self.format_expr(n))
                    .unwrap_or_default();
                let e = end
                    .as_ref()
                    .map(|n| self.format_expr(n))
                    .unwrap_or_default();
                format!("{obj}[{s}:{e}]")
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond = self.format_expr(condition);
                let t = self.format_expr(true_expr);
                let f = self.format_expr(false_expr);
                format!("{cond} ? {t} : {f}")
            }
            Node::Assignment {
                target, value, op, ..
            } => {
                let t = self.format_expr(target);
                let v = self.format_expr(value);
                if let Some(op) = op {
                    format!("{t} {op}= {v}")
                } else {
                    format!("{t} = {v}")
                }
            }
            Node::ListLiteral(elems) => {
                let items = elems
                    .iter()
                    .map(|e| self.format_expr(e))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{items}]")
            }
            Node::DictLiteral(entries) => self.format_dict_entries(entries),
            Node::RangeExpr {
                start,
                end,
                inclusive,
            } => {
                let s = self.format_expr(start);
                let e = self.format_expr(end);
                let kw = if *inclusive { "thru" } else { "upto" };
                format!("{s} {kw} {e}")
            }
            Node::Closure {
                params,
                body,
                fn_syntax,
            } => {
                if *fn_syntax {
                    self.format_fn_closure(params, body)
                } else {
                    self.format_arrow_closure(params, body)
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
                    let args_str = args
                        .iter()
                        .map(|a| self.format_expr(a))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{enum_name}.{variant}({args_str})")
                }
            }
            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                let items = self.format_dict_entry_list(fields);
                format!("{struct_name} {{{items}}}")
            }
            Node::AskExpr { fields } => {
                let items = self.format_dict_entry_list(fields);
                format!("ask {{{items}}}")
            }
            Node::SpawnExpr { body } => {
                let mut result = String::from("spawn {\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    format!("yield {v}")
                } else {
                    "yield".to_string()
                }
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    format!("return {v}")
                } else {
                    "return".to_string()
                }
            }
            Node::ThrowStmt { value } => {
                let v = self.format_expr(value);
                format!("throw {v}")
            }
            Node::BreakStmt => "break".to_string(),
            Node::ContinueStmt => "continue".to_string(),
            Node::Block(stmts) => {
                let mut result = String::from("{\n");
                let current_indent = self.indent + 1;
                for n in stmts {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value);
                let mut result = format!("match {val} {{\n");
                let arm_indent = self.indent + 1;
                for arm in arms {
                    let indent_str = "  ".repeat(arm_indent);
                    let pattern = self.format_expr(&arm.pattern);
                    if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
                        let expr = self.format_expr(&arm.body[0]);
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern} -> {{ {expr} }}\n"));
                    } else {
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern} -> {{\n"));
                        let body_indent = arm_indent + 1;
                        for n in &arm.body {
                            let bi = "  ".repeat(body_indent);
                            let expr = self.format_expr_or_stmt(n, body_indent);
                            result.push_str(&bi);
                            result.push_str(&expr);
                            result.push('\n');
                        }
                        result.push_str(&indent_str);
                        result.push_str("}\n");
                    }
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                let mut result = format!("guard {cond} else {{\n");
                let current_indent = self.indent + 1;
                for n in else_body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration);
                let mut result = format!("deadline {dur} {{\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::MutexBlock { body } => {
                let mut result = String::from("mutex {\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close_indent = "  ".repeat(self.indent);
                result.push_str(&close_indent);
                result.push('}');
                result
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                let mut result = format!("if {cond} {{\n");
                let current_indent = self.indent + 1;
                for n in then_body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                if let Some(eb) = else_body {
                    result.push_str(&close);
                    result.push_str("} else {\n");
                    for n in eb {
                        let indent_str = "  ".repeat(current_indent);
                        let expr = self.format_expr_or_stmt(n, current_indent);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                    result.push_str(&close);
                    result.push('}');
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
                let iter_str = self.format_expr(iterable);
                let mut result = format!("for {pat} in {iter_str} {{\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                let mut result = format!("while {cond} {{\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count);
                let mut result = format!("retry {cnt} {{\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                let mut result = String::from("try {\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = match (error_var, error_type) {
                        (Some(var), Some(ty)) => {
                            format!(" ({var}: {})", format_type_expr(ty))
                        }
                        (Some(var), None) => format!(" ({var})"),
                        _ => String::new(),
                    };
                    result.push_str(&close);
                    result.push_str(&format!("}} catch{catch_param} {{\n"));
                    for n in catch_body {
                        let indent_str = "  ".repeat(current_indent);
                        let expr = self.format_expr_or_stmt(n, current_indent);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                }
                if let Some(fb) = finally_body {
                    result.push_str(&close);
                    result.push_str("} finally {\n");
                    for n in fb {
                        let indent_str = "  ".repeat(current_indent);
                        let expr = self.format_expr_or_stmt(n, current_indent);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                }
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::TryExpr { body } => {
                let mut result = String::from("try {\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::Parallel {
                count,
                variable,
                body,
            } => {
                let cnt = self.format_expr(count);
                let current_indent = self.indent + 1;
                let mut result = if let Some(var) = variable {
                    format!("parallel({cnt}) {{ {var} ->\n")
                } else {
                    format!("parallel({cnt}) {{\n")
                };
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                let current_indent = self.indent + 1;
                let mut result = format!("parallel_map({lst}) {{ {variable} ->\n");
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                let current_indent = self.indent + 1;
                let mut result = format!("parallel_settle({lst}) {{ {variable} ->\n");
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            // Declaration nodes that cannot appear as expressions
            Node::Pipeline { name, .. } => format!("/* pipeline {name} */"),
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
            } => {
                let params_str = format_typed_params(params);
                let ret = if let Some(rt) = return_type {
                    format!(" -> {}", format_type_expr(rt))
                } else {
                    String::new()
                };
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let generics = format_type_params(type_params);
                let where_str = format_where_clauses(where_clauses);
                let mut result =
                    format!("{pub_prefix}fn {name}{generics}({params_str}){ret}{where_str} {{\n");
                let current_indent = self.indent + 1;
                for n in body {
                    let indent_str = "  ".repeat(current_indent);
                    let expr = self.format_expr_or_stmt(n, current_indent);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("let {pat}{type_str} = {val}")
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("var {pat}{type_str} = {val}")
            }
            Node::ImportDecl { path } => format!("import \"{path}\""),
            Node::SelectiveImport { names, path } => {
                format!("import {{ {} }} from \"{path}\"", names.join(", "))
            }
            Node::EnumDecl { name, .. } => format!("/* enum {name} */"),
            Node::StructDecl { name, .. } => format!("/* struct {name} */"),
            Node::InterfaceDecl { name, .. } => format!("/* interface {name} */"),
            Node::ImplBlock { type_name, .. } => format!("/* impl {type_name} */"),
            Node::OverrideDecl { name, .. } => format!("/* override {name} */"),
            Node::TypeDecl { name, type_expr } => {
                let te = format_type_expr(type_expr);
                format!("type {name} = {te}")
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let mut result = String::from("select {\n");
                let current_indent = self.indent + 1;
                for case in cases {
                    let indent_str = "  ".repeat(current_indent);
                    let ch = self.format_expr(&case.channel);
                    result.push_str(&format!("{indent_str}{} from {ch} {{\n", case.variable));
                    for n in &case.body {
                        let inner = "  ".repeat(current_indent + 1);
                        let expr = self.format_expr_or_stmt(n, current_indent + 1);
                        result.push_str(&inner);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                    result.push_str(&indent_str);
                    result.push_str("}\n");
                }
                if let Some((dur, body)) = timeout {
                    let indent_str = "  ".repeat(current_indent);
                    let d = self.format_expr(dur);
                    result.push_str(&format!("{indent_str}timeout {d} {{\n"));
                    for n in body {
                        let inner = "  ".repeat(current_indent + 1);
                        let expr = self.format_expr_or_stmt(n, current_indent + 1);
                        result.push_str(&inner);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                    result.push_str(&indent_str);
                    result.push_str("}\n");
                }
                if let Some(body) = default_body {
                    let indent_str = "  ".repeat(current_indent);
                    result.push_str(&format!("{indent_str}default {{\n"));
                    for n in body {
                        let inner = "  ".repeat(current_indent + 1);
                        let expr = self.format_expr_or_stmt(n, current_indent + 1);
                        result.push_str(&inner);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                    result.push_str(&indent_str);
                    result.push_str("}\n");
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::Spread(inner) => {
                let expr = self.format_expr(inner);
                format!("...{expr}")
            }
        }
    }

    /// Format a closure using arrow syntax: `{ params -> body }`
    fn format_arrow_closure(&self, params: &[TypedParam], body: &[SNode]) -> String {
        let params_str = format_typed_params(params);
        if body.len() == 1 && is_simple_expr(&body[0]) {
            let expr = self.format_expr(&body[0]);
            if params.is_empty() {
                format!("{{ {expr} }}")
            } else {
                format!("{{ {params_str} -> {expr} }}")
            }
        } else {
            let mut result = String::new();
            if params.is_empty() {
                result.push_str("{\n");
            } else {
                result.push_str(&format!("{{ {params_str} ->\n"));
            }
            let current_indent = self.indent + 1;
            for n in body {
                let indent_str = "  ".repeat(current_indent);
                let expr = self.format_expr_or_stmt(n, current_indent);
                result.push_str(&indent_str);
                result.push_str(&expr);
                result.push('\n');
            }
            let close_indent = "  ".repeat(self.indent);
            result.push_str(&close_indent);
            result.push('}');
            result
        }
    }

    /// Format a closure using fn syntax: `fn(params) { body }`
    fn format_fn_closure(&self, params: &[TypedParam], body: &[SNode]) -> String {
        let params_str = format_typed_params(params);
        if body.len() == 1 && is_simple_expr(&body[0]) {
            let expr = self.format_expr(&body[0]);
            format!("fn({params_str}) {{ {expr} }}")
        } else {
            let mut result = format!("fn({params_str}) {{\n");
            let current_indent = self.indent + 1;
            for n in body {
                let indent_str = "  ".repeat(current_indent);
                let expr = self.format_expr_or_stmt(n, current_indent);
                result.push_str(&indent_str);
                result.push_str(&expr);
                result.push('\n');
            }
            let close_indent = "  ".repeat(self.indent);
            result.push_str(&close_indent);
            result.push('}');
            result
        }
    }

    /// Format a dict key: the parser stores identifier keys as StringLiteral,
    /// so we format them back as bare identifiers (without quotes).
    fn format_dict_key(&self, node: &SNode) -> String {
        match &node.node {
            Node::StringLiteral(s) if is_identifier(s) => s.clone(),
            // Non-string-literal keys are computed — wrap in [...]
            _ => format!("[{}]", self.format_expr(node)),
        }
    }

    fn format_dict_entries(&self, entries: &[harn_parser::DictEntry]) -> String {
        let items = self.format_dict_entry_list(entries);
        format!("{{{items}}}")
    }

    fn format_dict_entry_list(&self, entries: &[harn_parser::DictEntry]) -> String {
        entries
            .iter()
            .map(|e| {
                // Spread entry: ...expr
                if let Node::Spread(inner) = &e.value.node {
                    let v = self.format_expr(inner);
                    return format!("...{v}");
                }
                let k = self.format_dict_key(&e.key);
                let v = self.format_expr(&e.value);
                format!("{k}: {v}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format a node as either an expression string or a statement string,
    /// used for closure/block body formatting from expression context.
    fn format_expr_or_stmt(&self, node: &SNode, indent_level: usize) -> String {
        match &node.node {
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                let mut result = format!("if {cond} {{\n");
                for n in then_body {
                    let indent_str = "  ".repeat(indent_level + 1);
                    let expr = self.format_expr_or_stmt(n, indent_level + 1);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(indent_level);
                if let Some(eb) = else_body {
                    result.push_str(&close);
                    result.push_str("} else {\n");
                    for n in eb {
                        let indent_str = "  ".repeat(indent_level + 1);
                        let expr = self.format_expr_or_stmt(n, indent_level + 1);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                    result.push_str(&close);
                    result.push('}');
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
                let iter_str = self.format_expr(iterable);
                let mut result = format!("for {pat} in {iter_str} {{\n");
                for n in body {
                    let indent_str = "  ".repeat(indent_level + 1);
                    let expr = self.format_expr_or_stmt(n, indent_level + 1);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(indent_level);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                let mut result = format!("while {cond} {{\n");
                for n in body {
                    let indent_str = "  ".repeat(indent_level + 1);
                    let expr = self.format_expr_or_stmt(n, indent_level + 1);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(indent_level);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
            } => {
                let params_str = format_typed_params(params);
                let ret = if let Some(rt) = return_type {
                    format!(" -> {}", format_type_expr(rt))
                } else {
                    String::new()
                };
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let generics = format_type_params(type_params);
                let where_str = format_where_clauses(where_clauses);
                let mut result =
                    format!("{pub_prefix}fn {name}{generics}({params_str}){ret}{where_str} {{\n");
                for n in body {
                    let indent_str = "  ".repeat(indent_level + 1);
                    let expr = self.format_expr_or_stmt(n, indent_level + 1);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(indent_level);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("let {pat}{type_str} = {val}")
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("var {pat}{type_str} = {val}")
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                let mut result = String::from("try {\n");
                for n in body {
                    let indent_str = "  ".repeat(indent_level + 1);
                    let expr = self.format_expr_or_stmt(n, indent_level + 1);
                    result.push_str(&indent_str);
                    result.push_str(&expr);
                    result.push('\n');
                }
                let close = "  ".repeat(indent_level);
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = match (error_var, error_type) {
                        (Some(var), Some(ty)) => {
                            format!(" ({var}: {})", format_type_expr(ty))
                        }
                        (Some(var), None) => format!(" ({var})"),
                        _ => String::new(),
                    };
                    result.push_str(&close);
                    result.push_str(&format!("}} catch{catch_param} {{\n"));
                    for n in catch_body {
                        let indent_str = "  ".repeat(indent_level + 1);
                        let expr = self.format_expr_or_stmt(n, indent_level + 1);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                }
                if let Some(fb) = finally_body {
                    result.push_str(&close);
                    result.push_str("} finally {\n");
                    for n in fb {
                        let indent_str = "  ".repeat(indent_level + 1);
                        let expr = self.format_expr_or_stmt(n, indent_level + 1);
                        result.push_str(&indent_str);
                        result.push_str(&expr);
                        result.push('\n');
                    }
                }
                result.push_str(&close);
                result.push('}');
                result
            }
            _ => self.format_expr(node),
        }
    }

    fn format_match_arm(&mut self, arm: &harn_parser::MatchArm) {
        let pattern = self.format_expr(&arm.pattern);
        if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
            let expr = self.format_expr(&arm.body[0]);
            self.writeln(&format!("{pattern} -> {{ {expr} }}"));
        } else {
            self.writeln(&format!("{pattern} -> {{"));
            self.indent();
            self.format_body(&arm.body, arm.pattern.span.line);
            self.dedent();
            self.writeln("}");
        }
    }

    fn format_enum_variant(&mut self, v: &harn_parser::EnumVariant) {
        if v.fields.is_empty() {
            self.writeln(&v.name);
        } else {
            let fields = format_typed_params(&v.fields);
            self.writeln(&format!("{}({fields})", v.name));
        }
    }

    fn format_struct_field(&mut self, f: &harn_parser::StructField) {
        let opt = if f.optional { "?" } else { "" };
        if let Some(te) = &f.type_expr {
            let type_str = format_type_expr(te);
            self.writeln(&format!("{}{opt}: {type_str}", f.name));
        } else {
            self.writeln(&format!("{}{opt}", f.name));
        }
    }
}

fn format_type_ann(type_ann: &Option<TypeExpr>) -> String {
    if let Some(te) = type_ann {
        format!(": {}", format_type_expr(te))
    } else {
        String::new()
    }
}

fn format_type_expr(te: &TypeExpr) -> String {
    match te {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Union(types) => types
            .iter()
            .map(format_type_expr)
            .collect::<Vec<_>>()
            .join(" | "),
        TypeExpr::Shape(fields) => {
            let items = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type_expr(&f.type_expr))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{items}}}")
        }
        TypeExpr::List(inner) => {
            format!("list<{}>", format_type_expr(inner))
        }
        TypeExpr::DictType(k, v) => {
            format!("dict<{}, {}>", format_type_expr(k), format_type_expr(v))
        }
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            let params_str = params
                .iter()
                .map(format_type_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({}) -> {}", params_str, format_type_expr(return_type))
        }
    }
}

fn format_type_params(type_params: &[TypeParam]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = type_params.iter().map(|tp| tp.name.as_str()).collect();
        format!("<{}>", names.join(", "))
    }
}

fn format_where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = clauses
            .iter()
            .map(|c| format!("{}: {}", c.type_name, c.bound))
            .collect();
        format!(" where {}", parts.join(", "))
    }
}

/// Format an expression inline for use in parameter defaults.
fn format_inline_expr(node: &SNode) -> String {
    let fmt = Formatter::new(BTreeMap::new());
    fmt.format_expr(node)
}

fn format_typed_params(params: &[TypedParam]) -> String {
    params
        .iter()
        .map(|p| {
            let mut s = if let Some(te) = &p.type_expr {
                format!("{}: {}", p.name, format_type_expr(te))
            } else {
                p.name.clone()
            };
            if let Some(default) = &p.default_value {
                s.push_str(&format!(" = {}", format_inline_expr(default)));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_duration(ms: u64) -> String {
    if ms == 0 {
        return "0ms".to_string();
    }
    if ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

fn format_float(f: f64) -> String {
    let s = f.to_string();
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_simple_expr(node: &SNode) -> bool {
    matches!(
        &node.node,
        Node::StringLiteral(_)
            | Node::InterpolatedString(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::Identifier(_)
            | Node::DurationLiteral(_)
            | Node::BinaryOp { .. }
            | Node::UnaryOp { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::OptionalMethodCall { .. }
            | Node::PropertyAccess { .. }
            | Node::OptionalPropertyAccess { .. }
            | Node::SubscriptAccess { .. }
            | Node::SliceAccess { .. }
            | Node::Ternary { .. }
            | Node::Assignment { .. }
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::RangeExpr { .. }
            | Node::EnumConstruct { .. }
            | Node::TryOperator { .. }
            | Node::ReturnStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_roundtrip(source: &str) {
        let formatted = format_source(source).unwrap();
        let mut lexer = Lexer::new(&formatted);
        let tokens = lexer
            .tokenize()
            .unwrap_or_else(|e| panic!("Formatted output failed to lex:\n{formatted}\nError: {e}"));
        let mut parser = Parser::new(tokens);
        parser.parse().unwrap_or_else(|e| {
            panic!("Formatted output failed to parse:\n{formatted}\nError: {e}")
        });
        // Format again and verify idempotence
        let formatted2 = format_source(&formatted).unwrap();
        assert_eq!(formatted, formatted2, "Formatter is not idempotent");
    }

    #[test]
    fn test_roundtrip_basic() {
        assert_roundtrip("pipeline default(task) { let x = 42\nlog(x) }");
    }

    #[test]
    fn test_roundtrip_fn_decl() {
        assert_roundtrip(
            "pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(1, 2)) }",
        );
    }

    #[test]
    fn test_roundtrip_closure() {
        assert_roundtrip("pipeline default(task) { let f = { x -> x * 2 }\nlog(f(3)) }");
    }

    #[test]
    fn test_roundtrip_if_else() {
        assert_roundtrip("pipeline default(task) { if true { log(1) } else { log(2) } }");
    }

    #[test]
    fn test_roundtrip_try_catch() {
        assert_roundtrip(r#"pipeline default(task) { try { throw "e" } catch (e) { log(e) } }"#);
    }

    #[test]
    fn test_roundtrip_for_in() {
        assert_roundtrip("pipeline default(task) { for i in [1, 2, 3] { log(i) } }");
    }

    #[test]
    fn test_roundtrip_match() {
        assert_roundtrip(
            r#"pipeline default(task) { match x { "a" -> { log(1) } "b" -> { log(2) } } }"#,
        );
    }

    #[test]
    fn test_roundtrip_computed_dict_key() {
        assert_roundtrip(
            r#"pipeline default(task) { let k = "x"
  let d = {[k]: 42, fixed: 1} }"#,
        );
    }

    #[test]
    fn test_roundtrip_interface() {
        assert_roundtrip(
            "interface Printable {\n  fn to_display() -> string\n}\npipeline default(task) { log(1) }",
        );
    }

    #[test]
    fn test_roundtrip_enum() {
        assert_roundtrip(
            "enum Color {\n  Red\n  Green\n  Blue\n}\npipeline default(task) { log(1) }",
        );
    }

    #[test]
    fn test_format_hello() {
        let source = r#"pipeline default(task) {
  log("Hello, Harn!")
}"#;
        let result = format_source(source).unwrap();
        assert_eq!(
            result,
            "pipeline default(task) {\n  log(\"Hello, Harn!\")\n}\n"
        );
    }

    #[test]
    fn test_format_let_var() {
        let source = r#"pipeline default(task) {
  let x = 42
  var y = "hello"
}"#;
        let result = format_source(source).unwrap();
        assert!(result.contains("let x = 42"));
        assert!(result.contains("var y = \"hello\""));
    }

    #[test]
    fn test_format_binary_ops() {
        let source = r#"pipeline default(task) {
  let x = 1 + 2
  let y = a * b
}"#;
        let result = format_source(source).unwrap();
        assert!(result.contains("1 + 2"));
        assert!(result.contains("a * b"));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(5000), "5s");
        assert_eq!(format_duration(60000), "1m");
        assert_eq!(format_duration(3600000), "1h");
        assert_eq!(format_duration(500), "500ms");
    }

    #[test]
    fn test_format_if_else() {
        let source = r#"pipeline default(task) {
  if x > 0 {
    log("positive")
  } else {
    log("non-positive")
  }
}"#;
        let result = format_source(source).unwrap();
        assert!(result.contains("if x > 0 {"));
        assert!(result.contains("} else {"));
    }

    #[test]
    fn test_format_for_in() {
        let source = r#"pipeline default(task) {
  for i in [1, 2, 3] {
    log(i)
  }
}"#;
        let result = format_source(source).unwrap();
        assert!(result.contains("for i in [1, 2, 3] {"));
    }

    #[test]
    fn test_format_fn() {
        let source = r#"pipeline default(task) {
  fn add(a, b) {
    return a + b
  }
}"#;
        let result = format_source(source).unwrap();
        assert!(result.contains("fn add(a, b) {"));
        assert!(result.contains("return a + b"));
    }

    #[test]
    fn test_single_newline_at_end() {
        let source = r#"pipeline default(task) {
  log("hello")
}"#;
        let result = format_source(source).unwrap();
        assert!(result.ends_with("}\n"));
        assert!(!result.ends_with("}\n\n"));
    }

    #[test]
    fn test_no_trailing_whitespace() {
        let source = r#"pipeline default(task) {
  log("hello")
}"#;
        let result = format_source(source).unwrap();
        for line in result.lines() {
            assert_eq!(
                line,
                line.trim_end(),
                "Line has trailing whitespace: {:?}",
                line
            );
        }
    }
}
