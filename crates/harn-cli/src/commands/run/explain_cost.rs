use std::collections::BTreeMap;
use std::fmt::Write as _;

use harn_parser::{DictEntry, Node, SNode};

const DEFAULT_AGENT_ITERATIONS: i64 = 50;

pub(super) fn render_explain_cost(path: &str, program: &[SNode]) -> String {
    let mut analyzer = CostAnalyzer {
        path: harn_parser::diagnostic::normalize_diagnostic_path(path),
        inherited_options: Vec::new(),
        rows: Vec::new(),
    };
    analyzer.walk_nodes(program);
    render_report(&analyzer.rows)
}

#[derive(Clone, Debug)]
enum StaticValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    List(Vec<StaticValue>),
    Dict(BTreeMap<String, StaticValue>),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CostCell {
    Amount,
    Unpriced,
    Unresolved,
}

#[derive(Clone, Debug)]
struct CostRow {
    callsite: String,
    model: Option<String>,
    input_tokens: Option<i64>,
    iterations: Option<i64>,
    cost_usd: Option<f64>,
    cost_cell: CostCell,
}

struct CostAnalyzer {
    path: String,
    inherited_options: Vec<BTreeMap<String, StaticValue>>,
    rows: Vec<CostRow>,
}

impl CostAnalyzer {
    fn walk_nodes(&mut self, nodes: &[SNode]) {
        for node in nodes {
            self.walk_node(node);
        }
    }

    fn walk_node(&mut self, node: &SNode) {
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                if name == "llm_call" || name == "agent_loop" {
                    self.rows.push(self.analyze_call(node, name, args));
                }
                self.walk_nodes(args);
            }
            Node::ValueCall { callee, args } => {
                self.walk_node(callee);
                self.walk_nodes(args);
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                self.walk_node(object);
                self.walk_nodes(args);
            }
            Node::AttributedDecl { inner, .. } => self.walk_node(inner),
            Node::CostRoute { options, body } => {
                let mut inherited = BTreeMap::new();
                for frame in &self.inherited_options {
                    inherited.extend(frame.clone());
                }
                for (key, value) in options {
                    inherited.insert(key.clone(), eval_static(value));
                    self.walk_node(value);
                }
                self.inherited_options.push(inherited);
                self.walk_nodes(body);
                self.inherited_options.pop();
            }
            Node::HitlExpr { args, .. } => {
                for arg in args {
                    self.walk_node(&arg.value);
                }
            }
            Node::Pipeline { body, .. }
            | Node::OverrideDecl { body, .. }
            | Node::FnDecl { body, .. }
            | Node::ToolDecl { body, .. }
            | Node::SpawnExpr { body }
            | Node::TryExpr { body }
            | Node::Block(body)
            | Node::Closure { body, .. }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body, .. } => self.walk_nodes(body),
            Node::SkillDecl { fields, .. } => {
                for (_, value) in fields {
                    self.walk_node(value);
                }
            }
            Node::EvalPackDecl {
                fields,
                body,
                summarize,
                ..
            } => {
                for (_, value) in fields {
                    self.walk_node(value);
                }
                self.walk_nodes(body);
                if let Some(body) = summarize {
                    self.walk_nodes(body);
                }
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
                ..
            } => {
                self.walk_node(condition);
                self.walk_nodes(then_body);
                if let Some(body) = else_body {
                    self.walk_nodes(body);
                }
            }
            Node::ForIn { iterable, body, .. } => {
                self.walk_node(iterable);
                self.walk_nodes(body);
            }
            Node::WhileLoop { condition, body } => {
                self.walk_node(condition);
                self.walk_nodes(body);
            }
            Node::Retry { count, body } => {
                self.walk_node(count);
                self.walk_nodes(body);
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                self.walk_nodes(body);
                self.walk_nodes(catch_body);
                if let Some(body) = finally_body {
                    self.walk_nodes(body);
                }
            }
            Node::DeadlineBlock { duration, body } => {
                self.walk_node(duration);
                self.walk_nodes(body);
            }
            Node::MatchExpr { value, arms } => {
                self.walk_node(value);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.walk_node(guard);
                    }
                    self.walk_nodes(&arm.body);
                }
            }
            Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
                self.walk_node(value);
            }
            Node::Assignment { target, value, .. } => {
                self.walk_node(target);
                self.walk_node(value);
            }
            Node::ReturnStmt { value: Some(value) } | Node::YieldExpr { value: Some(value) } => {
                self.walk_node(value);
            }
            Node::EmitExpr { value } => self.walk_node(value),
            Node::ThrowStmt { value }
            | Node::TryOperator { operand: value }
            | Node::TryStar { operand: value }
            | Node::Spread(value) => self.walk_node(value),
            Node::UnaryOp { operand, .. } => self.walk_node(operand),
            Node::BinaryOp { left, right, .. } => {
                self.walk_node(left);
                self.walk_node(right);
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.walk_node(condition);
                self.walk_node(true_expr);
                self.walk_node(false_expr);
            }
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.walk_node(object);
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                self.walk_node(object);
                self.walk_node(index);
            }
            Node::SliceAccess { object, start, end } => {
                self.walk_node(object);
                if let Some(start) = start {
                    self.walk_node(start);
                }
                if let Some(end) = end {
                    self.walk_node(end);
                }
            }
            Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => self.walk_nodes(args),
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => self.walk_entries(entries),
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.walk_node(condition);
                self.walk_nodes(else_body);
            }
            Node::RequireStmt { condition, message } => {
                self.walk_node(condition);
                if let Some(message) = message {
                    self.walk_node(message);
                }
            }
            Node::RangeExpr { start, end, .. } => {
                self.walk_node(start);
                self.walk_node(end);
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.walk_node(&case.channel);
                    self.walk_nodes(&case.body);
                }
                if let Some((duration, body)) = timeout {
                    self.walk_node(duration);
                    self.walk_nodes(body);
                }
                if let Some(body) = default_body {
                    self.walk_nodes(body);
                }
            }
            Node::Parallel {
                expr,
                body,
                options,
                ..
            } => {
                self.walk_node(expr);
                for (_, value) in options {
                    self.walk_node(value);
                }
                self.walk_nodes(body);
            }
            Node::ImplBlock { methods, .. } => self.walk_nodes(methods),
            _ => {}
        }
    }

    fn walk_entries(&mut self, entries: &[DictEntry]) {
        for entry in entries {
            self.walk_node(&entry.key);
            self.walk_node(&entry.value);
        }
    }

    fn analyze_call(&self, node: &SNode, name: &str, args: &[SNode]) -> CostRow {
        let options = self.merged_options(args.get(2));
        let input_tokens = estimate_input_tokens(args, options.as_ref());
        let iterations = if name == "agent_loop" {
            estimate_agent_iterations(options.as_ref(), args.get(2).is_some())
        } else {
            Some(1)
        };
        let model = resolve_model(options.as_ref(), args.get(2).is_some());
        let total_tokens = input_tokens
            .zip(iterations)
            .map(|(tokens, cap)| tokens.saturating_mul(cap.max(0)));
        let (cost_usd, cost_cell) = match (&model, total_tokens) {
            (Some(model), Some(tokens)) => estimate_cost(model, tokens),
            _ => (None, CostCell::Unresolved),
        };
        CostRow {
            callsite: format!(
                "{}:{}:{} {}",
                self.path, node.span.line, node.span.column, name
            ),
            model: model.map(|model| model.display()),
            input_tokens: total_tokens,
            iterations,
            cost_usd,
            cost_cell,
        }
    }

    fn merged_options(&self, explicit: Option<&SNode>) -> Option<BTreeMap<String, StaticValue>> {
        let mut merged = BTreeMap::new();
        for frame in &self.inherited_options {
            merged.extend(frame.clone());
        }

        match explicit {
            None => (!merged.is_empty()).then_some(merged),
            Some(node) => match eval_static(node) {
                StaticValue::Dict(dict) => {
                    merged.extend(dict);
                    Some(merged)
                }
                StaticValue::Nil if !merged.is_empty() => Some(merged),
                StaticValue::Nil => Some(BTreeMap::new()),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedModel {
    provider: String,
    model: String,
}

impl ResolvedModel {
    fn display(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }
}

fn resolve_model(
    options: Option<&BTreeMap<String, StaticValue>>,
    explicit_options_present: bool,
) -> Option<ResolvedModel> {
    if explicit_options_present && options.is_none() {
        return None;
    }

    let provider = options.and_then(|options| options.get("provider"));
    let model = options.and_then(|options| options.get("model"));

    let provider = match provider {
        Some(StaticValue::String(value)) if !value.eq_ignore_ascii_case("auto") => {
            Some(value.clone())
        }
        Some(StaticValue::String(_)) | None => None,
        Some(_) => return None,
    };
    let model = match model {
        Some(StaticValue::String(value)) => Some(value.clone()),
        None => None,
        Some(_) => return None,
    };

    match (provider, model) {
        (Some(provider), Some(raw_model)) => {
            let (model, _) = harn_vm::llm_config::resolve_model(&raw_model);
            Some(ResolvedModel { provider, model })
        }
        (None, Some(raw_model)) => {
            let resolved = harn_vm::llm_config::resolve_model_info(&raw_model);
            Some(ResolvedModel {
                provider: resolved.provider,
                model: resolved.id,
            })
        }
        (Some(provider), None) => Some(ResolvedModel {
            model: harn_vm::llm_config::default_model_for_provider(&provider),
            provider,
        }),
        (None, None) => Some(resolve_default_model()),
    }
}

fn resolve_default_model() -> ResolvedModel {
    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        let provider = provider.trim().to_string();
        if !provider.is_empty() && !provider.eq_ignore_ascii_case("auto") {
            let model = std::env::var("HARN_LLM_MODEL")
                .ok()
                .map(|raw| harn_vm::llm_config::resolve_model(&raw).0)
                .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider(&provider));
            return ResolvedModel { provider, model };
        }
    }

    if let Ok(raw_model) = std::env::var("HARN_LLM_MODEL") {
        let resolved = harn_vm::llm_config::resolve_model_info(&raw_model);
        return ResolvedModel {
            provider: resolved.provider,
            model: resolved.id,
        };
    }

    let provider = harn_vm::llm_config::default_provider();
    let model = harn_vm::llm_config::default_model_for_provider(&provider);
    ResolvedModel { provider, model }
}

fn estimate_input_tokens(
    args: &[SNode],
    options: Option<&BTreeMap<String, StaticValue>>,
) -> Option<i64> {
    if let Some(messages) = options.and_then(|options| options.get("messages")) {
        return estimate_json_like_tokens(messages);
    }

    let prompt = match args.first() {
        Some(prompt) => static_display(prompt)?,
        None => String::new(),
    };
    let mut tokens = harn_vm::llm::estimate_text_tokens(&prompt);
    if let Some(system) = args.get(1) {
        if !matches!(eval_static(system), StaticValue::Nil) {
            let system = static_display(system)?;
            tokens = tokens.saturating_add(harn_vm::llm::estimate_text_tokens(&system));
        }
    }
    Some(tokens)
}

fn estimate_agent_iterations(
    options: Option<&BTreeMap<String, StaticValue>>,
    explicit_options_present: bool,
) -> Option<i64> {
    if explicit_options_present && options.is_none() {
        return None;
    }
    let Some(options) = options else {
        return Some(DEFAULT_AGENT_ITERATIONS);
    };

    let profile_default = match options.get("profile") {
        Some(StaticValue::String(profile)) => profile_default_iterations(profile),
        Some(_) => None,
        None => Some(DEFAULT_AGENT_ITERATIONS),
    };
    let legacy = match options.get("max_iterations") {
        Some(StaticValue::Int(value)) if *value >= 1 => Some(*value),
        Some(StaticValue::Int(_)) => Some(DEFAULT_AGENT_ITERATIONS),
        Some(_) => None,
        None => profile_default,
    };

    match options.get("iteration_budget") {
        None | Some(StaticValue::Nil) => legacy,
        Some(StaticValue::String(mode)) if mode == "adaptive" => Some(16),
        Some(StaticValue::String(mode)) if mode == "fixed" => legacy,
        Some(StaticValue::Dict(budget)) => {
            if let Some(StaticValue::Int(value)) = budget.get("max") {
                return Some((*value).max(1));
            }
            let mode = match budget.get("mode") {
                Some(StaticValue::String(mode)) => mode.as_str(),
                None => "fixed",
                Some(_) => return None,
            };
            match mode {
                "adaptive" => Some(16),
                "fixed" => legacy,
                _ => None,
            }
        }
        Some(_) => None,
    }
}

fn profile_default_iterations(profile: &str) -> Option<i64> {
    match profile {
        "tool_using" => Some(50),
        "researcher" => Some(30),
        "verifier" => Some(5),
        "completer" => Some(1),
        _ => None,
    }
}

fn estimate_cost(model: &ResolvedModel, input_tokens: i64) -> (Option<f64>, CostCell) {
    match harn_vm::llm::llm_pricing_per_1k(&model.provider, &model.model) {
        Some((input_rate, _output_rate)) => (
            Some(input_tokens.max(0) as f64 * input_rate / 1000.0),
            CostCell::Amount,
        ),
        None => (None, CostCell::Unpriced),
    }
}

fn estimate_json_like_tokens(value: &StaticValue) -> Option<i64> {
    match value {
        StaticValue::Nil | StaticValue::Bool(_) | StaticValue::Int(_) | StaticValue::Float(_) => {
            Some(1)
        }
        StaticValue::String(value) => Some(harn_vm::llm::estimate_text_tokens(value)),
        StaticValue::List(items) => items.iter().try_fold(0_i64, |total, item| {
            estimate_json_like_tokens(item).map(|tokens| total.saturating_add(tokens))
        }),
        StaticValue::Dict(fields) => fields.iter().try_fold(0_i64, |total, (key, value)| {
            let key_tokens = harn_vm::llm::estimate_text_tokens(key);
            estimate_json_like_tokens(value).map(|value_tokens| {
                total
                    .saturating_add(key_tokens)
                    .saturating_add(value_tokens)
            })
        }),
        StaticValue::Unknown => None,
    }
}

fn eval_static(node: &SNode) -> StaticValue {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            StaticValue::String(value.clone())
        }
        Node::InterpolatedString(segments) => {
            let mut value = String::new();
            for segment in segments {
                match segment {
                    harn_lexer::StringSegment::Literal(literal) => value.push_str(literal),
                    harn_lexer::StringSegment::Expression(_, _, _) => return StaticValue::Unknown,
                }
            }
            StaticValue::String(value)
        }
        Node::IntLiteral(value) => StaticValue::Int(*value),
        Node::FloatLiteral(value) => StaticValue::Float(*value),
        Node::BoolLiteral(value) => StaticValue::Bool(*value),
        Node::NilLiteral => StaticValue::Nil,
        Node::ListLiteral(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(eval_static(item));
            }
            StaticValue::List(values)
        }
        Node::DictLiteral(entries) => {
            let mut values = BTreeMap::new();
            for entry in entries {
                let Some(key) = dict_key(&entry.key) else {
                    return StaticValue::Unknown;
                };
                values.insert(key, eval_static(&entry.value));
            }
            StaticValue::Dict(values)
        }
        Node::PropertyAccess { object, property } => match eval_static(object) {
            StaticValue::Dict(fields) => fields.get(property).cloned().unwrap_or(StaticValue::Nil),
            _ => StaticValue::Unknown,
        },
        Node::OptionalPropertyAccess { object, property } => match eval_static(object) {
            StaticValue::Nil => StaticValue::Nil,
            StaticValue::Dict(fields) => fields.get(property).cloned().unwrap_or(StaticValue::Nil),
            _ => StaticValue::Unknown,
        },
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => eval_static_subscript(
            object,
            index,
            matches!(&node.node, Node::OptionalSubscriptAccess { .. }),
        ),
        Node::BinaryOp { op, left, right } if op == "+" => {
            match (eval_static(left), eval_static(right)) {
                (StaticValue::String(left), StaticValue::String(right)) => {
                    StaticValue::String(format!("{left}{right}"))
                }
                _ => StaticValue::Unknown,
            }
        }
        Node::BinaryOp { op, left, right } if op == "??" => match eval_static(left) {
            StaticValue::Nil => eval_static(right),
            StaticValue::Unknown => StaticValue::Unknown,
            value => value,
        },
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => match eval_static(condition) {
            StaticValue::Bool(true) => eval_static(true_expr),
            StaticValue::Bool(false) => eval_static(false_expr),
            _ => StaticValue::Unknown,
        },
        Node::UnaryOp { op, operand } if op == "-" => match eval_static(operand) {
            StaticValue::Int(value) => StaticValue::Int(value.saturating_neg()),
            StaticValue::Float(value) => StaticValue::Float(-value),
            _ => StaticValue::Unknown,
        },
        Node::Block(body) if body.len() == 1 => eval_static(&body[0]),
        _ => StaticValue::Unknown,
    }
}

fn eval_static_subscript(object: &SNode, index: &SNode, optional: bool) -> StaticValue {
    match (eval_static(object), eval_static(index)) {
        (StaticValue::Nil, _) if optional => StaticValue::Nil,
        (StaticValue::List(items), StaticValue::Int(index)) if index >= 0 => items
            .get(index as usize)
            .cloned()
            .unwrap_or(StaticValue::Nil),
        (StaticValue::Dict(fields), StaticValue::String(key)) => {
            fields.get(&key).cloned().unwrap_or(StaticValue::Nil)
        }
        _ => StaticValue::Unknown,
    }
}

fn dict_key(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) | Node::StringLiteral(name) | Node::RawStringLiteral(name) => {
            Some(name.clone())
        }
        _ => static_display(node),
    }
}

fn static_display(node: &SNode) -> Option<String> {
    static_display_value(&eval_static(node))
}

fn static_display_value(value: &StaticValue) -> Option<String> {
    match value {
        StaticValue::String(value) => Some(value.clone()),
        StaticValue::Int(value) => Some(value.to_string()),
        StaticValue::Float(value) => Some(value.to_string()),
        StaticValue::Bool(value) => Some(value.to_string()),
        StaticValue::Nil => Some(String::new()),
        StaticValue::Unknown | StaticValue::List(_) | StaticValue::Dict(_) => None,
    }
}

fn render_report(rows: &[CostRow]) -> String {
    if rows.is_empty() {
        return "No llm_call or agent_loop callsites found.\n".to_string();
    }

    let mut table = Vec::with_capacity(rows.len() + 1);
    table.push([
        "Callsite".to_string(),
        "Model".to_string(),
        "Est tokens".to_string(),
        "Est cost".to_string(),
        "Iterations cap".to_string(),
    ]);
    for row in rows {
        table.push([
            row.callsite.clone(),
            row.model
                .clone()
                .unwrap_or_else(|| "unresolved".to_string()),
            row.input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unresolved".to_string()),
            render_cost_cell(row),
            row.iterations
                .map(|iterations| iterations.to_string())
                .unwrap_or_else(|| "unresolved".to_string()),
        ]);
    }

    let mut widths = [0_usize; 5];
    for row in &table {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "LLM cost estimate (static, no execution)");
    for (idx, row) in table.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:<w0$}  {:<w1$}  {:>w2$}  {:>w3$}  {:>w4$}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
        );
        if idx == 0 {
            let _ = writeln!(
                out,
                "{:-<w0$}  {:-<w1$}  {:-<w2$}  {:-<w3$}  {:-<w4$}",
                "",
                "",
                "",
                "",
                "",
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
                w4 = widths[4],
            );
        }
    }

    let known_tokens: i64 = rows.iter().filter_map(|row| row.input_tokens).sum();
    let known_cost: f64 = rows.iter().filter_map(|row| row.cost_usd).sum();
    let unresolved_tokens = rows.iter().any(|row| row.input_tokens.is_none());
    let unresolved_cost = rows
        .iter()
        .any(|row| !matches!(row.cost_cell, CostCell::Amount));
    let _ = write!(out, "\nTotal est tokens: {known_tokens}");
    if unresolved_tokens {
        out.push_str(" (excludes unresolved rows)");
    }
    let _ = write!(out, "\nTotal est cost: {}", format_usd(known_cost));
    if unresolved_cost {
        out.push_str(" (excludes unresolved or unpriced rows)");
    }
    out.push('\n');
    out.push_str(
        "Note: token estimates include statically known prompt, system, or messages input text only.\n",
    );
    out
}

fn render_cost_cell(row: &CostRow) -> String {
    match row.cost_cell {
        CostCell::Amount => row
            .cost_usd
            .map(format_usd)
            .unwrap_or_else(|| "unresolved".to_string()),
        CostCell::Unpriced => "unpriced".to_string(),
        CostCell::Unresolved => "unresolved".to_string(),
    }
}

fn format_usd(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    format!("${value:.6}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> String {
        let program = harn_parser::parse_source(source).expect("source parses");
        render_explain_cost("main.harn", &program)
    }

    #[test]
    fn renders_llm_call_and_agent_loop_estimates() {
        let out = render(
            r#"
pipeline main() {
  llm_call("hello world", "sys", {model: "gpt-4o"})
  agent_loop("do it", "sys", {provider: "ollama", model: "llama3.2", max_iterations: 3})
}
"#,
        );

        assert!(out.contains("main.harn:3:3 llm_call"));
        assert!(out.contains("openai:gpt-4o"));
        assert!(out.contains("main.harn:4:3 agent_loop"));
        let row: Vec<_> = out
            .lines()
            .find(|line| line.contains("main.harn:4:3 agent_loop"))
            .expect("agent_loop row exists")
            .split_whitespace()
            .collect();
        assert_eq!(
            row,
            [
                "main.harn:4:3",
                "agent_loop",
                "ollama:llama3.2",
                "9",
                "$0.000000",
                "3"
            ]
        );
    }

    #[test]
    fn reports_dynamic_model_as_unresolved() {
        let out = render(
            r#"
pipeline main() {
  const opts = {model: "gpt-4o"}
  llm_call("hello", nil, {model: opts?.model ?? "claude-sonnet-4-20250514"})
}
"#,
        );

        assert!(out.contains("main.harn:4:3 llm_call"));
        assert!(out.contains("unresolved"));
    }

    #[test]
    fn inherits_literal_cost_route_options() {
        let out = render(
            r#"
pipeline main() {
  cost_route {
    provider: "ollama",
    model: "llama3.2",
    llm_call("hello", nil)
  }
}
"#,
        );

        assert!(out.contains("ollama:llama3.2"));
        assert!(out.contains("$0.000000"));
    }

    #[test]
    fn walks_control_flow_expression_slots() {
        let out = render(
            r#"
pipeline main() {
  while llm_call("condition", nil, {model: "gpt-4o"}) {
    retry (llm_call("count", nil, {model: "gpt-4o"})) {
      const values = parallel each [llm_call("source", nil, {model: "gpt-4o"})] with {
        max_concurrent: llm_call("cap", nil, {model: "gpt-4o"})
      } { item ->
        item
      }
    }
  }
}
"#,
        );

        assert_eq!(
            out.lines()
                .filter(|line| line.contains(" llm_call"))
                .count(),
            4
        );
    }

    #[test]
    fn reports_no_calls() {
        let out = render("pipeline main() { __io_println(\"hi\") }");
        assert_eq!(out, "No llm_call or agent_loop callsites found.\n");
    }
}
