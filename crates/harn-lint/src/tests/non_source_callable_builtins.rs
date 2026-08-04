//! HARN-LNT-072 — a builtin the VM registers whose declared exposure keeps
//! Harn source from naming it.
//!
//! The undefined-function rule cannot reach these: it treats every registered
//! builtin as defined, so a privileged wire draws no diagnostic at all even
//! though the typechecker rejects the call.

use super::*;

/// `lint_source` unwraps the parse. The registry sweep below feeds it every
/// registered builtin name, some of which the parser will not accept in call
/// position, so it needs a fallible variant.
fn parse_and_lint(source: &str) -> Option<Vec<LintDiagnostic>> {
    let tokens = Lexer::new(source).tokenize().ok()?;
    let program = Parser::new(tokens).parse().ok()?;
    Some(lint_with_source(&program, source))
}

#[test]
fn privileged_wire_call_reports_and_names_the_replacement_route() {
    let source = "fn main(harness: Harness) {\n  let out = host_call(\"ast.outline\", {path: \"a.rs\"})\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "non-source-callable-builtin"),
        1,
        "expected one non-source-callable lint for host_call, got: {diags:?}"
    );
    let diagnostic = diags
        .iter()
        .find(|d| d.rule == "non-source-callable-builtin")
        .expect("host_call lint");
    assert!(
        diagnostic.message.contains("privileged embedder wire"),
        "message should name the exposure, got: {}",
        diagnostic.message
    );
    let suggestion = diagnostic.suggestion.as_deref().unwrap_or_default();
    assert!(
        suggestion.contains("register_callable_host_operation"),
        "suggestion should name the host-side successor, got: {suggestion}"
    );
}

#[test]
fn a_name_with_a_migration_recipe_keeps_reporting_the_repairable_rule() {
    // `log_info` moved onto `harness.obs`, so it must stay on HARN-LNT-071
    // where `harn fix` can rewrite it, not fall through to this rule.
    let source = "fn main(harness: Harness) {\n  log_info(\"hello\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        1,
        "expected the repairable rule, got: {diags:?}"
    );
    assert_eq!(count_rule(&diags, "non-source-callable-builtin"), 0);
}

#[test]
fn a_local_definition_of_the_same_name_is_not_reported() {
    let source = "fn host_call(name: string) -> string {\n  name\n}\n\nfn main(harness: Harness) {\n  let out = host_call(\"ast.outline\")\n  harness.stdio.println(out)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "non-source-callable-builtin"),
        0,
        "a local definition owns the name, got: {diags:?}"
    );
}

#[test]
fn a_pure_global_is_not_reported() {
    let source =
        "fn main(harness: Harness) {\n  harness.stdio.println(json_stringify({a: 1}))\n}\n";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "non-source-callable-builtin"), 0);
}

/// No registered builtin may be callable in source without drawing something.
///
/// This is the invariant `host_call` broke. The linter seeds its known-name set
/// from the VM registry, so any builtin the typechecker rejects is invisible to
/// it unless a rule claims the name — a migration recipe (HARN-LNT-071 and the
/// clock/stdio/fs/env/random/net families) or, failing that, this rule. A
/// silent name is the worst outcome for a downstream repo: the build fails and
/// nothing says why.
///
/// `__`-prefixed and `hostlib_`-prefixed names are excluded because the
/// undefined-function rule deliberately skips those prefixes; they are not part
/// of the surface a script is expected to spell.
#[test]
fn every_non_source_callable_builtin_draws_a_diagnostic() {
    let silent: Vec<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .filter(|name| !name.starts_with("__") && !name.starts_with("hostlib_"))
        // Namespaced registrations (`workflow.pause`, `event_log.emit`) are not
        // bare identifiers, so no call expression can name one.
        .filter(|name| !name.contains('.'))
        .filter(|name| !harn_parser::builtin_signatures::is_language_intrinsic(name))
        .filter(|name| {
            harn_vm::stdlib::builtin_exposure(name)
                .is_some_and(|exposure| !harn_vm::stdlib::exposure_is_source_nameable(exposure))
        })
        .filter(|name| {
            // Unused-binding lints always fire on this fixture, so "drew a
            // diagnostic" is not the question — "drew one that claims this
            // name" is. A name the parser will not accept in call position
            // (`import`, `return`, …) is not callable in the first place, so
            // an unparseable fixture is a skip rather than a finding.
            let source = format!("fn main(harness: Harness) {{\n  {name}()\n}}\n");
            let needle = format!("`{name}`");
            parse_and_lint(&source).is_some_and(|diagnostics| {
                !diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(&needle))
            })
        })
        .collect();

    assert!(
        silent.is_empty(),
        "these builtins are not source-callable but draw no lint at all: {silent:?}"
    );
}
