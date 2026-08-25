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
//!
//! A call site names its surface in either spelling — `llm_call(...)` or the
//! typed `harness.llm.call(...)` that replaced it — so the surface is
//! recognized through [`HarnessFacts`] rather than by matching call syntax.
//! A key removed from the options dict is removed either way, and a rule that
//! only sees the ambient spelling goes silent as call sites migrate rather
//! than reporting anything (harn#7280).

use harn_lexer::Span;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::linter::harness_facts::HarnessFacts;

const RULE_NAME: &str = "deprecated_llm_options";

const LEGACY_RETRY_KEYS: &[&str] = &["llm_retries", "llm_backoff_ms"];

/// The LLM surfaces that take an options dict, and where in the argument list
/// it sits.
///
/// Written out rather than derived from the builtin signatures, because the
/// set is not all builtins: `agent_loop` is a Harn function from
/// `std/agent/loop` with no `BuiltinSignature` to read a parameter list from.
/// Deriving the index would drop it — the same silent narrowing this rule is
/// being fixed for.
const OPTION_SURFACES: &[(&str, usize)] = &[
    ("llm_completion", 3),
    ("llm_call", 2),
    ("llm_call_safe", 2),
    ("llm_call_structured", 2),
    ("llm_call_structured_safe", 2),
    ("llm_call_structured_result", 2),
    ("llm_stream", 2),
    ("llm_stream_call", 2),
    ("agent_loop", 2),
];

/// Walk the program looking for calls to LLM surfaces whose dict-literal
/// options argument contains removed keys. Emits one Error per offending
/// key occurrence, anchored to the key's span.
pub(crate) fn check_deprecated_llm_options(
    program: &[SNode],
    harness: &HarnessFacts,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    harn_parser::visit::walk_program(program, &mut |node| {
        if let Some(entries) = options_entries(node, harness) {
            scan_entries(entries, diagnostics);
        }
    });
}

/// The dict-literal options argument of `node`, when `node` calls one of the
/// [`OPTION_SURFACES`] in either spelling and passes its options inline.
fn options_entries<'node>(
    node: &'node SNode,
    harness: &HarnessFacts,
) -> Option<&'node [DictEntry]> {
    // Resolving the receiver once rejects the ordinary method call —
    // `items.map(...)`, `client.send(...)` — before the scan below asks the
    // migration recipe about each surface name in turn.
    if matches!(
        &node.node,
        Node::MethodCall { .. } | Node::OptionalMethodCall { .. }
    ) && !harness.is_capability_method_call(node)
    {
        return None;
    }
    let (args, index) = OPTION_SURFACES.iter().find_map(|(name, index)| {
        harness
            .call_names_builtin(node, name)
            .map(|args| (args, *index))
    })?;
    match args.get(index) {
        Some(SNode {
            node: Node::DictLiteral(entries),
            ..
        }) => Some(entries.as_slice()),
        _ => None,
    }
}

/// Scan an LLM call's dict-literal options and emit an Error for every key
/// matching a removed name.
///
/// Only this dict's own keys: a nested `{opts: {llm_retries: ...}}` is not an
/// options bag, and a call appearing inside a value is reached by the walk in
/// its own right.
fn scan_entries(entries: &[DictEntry], diagnostics: &mut Vec<LintDiagnostic>) {
    for entry in entries {
        let Node::StringLiteral(name) = &entry.key.node else {
            continue;
        };
        if LEGACY_RETRY_KEYS.contains(&name.as_str()) {
            diagnostics.push(make_diagnostic(name, entry.key.span));
        } else if let Some(removed) = harn_builtin_meta::llm_options::removed_llm_option(name) {
            diagnostics.push(make_registry_diagnostic(name, removed.fix, entry.key.span));
        }
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
        let harness = HarnessFacts::collect(&program);
        let mut diags = Vec::new();
        check_deprecated_llm_options(&program, &harness, &mut diags);
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

    // The migrated spelling. Each of these is the same call the ambient test
    // above makes, written the way `HARN-LNT-071` asks for it.

    #[test]
    fn triggers_on_migrated_llm_call() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    harness.llm.call("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        assert!(
            message_for(&diags, 0).contains("`llm_retries` was removed in v0.10"),
            "msg: {}",
            message_for(&diags, 0)
        );
    }

    #[test]
    fn triggers_on_migrated_registry_removal() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    harness.llm.call("hi", nil, {json_schema: "x"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        assert!(
            message_for(&diags, 0).contains("option `json_schema` was removed"),
            "msg: {}",
            message_for(&diags, 0)
        );
    }

    /// `llm_completion` keeps its options at index 3, not 2. The migration
    /// forwards arguments unchanged, so the migrated call must read the same
    /// position — reading index 2 would silently scan the system prompt.
    #[test]
    fn migrated_completion_reads_its_own_options_index() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    harness.llm.completion("hi", nil, nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_migrated_call_safe_and_structured() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    harness.llm.call_safe("hi", nil, {llm_retries: 3})
    harness.llm.call_structured("hi", {schema: "x"}, {llm_retries: 3})
    harness.llm.call_structured_safe("hi", {type: "string"}, {llm_retries: 3})
    harness.llm.call_structured_result("hi", {schema: "x"}, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 4, "diags: {diags:?}");
    }

    /// `llm_stream` reaches the manifest as an alias of `__cap_llm_stream`, so
    /// this also pins that an aliased ambient name resolves to its harness
    /// method rather than falling through to the ambient-only path.
    #[test]
    fn triggers_on_migrated_stream() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    harness.llm.stream("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    /// The falsifier for the whole approach: a same-named method on a receiver
    /// that is not the host handle must stay silent. Without this, teaching
    /// the rule the migrated spelling would turn every `x.llm.call(...)` into
    /// a false positive.
    #[test]
    fn does_not_trigger_on_non_harness_receiver() {
        let diags = lint(
            r#"
pipeline p(proxy: LlmProxy) {
    proxy.llm.call("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    /// A local that merely spells its name `harness` carries no host
    /// authority, so its `llm.call` is not the migrated surface.
    #[test]
    fn does_not_trigger_on_local_named_harness() {
        let diags = lint(
            r#"
pipeline p(task) {
    const harness = make_proxy()
    harness.llm.call("hi", nil, {llm_retries: 3})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    /// The walk reaches call sites nested anywhere the AST allows, not just
    /// statements directly in a pipeline body.
    #[test]
    fn reaches_nested_call_sites() {
        let diags = lint(
            r#"
pipeline p(harness: Harness) {
    const run = fn(kind: string) {
        return match kind {
            "a" -> { harness.llm.call("hi", nil, {llm_retries: 3}) }
            _ -> { llm_call("hi", nil, {llm_backoff_ms: 250}) }
        }
    }
    try {
        run("a")
    } catch (e) {
        llm_call("hi", nil, {llm_retries: 1})
    }
}
"#,
        );
        assert_eq!(count_rule(&diags), 3, "diags: {diags:?}");
    }
}
