use super::*;

/// Run `snippet` against an empty manifest so nothing but validation can decide
/// the outcome.
async fn execute_unbound_snippet(run_id: &str, snippet: &str) -> CompositionExecutionReport {
    execute_harn_composition(
        CompositionExecutionRequest {
            run_id: run_id.to_string(),
            snippet: snippet.to_string(),
            manifest: BindingManifest::default(),
            ..CompositionExecutionRequest::default()
        },
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
    )
    .await
}

/// The bare spelling the ambient-to-harness migration removed. Pinned so the
/// harness-spelling case below is a comparison rather than a lone assertion.
#[tokio::test(flavor = "current_thread")]
async fn composition_denies_the_legacy_bare_interaction_spelling() {
    let report = execute_unbound_snippet(
        "deny-bare-ask-user",
        "return ask_user({prompt: \"continue?\"})",
    )
    .await;

    assert!(!report.ok);
    assert_eq!(
        report.run.failure_category,
        Some(CompositionFailureCategory::PolicyDenied)
    );
    assert!(report.summary.contains("ask_user"), "{}", report.summary);
    assert!(report.child_calls.is_empty());
}

/// The spelling `HARN-LNT-071` migrates authors to. Before this was fixed it
/// passed validation and reached the null harness, so an author following the
/// lint traded a validation error for a late runtime denial.
#[tokio::test(flavor = "current_thread")]
async fn composition_denies_the_canonical_harness_spelling_at_validation_time() {
    let report = execute_unbound_snippet(
        "deny-harness-ask-user",
        "return harness.interaction.ask_user({prompt: \"continue?\"})",
    )
    .await;

    assert!(!report.ok);
    assert_eq!(
        report.run.failure_category,
        Some(CompositionFailureCategory::PolicyDenied)
    );
    assert!(
        report.summary.contains("harness.interaction.ask_user"),
        "{}",
        report.summary
    );
    assert!(report.child_calls.is_empty());
}

/// The denial keys on the receiver being the pipeline's own harness handle, not
/// on the method name, so a local that happens to share a method name is not
/// swept up. This snippet fails for its own reason — the check must not be that
/// reason.
#[tokio::test(flavor = "current_thread")]
async fn composition_denial_does_not_reach_a_non_harness_receiver() {
    let report = execute_unbound_snippet(
        "allow-colliding-method-name",
        "const helper = {interaction: {}}\nreturn helper.interaction.ask_user({})",
    )
    .await;

    assert!(!report.ok);
    assert!(
        !report.summary.contains("without harness capabilities"),
        "{}",
        report.summary
    );
}

/// Receiver authority follows the entrypoint parameter's declaration, not its
/// spelling. Nested callables and locals may legally reuse `harness` without
/// acquiring the composition entrypoint's denied host capabilities.
#[tokio::test(flavor = "current_thread")]
async fn composition_denial_respects_lexical_harness_shadowing() {
    for (run_id, snippet) in [
        (
            "closure-harness-shadow",
            "const helper = fn(harness) { return harness.tools.run_command({}) }\nreturn helper({})",
        ),
        (
            "function-harness-shadow",
            "fn helper(harness) { return harness.tools.run_command({}) }\nreturn helper({})",
        ),
        (
            "local-harness-shadow",
            "const harness = {tools: {}}\nreturn harness.tools.run_command({})",
        ),
    ] {
        let report = execute_unbound_snippet(run_id, snippet).await;
        assert!(
            !report.summary.contains("without harness capabilities"),
            "shadowed receiver inherited entrypoint authority: {}",
            report.summary
        );
    }
}

/// Optional and recursively nested properties retain the root parameter's
/// binding identity all the way to the invoked method.
#[tokio::test(flavor = "current_thread")]
async fn composition_denies_recursive_optional_harness_receivers() {
    let report = execute_unbound_snippet(
        "deny-recursive-optional-harness",
        "return harness?.interaction.channel.ask_user({prompt: \"continue?\"})",
    )
    .await;

    assert!(!report.ok);
    assert_eq!(
        report.run.failure_category,
        Some(CompositionFailureCategory::PolicyDenied)
    );
    assert!(
        report
            .summary
            .contains("harness.interaction.channel.ask_user"),
        "{}",
        report.summary
    );
    assert!(report.child_calls.is_empty());
}
