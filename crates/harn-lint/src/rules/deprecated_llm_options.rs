//! `deprecated_llm_options` rule: hard-error on removed option keys passed
//! in dict-literal options to `llm_call`-family surfaces.
//!
//! Two sources feed the removed-key set:
//! * the canonical option registry's removal table
//!   (`harn_builtin_meta::llm_options::LLM_REMOVED_OPTIONS`) — every synonym
//!   the W2 options re-cut killed, each carrying its replacement; and
//! * the legacy `llm_retries` / `llm_backoff_ms` pair (removed in v0.10,
//!   pre-registry) with its bespoke off-by-one migration note
//!   (`llm_retries: K` retried K times after the first attempt ⇒
//!   `with_retry(..., {max_attempts: K + 1})`).
//!
//! The runtime rejects these keys too (the extractor's unknown-key gate);
//! this rule surfaces them at `harn check` time for dict-literal call sites.

use harn_lexer::Span;
use harn_parser::{DiagnosticCode as Code, DictEntry, MatchArm, Node, SNode, SelectCase};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "deprecated_llm_options";

const LEGACY_RETRY_KEYS: &[&str] = &["llm_retries", "llm_backoff_ms"];

fn options_arg_index(name: &str) -> Option<usize> {
    match name {
        "llm_completion" => Some(3),
        "llm_call"
        | "llm_call_safe"
        | "llm_call_structured"
        | "llm_call_structured_safe"
        | "llm_call_structured_result"
        | "llm_stream"
        | "llm_stream_call"
        | "agent_loop" => Some(2),
        _ => None,
    }
}

/// Walk the program looking for calls to LLM surfaces whose dict-literal
/// options argument contains removed keys. Emits one Error per offending
/// key occurrence, anchored to the key's span.
pub(crate) fn check_deprecated_llm_options(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    for node in program {
        visit_node(node, diagnostics);
    }
}

fn visit_node(node: &SNode, diagnostics: &mut Vec<LintDiagnostic>) {
    match &node.node {
        Node::FunctionCall { name, args, .. } => {
            if let Some(index) = options_arg_index(name) {
                if let Some(SNode {
                    node: Node::DictLiteral(entries),
                    ..
                }) = args.get(index)
                {
                    scan_entries(entries, diagnostics);
                }
            }
            for arg in args {
                visit_node(arg, diagnostics);
            }
        }
        Node::AttributedDecl { attributes, inner } => {
            for attr in attributes {
                for arg in &attr.args {
                    visit_node(&arg.value, diagnostics);
                }
            }
            visit_node(inner, diagnostics);
        }
        Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
            visit_nodes(body, diagnostics);
        }
        Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
            visit_node(value, diagnostics);
        }
        Node::ImplBlock { methods, .. } => visit_nodes(methods, diagnostics),
        Node::IfElse {
            condition,
            then_body,
            else_body,
            ..
        } => {
            visit_node(condition, diagnostics);
            visit_nodes(then_body, diagnostics);
            if let Some(body) = else_body {
                visit_nodes(body, diagnostics);
            }
        }
        Node::ForIn { iterable, body, .. } => {
            visit_node(iterable, diagnostics);
            visit_nodes(body, diagnostics);
        }
        Node::WhileLoop { condition, body } => {
            visit_node(condition, diagnostics);
            visit_nodes(body, diagnostics);
        }
        Node::Retry { count, body } => {
            visit_node(count, diagnostics);
            visit_nodes(body, diagnostics);
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                visit_node(value, diagnostics);
            }
            visit_nodes(body, diagnostics);
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            visit_nodes(body, diagnostics);
            visit_nodes(catch_body, diagnostics);
            if let Some(body) = finally_body {
                visit_nodes(body, diagnostics);
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::ScopeBlock { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body, .. }
        | Node::Block(body)
        | Node::Closure { body, .. } => visit_nodes(body, diagnostics),
        Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } => visit_nodes(body, diagnostics),
        Node::SkillDecl { fields, .. } => {
            for (_, value) in fields {
                visit_node(value, diagnostics);
            }
        }
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            for (_, value) in fields {
                visit_node(value, diagnostics);
            }
            visit_nodes(body, diagnostics);
            if let Some(body) = summarize {
                visit_nodes(body, diagnostics);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            visit_node(start, diagnostics);
            visit_node(end, diagnostics);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            visit_node(condition, diagnostics);
            visit_nodes(else_body, diagnostics);
        }
        Node::RequireStmt { condition, message } => {
            visit_node(condition, diagnostics);
            if let Some(message) = message {
                visit_node(message, diagnostics);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            visit_node(duration, diagnostics);
            visit_nodes(body, diagnostics);
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                visit_node(value, diagnostics);
            }
        }
        Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::NonNullAssert { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => visit_node(value, diagnostics),
        Node::HitlExpr { args, .. } => {
            for arg in args {
                visit_node(&arg.value, diagnostics);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            visit_node(expr, diagnostics);
            for (_, value) in options {
                visit_node(value, diagnostics);
            }
            visit_nodes(body, diagnostics);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                visit_select_case(case, diagnostics);
            }
            if let Some((duration, body)) = timeout {
                visit_node(duration, diagnostics);
                visit_nodes(body, diagnostics);
            }
            if let Some(body) = default_body {
                visit_nodes(body, diagnostics);
            }
        }
        Node::EnumConstruct { args, .. } => visit_nodes(args, diagnostics),
        Node::ValueCall { callee, args } => {
            visit_node(callee, diagnostics);
            visit_nodes(args, diagnostics);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            visit_node(object, diagnostics);
            visit_nodes(args, diagnostics);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            visit_node(object, diagnostics);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            visit_node(object, diagnostics);
            visit_node(index, diagnostics);
        }
        Node::SliceAccess { object, start, end } => {
            visit_node(object, diagnostics);
            if let Some(start) = start {
                visit_node(start, diagnostics);
            }
            if let Some(end) = end {
                visit_node(end, diagnostics);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            visit_node(left, diagnostics);
            visit_node(right, diagnostics);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            visit_node(condition, diagnostics);
            visit_node(true_expr, diagnostics);
            visit_node(false_expr, diagnostics);
        }
        Node::Assignment { target, value, .. } => {
            visit_node(target, diagnostics);
            visit_node(value, diagnostics);
        }
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            visit_dict_entries(fields, diagnostics);
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => visit_nodes(items, diagnostics),
        Node::MatchExpr { value, arms } => {
            visit_node(value, diagnostics);
            for arm in arms {
                visit_match_arm(arm, diagnostics);
            }
        }
        // Leaf / no-child nodes.
        Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::ImportDecl { .. }
        | Node::NamespaceImport { .. }
        | Node::SelectiveImport { .. }
        | Node::TypeDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt
        | Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_) => {}
    }
}

fn visit_nodes(nodes: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    for node in nodes {
        visit_node(node, diagnostics);
    }
}

fn visit_dict_entries(entries: &[DictEntry], diagnostics: &mut Vec<LintDiagnostic>) {
    for entry in entries {
        visit_node(&entry.key, diagnostics);
        visit_node(&entry.value, diagnostics);
    }
}

fn visit_match_arm(arm: &MatchArm, diagnostics: &mut Vec<LintDiagnostic>) {
    visit_node(&arm.pattern, diagnostics);
    if let Some(guard) = &arm.guard {
        visit_node(guard, diagnostics);
    }
    visit_nodes(&arm.body, diagnostics);
}

fn visit_select_case(case: &SelectCase, diagnostics: &mut Vec<LintDiagnostic>) {
    visit_node(&case.channel, diagnostics);
    visit_nodes(&case.body, diagnostics);
}

/// Scan an LLM call's dict-literal options and emit an Error for every key
/// matching a removed name. Also recurses into
/// each value so a nested `{opts: {llm_retries: ...}}` is still flagged
/// in the unlikely case it appears at this level.
fn scan_entries(entries: &[DictEntry], diagnostics: &mut Vec<LintDiagnostic>) {
    for entry in entries {
        if let Node::StringLiteral(name) = &entry.key.node {
            if LEGACY_RETRY_KEYS.contains(&name.as_str()) {
                diagnostics.push(make_diagnostic(name, entry.key.span));
            } else if let Some(removed) = harn_builtin_meta::llm_options::removed_llm_option(name) {
                diagnostics.push(make_registry_diagnostic(name, removed.fix, entry.key.span));
            }
        }
        // Walk the value so nested dict literals or other call sites
        // inside the value still get linted normally.
        visit_node(&entry.value, diagnostics);
    }
}

fn make_registry_diagnostic(key: &str, fix: &str, span: Span) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::LintDeprecatedLlmOptions,
        rule: RULE_NAME.into(),
        message: format!("option `{key}` was removed — {fix}"),
        span,
        severity: LintSeverity::Error,
        suggestion: Some(fix.to_string()),
        fix: None,
    }
}

fn make_diagnostic(key: &str, span: Span) -> LintDiagnostic {
    let message = format!(
        "`{key}` was removed in v0.10 and is no longer read; use `with_retry(default_llm_caller(), {{...}})` from `std/llm/handlers`. Note the off-by-one: `llm_retries: K` retried K times after the first attempt, so pass `with_retry(..., {{max_attempts: K + 1}})`. See docs/src/migrations/v0.10.md."
    );
    let suggestion = Some(format!(
        "remove `{key}` from this options dict and wrap the call with `with_retry(default_llm_caller(), {{max_attempts: K + 1}})` from `std/llm/handlers` (K = the old `llm_retries` value)."
    ));
    LintDiagnostic {
        code: Code::LintDeprecatedLlmOptions,
        rule: RULE_NAME.into(),
        message,
        span,
        severity: LintSeverity::Error,
        suggestion,
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diags = Vec::new();
        check_deprecated_llm_options(&program, &mut diags);
        diags
    }

    fn count_rule(diags: &[LintDiagnostic]) -> usize {
        diags.iter().filter(|d| d.rule == RULE_NAME).count()
    }

    fn message_for(diags: &[LintDiagnostic], idx: usize) -> &str {
        diags
            .iter()
            .filter(|d| d.rule == RULE_NAME)
            .nth(idx)
            .expect("diagnostic at idx")
            .message
            .as_str()
    }

    #[test]
    fn triggers_on_llm_call_with_llm_retries() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        assert!(
            message_for(&diags, 0).contains("`llm_retries` was removed in v0.10"),
            "msg: {}",
            message_for(&diags, 0)
        );
        assert!(
            message_for(&diags, 0).contains("max_attempts: K + 1"),
            "message must carry the off-by-one migration hint: {}",
            message_for(&diags, 0)
        );
    }

    #[test]
    fn triggers_on_llm_call_with_llm_backoff_ms() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call("hi", nil, {llm_backoff_ms: 250})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        assert!(
            message_for(&diags, 0).contains("`llm_backoff_ms` was removed in v0.10"),
            "msg: {}",
            message_for(&diags, 0)
        );
    }

    #[test]
    fn triggers_on_both_keys_in_one_call() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call("hi", nil, {llm_retries: 3, llm_backoff_ms: 250})
}
"#,
        );
        assert_eq!(count_rule(&diags), 2, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_llm_call_safe() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call_safe("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_llm_call_structured() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call_structured("hi", {schema: "x"}, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_llm_call_structured_result() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call_structured_result("hi", {schema: "x"}, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_all_remaining_option_surfaces() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_completion("hi", nil, nil, {llm_retries: 3})
    llm_call_structured_safe("hi", {type: "string"}, {llm_retries: 3})
    llm_stream("hi", nil, {llm_retries: 3})
    llm_stream_call("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 4, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_structured_schema_argument() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call_structured("hi", {schema: "x"})
    llm_call_structured_result("hi", {schema: "x"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_agent_loop() {
        let diags = lint(
            r#"
pipeline default(task) {
    agent_loop("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_unrelated_callee() {
        let diags = lint(
            r#"
pipeline default(task) {
    foo("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_non_literal_opts() {
        let diags = lint(
            r#"
pipeline default(task) {
    const opts = {llm_retries: 3}
    llm_call("hi", nil, opts)
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_safe_keys() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call("hi", nil, {temperature: 0.5})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn severity_is_hard_error() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call("hi", nil, {llm_retries: 3})
}
"#,
        );
        let our_diag = diags
            .iter()
            .find(|d| d.rule == RULE_NAME)
            .expect("diagnostic present");
        assert_eq!(our_diag.severity, LintSeverity::Error);
    }
}
