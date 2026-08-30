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

/// Lint `source` as a privileged artifact, the way
/// `harn lint --trusted-host-dispatch` does.
fn lint_trusted(source: &str) -> Vec<LintDiagnostic> {
    let tokens = Lexer::new(source).tokenize().expect("tokenize");
    let program = Parser::new(tokens).parse().expect("parse");
    crate::lint_full(
        &program,
        &[],
        Some(source),
        &std::collections::HashSet::new(),
        &crate::LintOptions {
            trusted_host_dispatch: true,
            ..Default::default()
        },
        None,
    )
}

/// The `non-source-callable-builtin` suggestion for `source`, or a panic
/// naming what came out instead.
fn wire_route(source: &str) -> String {
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "non-source-callable-builtin"),
        1,
        "expected exactly one non-source-callable lint, got: {diags:?}"
    );
    let diagnostic = diags
        .iter()
        .find(|d| d.rule == "non-source-callable-builtin")
        .expect("non-source-callable lint");
    assert!(
        diagnostic.message.contains("privileged embedder wire"),
        "message should name the exposure, got: {}",
        diagnostic.message
    );
    diagnostic.suggestion.clone().unwrap_or_default()
}

#[test]
fn privileged_wire_with_a_declared_operation_names_the_destination_method() {
    // The point of the rule. A wire carries its destination as a string, so
    // the call is opaque to every name-keyed check — but the literal can be
    // read back through the declared contract, which turns "you cannot call
    // this" into "call this instead".
    let route = wire_route(
        "fn main(harness: Harness) {\n  let out = host_call(\"ast.outline\", {path: \"a.rs\"})\n}\n",
    );
    assert!(
        route.contains("harness.ast.outline"),
        "suggestion should name the declared method, got: {route}"
    );
}

#[test]
fn privileged_wire_route_normalizes_the_namespace_spelling() {
    let route = wire_route(
        "fn main(harness: Harness) {\n  let out = host_call(\"prmonitor.run_commands\", {})\n}\n",
    );
    assert!(
        route.contains("harness.pr_monitor.run_commands"),
        "suggestion should name the declared capability field, got: {route}"
    );
}

#[test]
fn privileged_wire_route_uses_the_scripts_own_harness_binding() {
    // `callable_harness_param` recognizes `harness` and `_harness` only, so
    // this is the whole range the route can vary over — but it does vary,
    // rather than printing a fixed `harness.` prefix the script cannot use.
    let route = wire_route(
        "fn main(_harness: Harness) {\n  let out = host_call(\"ast.outline\", {path: \"a.rs\"})\n}\n",
    );
    assert!(
        route.contains("_harness.ast.outline"),
        "suggestion should use the binding the script declared, got: {route}"
    );
}

#[test]
fn undeclared_operation_points_at_the_host_registration_seam() {
    // No capability owns this one, so the honest answer is the embedder's
    // callable root rather than a guessed harness method.
    let route = wire_route(
        "fn main(harness: Harness) {\n  let out = host_call(\"acme.frobnicate\", {})\n}\n",
    );
    assert!(
        route.contains("register_callable_host_operation"),
        "suggestion should name the host-side successor, got: {route}"
    );
    assert!(
        !route.contains("harness.acme"),
        "suggestion must not invent a capability, got: {route}"
    );
}

#[test]
fn non_literal_operation_falls_back_to_the_generic_route() {
    // Nothing to resolve when the destination is computed, so the rule must
    // still fire and must not claim a destination it cannot see.
    let route = wire_route(
        "fn main(harness: Harness) {\n  let op = \"ast.outline\"\n  let out = host_call(op, {})\n}\n",
    );
    assert!(
        route.contains("harness.<capability>.<operation>"),
        "suggestion should stay generic, got: {route}"
    );
}

#[test]
fn trusted_host_dispatch_does_not_report_a_privileged_wire() {
    // A privileged artifact is exactly who `privileged_wire` admits. Reporting
    // it there lands on a host's whole corpus at once — 84 findings in
    // the hosted runtime, 114 in a downstream host (harn#6162).
    let source =
        "fn main(harness: Harness) {\n  let out = host_call(\"ast.outline\", {path: \"a.rs\"})\n}\n";
    assert_eq!(
        count_rule(&lint_source(source), "non-source-callable-builtin"),
        1,
        "untrusted lint must still report it"
    );
    assert_eq!(
        count_rule(&lint_trusted(source), "non-source-callable-builtin"),
        0,
        "trusted host dispatch admits the wire"
    );
}

#[test]
fn trusted_host_dispatch_still_reports_a_runtime_internal() {
    // The flag says who may reach a *wire*. A compiler/runtime internal is
    // never source-visible, to anyone, so it must keep reporting under both
    // regimes — otherwise the flag is a blanket mute rather than a trust
    // decision.
    //
    // Registry-driven rather than a hardcoded name: the rule only reaches a
    // `RuntimeInternal` when no migration recipe claims it first, and which
    // names satisfy that moves as capabilities land. Pick whatever the live
    // registry currently reports, then assert the flag does not silence it.
    let probe = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .filter(|name| !name.starts_with("__") && !name.starts_with("hostlib_"))
        .filter(|name| !name.contains('.'))
        .filter(|name| {
            matches!(
                harn_vm::stdlib::builtin_exposure(name),
                Some(harn_builtin_meta::BuiltinExposure::RuntimeInternal)
            )
        })
        .map(|name| format!("fn main(harness: Harness) {{\n  {name}()\n}}\n"))
        .find(|source| {
            parse_and_lint(source).is_some_and(|diagnostics| {
                count_rule(&diagnostics, "non-source-callable-builtin") == 1
            })
        })
        .expect("some runtime-internal builtin reports the rule");

    assert_eq!(
        count_rule(&lint_trusted(&probe), "non-source-callable-builtin"),
        1,
        "trusted host dispatch must not hide a runtime internal: {probe}"
    );
}

#[test]
fn trusted_host_dispatch_still_reports_a_migrated_ambient_builtin() {
    // Measured on the real corpus: under the flag the downstream HARN-LNT-072
    // count goes to zero while its HARN-LNT-071 count stays at 99. The flag
    // admits the wire and changes nothing about the capability migrations.
    let source = "fn main(harness: Harness) {\n  log_info(\"hello\")\n}\n";
    assert_eq!(
        count_rule(&lint_trusted(source), "ambient-harness-method"),
        1
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
