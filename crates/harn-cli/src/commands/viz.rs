use std::fs;
use std::path::Path;
use std::process;

use harn_parser::{BindingPattern, MatchArm, Node, SNode};

pub(crate) fn run_viz(file: &str, output: Option<&str>) {
    let source = fs::read_to_string(file).unwrap_or_else(|error| {
        eprintln!("error: failed to read {file}: {error}");
        process::exit(1);
    });

    let mermaid = render_source_to_mermaid(&source).unwrap_or_else(|error| {
        eprintln!("error: failed to visualize {file}: {error}");
        process::exit(1);
    });

    if let Some(path) = output {
        if let Some(parent) = Path::new(path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, mermaid).unwrap_or_else(|error| {
            eprintln!("error: failed to write {path}: {error}");
            process::exit(1);
        });
    } else {
        print!("{mermaid}");
    }
}

fn render_source_to_mermaid(source: &str) -> Result<String, String> {
    let program = harn_parser::parse_source(source).map_err(|e| e.to_string())?;
    Ok(render_program_to_mermaid(&program))
}

fn render_program_to_mermaid(program: &[SNode]) -> String {
    let mut graph = MermaidGraph::new();
    let root = graph.node("module");
    for node in program {
        let (head, _) = graph.emit_node(node);
        graph.edge(&root, &head, None);
    }
    graph.render()
}

struct MermaidGraph {
    next_id: usize,
    lines: Vec<String>,
}

impl MermaidGraph {
    fn new() -> Self {
        Self {
            next_id: 0,
            lines: Vec::new(),
        }
    }

    fn render(self) -> String {
        let mut out = String::from("flowchart TD\n");
        for line in self.lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn node(&mut self, label: impl AsRef<str>) -> String {
        let id = format!("n{}", self.next_id);
        self.next_id += 1;
        self.lines.push(format!(
            "    {id}[\"{}\"]",
            escape_mermaid_label(label.as_ref())
        ));
        id
    }

    fn edge(&mut self, from: &str, to: &str, label: Option<&str>) {
        match label.filter(|value| !value.is_empty()) {
            Some(label) => self.lines.push(format!(
                "    {from} -- {} --> {to}",
                escape_mermaid_label(label)
            )),
            None => self.lines.push(format!("    {from} --> {to}")),
        }
    }

    fn emit_sequence(&mut self, body: &[SNode], empty_label: &str) -> (String, String) {
        if body.is_empty() {
            let node = self.node(empty_label);
            return (node.clone(), node);
        }

        let mut first = None::<String>;
        let mut previous = None::<String>;
        for stmt in body {
            let (head, tail) = self.emit_node(stmt);
            if let Some(prev) = previous.as_ref() {
                self.edge(prev, &head, None);
            } else {
                first = Some(head.clone());
            }
            previous = Some(tail);
        }

        (
            first.expect("non-empty sequence has a first node"),
            previous.expect("non-empty sequence has a last node"),
        )
    }

    fn emit_branch(&mut self, label: &str, body: &[SNode]) -> (String, String) {
        let branch = self.node(label);
        let (body_head, body_tail) = self.emit_sequence(body, label);
        self.edge(&branch, &body_head, None);
        (branch, body_tail)
    }

    fn emit_node(&mut self, node: &SNode) -> (String, String) {
        match &node.node {
            Node::Pipeline { name, body, .. } => {
                self.emit_named_block(&format!("pipeline {name}"), body)
            }
            Node::FnDecl { name, body, .. } => self.emit_named_block(&format!("fn {name}"), body),
            Node::ToolDecl { name, body, .. } => {
                self.emit_named_block(&format!("tool {name}"), body)
            }
            Node::OverrideDecl { name, body, .. } => {
                self.emit_named_block(&format!("override {name}"), body)
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let decision = self.node(format!("if {}", inline_label(condition)));
                let (then_head, then_tail) = self.emit_branch("then", then_body);
                let join = self.node("join");
                self.edge(&decision, &then_head, Some("yes"));
                self.edge(&then_tail, &join, None);

                if let Some(else_body) = else_body {
                    let (else_head, else_tail) = self.emit_branch("else", else_body);
                    self.edge(&decision, &else_head, Some("no"));
                    self.edge(&else_tail, &join, None);
                } else {
                    self.edge(&decision, &join, Some("no"));
                }

                (decision, join)
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let loop_node = self.node(format!(
                    "for {} in {}",
                    binding_pattern_label(pattern),
                    inline_label(iterable)
                ));
                let (body_head, body_tail) = self.emit_branch("body", body);
                let done = self.node("after loop");
                self.edge(&loop_node, &body_head, Some("each"));
                self.edge(&body_tail, &loop_node, None);
                self.edge(&loop_node, &done, Some("done"));
                (loop_node, done)
            }
            Node::WhileLoop { condition, body } => {
                let loop_node = self.node(format!("while {}", inline_label(condition)));
                let (body_head, body_tail) = self.emit_branch("body", body);
                let done = self.node("after loop");
                self.edge(&loop_node, &body_head, Some("true"));
                self.edge(&body_tail, &loop_node, None);
                self.edge(&loop_node, &done, Some("false"));
                (loop_node, done)
            }
            Node::MatchExpr { value, arms } => self.emit_match_expr(value, arms),
            Node::Retry { count, body } => {
                let retry = self.node(format!("retry {}", inline_label(count)));
                let (body_head, body_tail) = self.emit_branch("attempt", body);
                let done = self.node("retry done");
                self.edge(&retry, &body_head, Some("run"));
                self.edge(&body_tail, &retry, Some("retry"));
                self.edge(&retry, &done, Some("done"));
                (retry, done)
            }
            Node::TryCatch {
                body,
                error_var,
                catch_body,
                finally_body,
                ..
            } => {
                let try_node = self.node("try");
                let (body_head, body_tail) = self.emit_branch("try body", body);
                let catch_label = error_var
                    .as_deref()
                    .map(|name| format!("catch {name}"))
                    .unwrap_or_else(|| "catch".to_string());
                let (catch_head, catch_tail) = self.emit_branch(&catch_label, catch_body);
                let after = if finally_body.is_some() {
                    self.node("finally")
                } else {
                    self.node("after try")
                };

                self.edge(&try_node, &body_head, Some("ok"));
                self.edge(&body_tail, &after, None);
                self.edge(&try_node, &catch_head, Some("error"));
                self.edge(&catch_tail, &after, None);

                if let Some(finally_body) = finally_body {
                    let (finally_head, finally_tail) =
                        self.emit_branch("finally body", finally_body);
                    let done = self.node("after try");
                    self.edge(&after, &finally_head, None);
                    self.edge(&finally_tail, &done, None);
                    (try_node, done)
                } else {
                    (try_node, after)
                }
            }
            Node::TryExpr { body } => self.emit_named_block("try expr", body),
            Node::SpawnExpr { body } => self.emit_named_block("spawn", body),
            Node::DeferStmt { body } => self.emit_named_block("defer", body),
            Node::DeadlineBlock { duration, body } => {
                self.emit_named_block(&format!("deadline {}", inline_label(duration)), body)
            }
            Node::MutexBlock { body } => self.emit_named_block("mutex", body),
            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options: _,
            } => {
                let mode_label = match mode {
                    harn_parser::ParallelMode::Count => "parallel count",
                    harn_parser::ParallelMode::Each => "parallel each",
                    harn_parser::ParallelMode::Settle => "parallel settle",
                };
                let worker = variable
                    .as_deref()
                    .map(|name| format!(" -> {name}"))
                    .unwrap_or_default();
                let start = self.node(format!("{mode_label} {}{worker}", inline_label(expr)));
                let (body_head, body_tail) = self.emit_branch("parallel body", body);
                let done = self.node("parallel join");
                self.edge(&start, &body_head, Some("fan out"));
                self.edge(&body_tail, &done, None);
                (start, done)
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let select = self.node("select");
                let join = self.node("after select");
                for case in cases {
                    let label = format!("recv {}", case.variable);
                    let (case_head, case_tail) = self.emit_branch(&label, &case.body);
                    self.edge(&select, &case_head, Some(&inline_label(&case.channel)));
                    self.edge(&case_tail, &join, None);
                }
                if let Some((duration, body)) = timeout {
                    let (timeout_head, timeout_tail) =
                        self.emit_branch(&format!("timeout {}", inline_label(duration)), body);
                    self.edge(&select, &timeout_head, Some("timeout"));
                    self.edge(&timeout_tail, &join, None);
                }
                if let Some(body) = default_body {
                    let (default_head, default_tail) = self.emit_branch("default", body);
                    self.edge(&select, &default_head, Some("default"));
                    self.edge(&default_tail, &join, None);
                }
                (select, join)
            }
            Node::Block(body) => self.emit_named_block("block", body),
            _ => {
                let label = summarize_node(node);
                let entry = self.node(label);
                (entry.clone(), entry)
            }
        }
    }

    fn emit_named_block(&mut self, label: &str, body: &[SNode]) -> (String, String) {
        let start = self.node(label);
        let end = self.node(format!("end {label}"));
        if body.is_empty() {
            self.edge(&start, &end, None);
            return (start, end);
        }

        let (body_head, body_tail) = self.emit_sequence(body, label);
        self.edge(&start, &body_head, None);
        self.edge(&body_tail, &end, None);
        (start, end)
    }

    fn emit_match_expr(&mut self, value: &SNode, arms: &[MatchArm]) -> (String, String) {
        let match_node = self.node(format!("match {}", inline_label(value)));
        let join = self.node("after match");
        for arm in arms {
            let guard_suffix = arm
                .guard
                .as_ref()
                .map(|guard| format!(" if {}", inline_label(guard)))
                .unwrap_or_default();
            let label = format!("{}{}", inline_label(&arm.pattern), guard_suffix);
            let (arm_head, arm_tail) = self.emit_branch(&label, &arm.body);
            self.edge(&match_node, &arm_head, Some("arm"));
            self.edge(&arm_tail, &join, None);
        }
        (match_node, join)
    }
}

fn summarize_node(node: &SNode) -> String {
    match &node.node {
        Node::LetBinding { pattern, value, .. } => {
            format!(
                "let {} = {}",
                binding_pattern_label(pattern),
                inline_label(value)
            )
        }
        Node::VarBinding { pattern, value, .. } => {
            format!(
                "var {} = {}",
                binding_pattern_label(pattern),
                inline_label(value)
            )
        }
        Node::ImportDecl { path } => format!("import \"{}\"", truncate(path)),
        Node::SelectiveImport { names, path } => {
            format!(
                "import {{{}}} from \"{}\"",
                names.join(", "),
                truncate(path)
            )
        }
        Node::EnumDecl { name, .. } => format!("enum {name}"),
        Node::StructDecl { name, .. } => format!("struct {name}"),
        Node::InterfaceDecl { name, .. } => format!("interface {name}"),
        Node::ImplBlock { type_name, .. } => format!("impl {type_name}"),
        Node::TypeDecl { name, .. } => format!("type {name}"),
        Node::ReturnStmt { value } => value
            .as_ref()
            .map(|value| format!("return {}", inline_label(value)))
            .unwrap_or_else(|| "return".to_string()),
        Node::GuardStmt {
            condition,
            else_body,
        } => format!(
            "guard {} else ({} statements)",
            inline_label(condition),
            else_body.len()
        ),
        Node::RequireStmt { condition, message } => match message {
            Some(message) => format!(
                "require {} : {}",
                inline_label(condition),
                inline_label(message)
            ),
            None => format!("require {}", inline_label(condition)),
        },
        Node::YieldExpr { value } => value
            .as_ref()
            .map(|value| format!("yield {}", inline_label(value)))
            .unwrap_or_else(|| "yield".to_string()),
        Node::BreakStmt => "break".to_string(),
        Node::ContinueStmt => "continue".to_string(),
        Node::FunctionCall { name, .. } => format!("{name}(...)"),
        Node::MethodCall { object, method, .. } => {
            format!("{}.{}(...)", inline_label(object), method)
        }
        Node::OptionalMethodCall { object, method, .. } => {
            format!("{}?.{}(...)", inline_label(object), method)
        }
        Node::PropertyAccess { object, property } => {
            format!("{}.{}", inline_label(object), property)
        }
        Node::OptionalPropertyAccess { object, property } => {
            format!("{}?.{}", inline_label(object), property)
        }
        Node::SubscriptAccess { object, index } => {
            format!("{}[{}]", inline_label(object), inline_label(index))
        }
        Node::SliceAccess { object, .. } => format!("{}[..]", inline_label(object)),
        Node::BinaryOp { op, left, right } => {
            format!("{} {} {}", inline_label(left), op, inline_label(right))
        }
        Node::UnaryOp { op, operand } => format!("{op}{}", inline_label(operand)),
        Node::Ternary { condition, .. } => format!("{} ? ...", inline_label(condition)),
        Node::Assignment { target, value, op } => format!(
            "{} {} {}",
            inline_label(target),
            op.as_deref().unwrap_or("="),
            inline_label(value)
        ),
        Node::ThrowStmt { value } => format!("throw {}", inline_label(value)),
        Node::EnumConstruct {
            enum_name, variant, ..
        } => format!("{enum_name}.{variant}(...)"),
        Node::StructConstruct { struct_name, .. } => format!("{struct_name} {{...}}"),
        Node::InterpolatedString(_) => "interpolated string".to_string(),
        Node::StringLiteral(value) => format!("\"{}\"", truncate(value)),
        Node::RawStringLiteral(value) => format!("r\"{}\"", truncate(value)),
        Node::IntLiteral(value) => value.to_string(),
        Node::FloatLiteral(value) => value.to_string(),
        Node::BoolLiteral(value) => value.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::Identifier(name) => name.clone(),
        Node::ListLiteral(values) => format!("[{} items]", values.len()),
        Node::DictLiteral(entries) => format!("{{{} fields}}", entries.len()),
        Node::Spread(value) => format!("...{}", inline_label(value)),
        Node::TryOperator { operand } => format!("{}?", inline_label(operand)),
        Node::Closure { params, .. } => format!("closure ({})", params.len()),
        Node::DurationLiteral(value) => format!("{value}ms"),
        Node::RangeExpr {
            start,
            end,
            inclusive,
        } => format!(
            "{} {} {}",
            inline_label(start),
            if *inclusive { "thru" } else { "upto" },
            inline_label(end)
        ),
        Node::Pipeline { name, .. } => format!("pipeline {name}"),
        Node::FnDecl { name, .. } => format!("fn {name}"),
        Node::ToolDecl { name, .. } => format!("tool {name}"),
        Node::OverrideDecl { name, .. } => format!("override {name}"),
        Node::IfElse { condition, .. } => format!("if {}", inline_label(condition)),
        Node::ForIn {
            pattern, iterable, ..
        } => format!(
            "for {} in {}",
            binding_pattern_label(pattern),
            inline_label(iterable)
        ),
        Node::MatchExpr { value, .. } => format!("match {}", inline_label(value)),
        Node::WhileLoop { condition, .. } => format!("while {}", inline_label(condition)),
        Node::Retry { count, .. } => format!("retry {}", inline_label(count)),
        Node::TryCatch { .. } => "try/catch".to_string(),
        Node::TryExpr { .. } => "try expr".to_string(),
        Node::SpawnExpr { .. } => "spawn".to_string(),
        Node::DeferStmt { .. } => "defer".to_string(),
        Node::DeadlineBlock { duration, .. } => format!("deadline {}", inline_label(duration)),
        Node::MutexBlock { .. } => "mutex".to_string(),
        Node::Parallel { .. } => "parallel".to_string(),
        Node::SelectExpr { .. } => "select".to_string(),
        Node::Block(body) => format!("block ({} statements)", body.len()),
    }
}

fn binding_pattern_label(pattern: &BindingPattern) -> String {
    match pattern {
        BindingPattern::Identifier(name) => name.clone(),
        BindingPattern::Dict(fields) => {
            let names: Vec<String> = fields
                .iter()
                .map(|field| {
                    if field.is_rest {
                        format!("...{}", field.alias.as_deref().unwrap_or(&field.key))
                    } else {
                        field.alias.clone().unwrap_or_else(|| field.key.clone())
                    }
                })
                .collect();
            format!("{{{}}}", names.join(", "))
        }
        BindingPattern::List(items) => {
            let names: Vec<String> = items
                .iter()
                .map(|item| {
                    if item.is_rest {
                        format!("...{}", item.name)
                    } else {
                        item.name.clone()
                    }
                })
                .collect();
            format!("[{}]", names.join(", "))
        }
    }
}

fn inline_label(node: &SNode) -> String {
    match &node.node {
        Node::Identifier(name) => name.clone(),
        Node::StringLiteral(value) => format!("\"{}\"", truncate(value)),
        Node::RawStringLiteral(value) => format!("r\"{}\"", truncate(value)),
        Node::IntLiteral(value) => value.to_string(),
        Node::FloatLiteral(value) => value.to_string(),
        Node::BoolLiteral(value) => value.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::DurationLiteral(value) => format!("{value}ms"),
        Node::FunctionCall { name, .. } => format!("{name}(...)"),
        Node::MethodCall { object, method, .. } => {
            format!("{}.{}(...)", inline_label(object), method)
        }
        Node::OptionalMethodCall { object, method, .. } => {
            format!("{}?.{}(...)", inline_label(object), method)
        }
        Node::PropertyAccess { object, property } => {
            format!("{}.{}", inline_label(object), property)
        }
        Node::OptionalPropertyAccess { object, property } => {
            format!("{}?.{}", inline_label(object), property)
        }
        Node::SubscriptAccess { object, index } => {
            format!("{}[{}]", inline_label(object), inline_label(index))
        }
        Node::BinaryOp { op, left, right } => {
            format!("{} {} {}", inline_label(left), op, inline_label(right))
        }
        Node::UnaryOp { op, operand } => format!("{op}{}", inline_label(operand)),
        Node::EnumConstruct {
            enum_name, variant, ..
        } => format!("{enum_name}.{variant}(...)"),
        Node::StructConstruct { struct_name, .. } => format!("{struct_name} {{...}}"),
        Node::ListLiteral(values) => format!("[{} items]", values.len()),
        Node::DictLiteral(entries) => format!("{{{} fields}}", entries.len()),
        Node::TryOperator { operand } => format!("{}?", inline_label(operand)),
        Node::RangeExpr {
            start,
            end,
            inclusive,
        } => format!(
            "{} {} {}",
            inline_label(start),
            if *inclusive { "thru" } else { "upto" },
            inline_label(end)
        ),
        _ => summarize_node(node),
    }
}

fn truncate(value: &str) -> String {
    const MAX_CHARS: usize = 24;
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn escape_mermaid_label(label: &str) -> String {
    let mut escaped = String::new();
    for ch in label.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\n' | '\r' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::render_source_to_mermaid;

    #[test]
    fn renders_pipeline_with_branches_and_parallel_blocks() {
        let source = r#"
pipeline main(task) {
  let items = [1, 2]
  if ready {
    println("go")
  } else {
    println("wait")
  }
  parallel each items { item ->
    println(item)
  }
}
"#;

        let graph = render_source_to_mermaid(source).expect("graph");
        assert!(graph.contains("flowchart TD"));
        assert!(graph.contains("pipeline main"));
        assert!(graph.contains("if ready"));
        assert!(graph.contains("parallel each items -> item"));
    }

    #[test]
    fn renders_match_and_try_catch_nodes() {
        let source = r#"
pipeline main(task) {
  let value = 1
  match value {
    1 -> { println("one") }
    _ -> { println("other") }
  }
  try {
    risky()
  } catch err {
    println(err)
  } finally {
    println("done")
  }
}
"#;

        let graph = render_source_to_mermaid(source).expect("graph");
        assert!(graph.contains("match value"));
        assert!(graph.contains("catch err"));
        assert!(graph.contains("finally body"));
    }
}
