use super::*;
use crate::value::ErrorCategory;

fn dict(items: &[(&str, VmValue)]) -> crate::value::DictMap {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn equivalent_failover_excludes_internal_simulators() {
    let policy = build_equivalent_failover_policy(
        "mock",
        "model",
        3,
        true,
        crate::llm_config::EquivalentModelRequirements::default(),
    );
    assert!(policy.is_none());
}

#[test]
fn transport_fallbacks_lower_to_one_routing_chain() {
    let policy = build_transport_failover_policy(
        "mock",
        "primary-model",
        &[crate::llm::api::LlmRouteFallback {
            provider: "fake".to_string(),
            model: "backup-model".to_string(),
        }],
        &["mock".to_string()],
    )
    .expect("available fallback creates a routing policy");

    let routes: Vec<(&str, &str)> = policy
        .chain
        .iter()
        .map(|link| (link.provider.as_str(), link.model.as_str()))
        .collect();
    assert_eq!(
        routes,
        vec![("mock", "primary-model"), ("fake", "backup-model")]
    );
    assert_eq!(policy.failover.max_attempts, Some(2));
}

#[test]
fn routing_exhaustion_preserves_structured_attempt_chain() {
    let snapshot = RoutingErrorSnapshot {
        category: "circuit_open".to_string(),
        code: Some("provider_exhausted".to_string()),
        reason: Some("empty_generation".to_string()),
        attempt_count: Some(2),
        message: "empty generation".to_string(),
        status: None,
    };
    let mut trace = RoutingTrace {
        label: "test".to_string(),
        attempts: vec![RoutingAttempt {
            index: 0,
            provider: "primary".to_string(),
            model: "model".to_string(),
            label: "primary".to_string(),
            status: AttemptStatus::Failed,
            duration_ms: 12,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            error: Some(snapshot.clone()),
            verifier_signals: Vec::new(),
            verifier_outcome: None,
        }],
        selected: None,
        terminal: None,
        session_cost_usd: 0.0,
    };

    let error = provider_exhausted_routing_error(&trace, Some(&snapshot));
    let VmError::Thrown(VmValue::Dict(fields)) = error else {
        panic!("expected typed provider exhaustion");
    };
    assert_eq!(
        fields.get("code").map(VmValue::display).as_deref(),
        Some("provider_exhausted")
    );
    assert_eq!(
        fields.get("reason").map(VmValue::display).as_deref(),
        Some("empty_generation")
    );
    assert_eq!(
        fields.get("attempt_count").and_then(VmValue::as_int),
        Some(2)
    );
    let Some(VmValue::List(attempts)) = fields.get("attempts") else {
        panic!("expected attempt list");
    };
    assert_eq!(attempts.len(), 1);
    let attempt = attempts[0].as_dict().expect("attempt dict");
    let nested = attempt
        .get("error")
        .and_then(VmValue::as_dict)
        .expect("structured attempt error");
    assert_eq!(
        nested.get("reason").map(VmValue::display).as_deref(),
        Some("empty_generation")
    );

    trace.attempts.push(RoutingAttempt {
        index: 1,
        provider: "budget-skipped".to_string(),
        model: "model".to_string(),
        label: "budget-skipped".to_string(),
        status: AttemptStatus::Skipped,
        duration_ms: 0,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: None,
        verifier_signals: Vec::new(),
        verifier_outcome: None,
    });
    assert_eq!(
        physical_request_attempt_count(&trace),
        2,
        "budget-skipped routes are receipts, not physical provider requests"
    );

    trace.attempts.push(RoutingAttempt {
        index: 2,
        provider: "quarantined".to_string(),
        model: "model".to_string(),
        label: "quarantined".to_string(),
        status: AttemptStatus::Failed,
        duration_ms: 0,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: Some(RoutingErrorSnapshot {
            category: "circuit_open".to_string(),
            code: Some("route_quarantined".to_string()),
            reason: Some("unproductive_completion".to_string()),
            attempt_count: Some(0),
            message: "route is quarantined".to_string(),
            status: None,
        }),
        verifier_signals: Vec::new(),
        verifier_outcome: None,
    });
    assert_eq!(
        physical_request_attempt_count(&trace),
        2,
        "quarantined routes are logical attempts with zero provider requests"
    );
}

#[test]
fn build_routing_policy_validates_chain() {
    clear_policy_registry();
    let config = dict(&[
        (
            "chain",
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(arcstr::ArcStr::from("mock:mock")),
                VmValue::dict(dict(&[
                    ("provider", VmValue::String(arcstr::ArcStr::from("mock"))),
                    ("model", VmValue::String(arcstr::ArcStr::from("mock-2"))),
                ])),
            ])),
        ),
        (
            "failover",
            VmValue::dict(dict(&[
                (
                    "on_status",
                    VmValue::List(std::sync::Arc::new(vec![
                        VmValue::Int(429),
                        VmValue::Int(500),
                    ])),
                ),
                ("max_attempts", VmValue::Int(2)),
            ])),
        ),
        (
            "budget",
            VmValue::dict(dict(&[
                ("per_call_usd", VmValue::Float(0.5)),
                ("on_exceed", VmValue::String(arcstr::ArcStr::from("abort"))),
            ])),
        ),
    ]);
    let tagged = build_routing_policy(&config).expect("validates");
    let inner = tagged.as_dict().expect("dict");
    assert!(matches!(
        inner.get(ROUTING_POLICY_TAG),
        Some(VmValue::Bool(true))
    ));
    assert!(inner.contains_key(HANDLE_KEY));
    let handle = inner.get(HANDLE_KEY).and_then(|v| v.as_int()).unwrap();
    let policy = lookup_policy(handle as u64).expect("policy registered");
    assert_eq!(policy.chain.len(), 2);
    assert_eq!(policy.failover.on_status, vec![429, 500]);
}

#[test]
fn chain_link_region_parses_summarizes_and_threads_into_options() {
    clear_policy_registry();
    let config = dict(&[(
        "chain",
        VmValue::List(std::sync::Arc::new(vec![
            // Link 0: explicit region override.
            VmValue::dict(dict(&[
                ("provider", VmValue::String(arcstr::ArcStr::from("bedrock"))),
                (
                    "model",
                    VmValue::String(arcstr::ArcStr::from("anthropic.claude-3-5-sonnet-v2:0")),
                ),
                ("region", VmValue::String(arcstr::ArcStr::from("eu-west-1"))),
            ])),
            // Link 1: no region -> falls back to env at call time.
            VmValue::String(arcstr::ArcStr::from("mock:mock")),
        ])),
    )]);
    let tagged = build_routing_policy(&config).expect("validates");
    let inner = tagged.as_dict().expect("dict");

    // Parsed chain carries the region on link 0 and None on link 1.
    let handle = inner.get(HANDLE_KEY).and_then(|v| v.as_int()).unwrap();
    let policy = lookup_policy(handle as u64).expect("policy registered");
    assert_eq!(policy.chain[0].region.as_deref(), Some("eu-west-1"));
    assert_eq!(policy.chain[1].region, None);

    // The summary dict echoes the region back for introspection,
    // and only on the link that set it.
    let chain_summary = match inner.get("chain") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("expected chain list, got {other:?}"),
    };
    let link0 = chain_summary[0].as_dict().expect("link0 dict");
    assert_eq!(
        link0.get("region").and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        }),
        Some("eu-west-1".to_string())
    );
    let link1 = chain_summary[1].as_dict().expect("link1 dict");
    assert!(!link1.contains_key("region"));

    // link_options threads the region into the per-link call options;
    // the region-less link resolves to None (env fallback).
    let base = crate::llm::api::options::base_opts("bedrock");
    let with_region = auth::link_options(&base, &policy, &policy.chain[0]);
    assert_eq!(with_region.region.as_deref(), Some("eu-west-1"));
    let without_region = auth::link_options(&base, &policy, &policy.chain[1]);
    assert_eq!(without_region.region, None);
}

#[test]
fn build_rejects_empty_chain() {
    clear_policy_registry();
    let config = dict(&[("chain", VmValue::List(std::sync::Arc::new(Vec::new())))]);
    let err = build_routing_policy(&config).unwrap_err();
    let message = match err {
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        other => panic!("unexpected error: {other:?}"),
    };
    assert!(message.contains("at least one"));
}

#[test]
fn build_rejects_invalid_status_code() {
    clear_policy_registry();
    let config = dict(&[
        (
            "chain",
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("mock:mock"),
            )])),
        ),
        (
            "failover",
            VmValue::dict(dict(&[(
                "on_status",
                VmValue::List(std::sync::Arc::new(vec![VmValue::Int(42)])),
            )])),
        ),
    ]);
    let err = build_routing_policy(&config).unwrap_err();
    let message = match err {
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        other => panic!("unexpected error: {other:?}"),
    };
    assert!(message.contains("not a valid HTTP status"));
}

#[test]
fn matches_failover_default_status() {
    let rules = FailoverRules::default();
    let err = VmError::Runtime("HTTP 429 rate limit".to_string());
    let (eligible, snap) = matches_failover(&rules, &err);
    assert!(eligible);
    assert_eq!(snap.status, Some(429));
}

#[test]
fn matches_failover_default_circuit_open() {
    let rules = FailoverRules::default();
    let err = VmError::CategorizedError {
        message: "rate governor circuit_open after empty completion budget".to_string(),
        category: ErrorCategory::CircuitOpen,
    };
    let (eligible, snap) = matches_failover(&rules, &err);
    assert!(eligible);
    assert_eq!(snap.category, "circuit_open");
}

#[test]
fn matches_failover_explicit_kind() {
    let rules = FailoverRules {
        on_error_kinds: vec!["rate_limit".to_string()],
        ..Default::default()
    };
    let err = VmError::CategorizedError {
        message: "throttled".to_string(),
        category: ErrorCategory::RateLimit,
    };
    let (eligible, _) = matches_failover(&rules, &err);
    assert!(eligible);
}

#[test]
fn no_dispatch_contract_violation_does_not_failover_by_default() {
    let rules = FailoverRules::default();
    let err = VmError::Runtime(
        "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
         (completion_tokens=342) with no dispatchable tool call or answer \
         (upstream contract violation): the model finished cleanly"
            .to_string(),
    );

    let (eligible, _) = matches_failover(&rules, &err);

    assert!(!eligible);
}

#[test]
fn no_dispatch_contract_violation_can_opt_into_failover() {
    let rules = FailoverRules {
        on_no_dispatch: true,
        ..Default::default()
    };
    let err = VmError::Runtime(
        "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
         (completion_tokens=342) with no dispatchable tool call or answer \
         (upstream contract violation): the model finished cleanly"
            .to_string(),
    );

    let (eligible, snap) = matches_failover(&rules, &err);

    assert!(eligible);
    assert!(snap.message.contains("upstream contract violation"));
}

#[test]
fn no_dispatch_matcher_requires_billed_completion_token_contract_shape() {
    let rules = FailoverRules {
        on_no_dispatch: true,
        ..Default::default()
    };
    let cases = [
        "returned billed output with no dispatchable tool call or answer \
         (upstream contract violation)",
        "returned billed output (completion_tokens=12) with no answer \
         (upstream contract violation)",
        "returned billed output (completion_tokens=12) with no dispatchable tool call or answer",
        "completion_tokens=12 with no dispatchable tool call or answer \
         (upstream contract violation)",
    ];

    for message in cases {
        let (eligible, _) = matches_failover(&rules, &VmError::Runtime(message.to_string()));
        assert!(!eligible, "message should not be eligible: {message}");
    }
}

#[test]
fn explicit_failover_kind_does_not_implicitly_match_timeout() {
    let rules = FailoverRules {
        on_error_kinds: vec!["rate_limit".to_string()],
        ..Default::default()
    };
    let err = VmError::CategorizedError {
        message: "timed out".to_string(),
        category: ErrorCategory::Timeout,
    };
    let (eligible, _) = matches_failover(&rules, &err);
    assert!(!eligible);
}

#[test]
fn rejects_non_failover_error_by_default() {
    let rules = FailoverRules::default();
    let err = VmError::CategorizedError {
        message: "schema mismatch".to_string(),
        category: ErrorCategory::SchemaValidation,
    };
    let (eligible, _) = matches_failover(&rules, &err);
    assert!(!eligible);
}

#[test]
fn budget_envelope_round_trips() {
    let budget = BudgetRules {
        per_call_usd: Some(0.25),
        session_usd: Some(5.0),
        on_exceed: Some(BudgetExceedAction::Skip),
    };
    let envelope = budget.envelope().unwrap();
    assert_eq!(envelope.max_cost_usd, Some(0.25));
    assert_eq!(envelope.total_budget_usd, Some(5.0));
}

fn str_list(items: &[&str]) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        items
            .iter()
            .map(|s| VmValue::String(arcstr::ArcStr::from(*s)))
            .collect(),
    ))
}

#[test]
fn model_ladder_returns_none_without_models_or_ladder() {
    let options = dict(&[("model", VmValue::String(arcstr::ArcStr::from("x")))]);
    let policy = build_model_ladder_policy(&options, "anthropic", "x").expect("ok");
    assert!(policy.is_none());
}

#[test]
fn model_ladder_from_string_sugar_builds_ladder_chain() {
    let options = dict(&[("models", str_list(&["mock-cheap", "mock-strong"]))]);
    let policy = build_model_ladder_policy(&options, "anthropic", "base")
        .expect("ok")
        .expect("ladder present");
    assert!(policy.is_ladder);
    assert_eq!(policy.chain.len(), 2);
    assert_eq!(policy.chain[0].model, "mock-cheap");
    assert_eq!(policy.chain[1].model, "mock-strong");
    // One transport attempt per rung.
    assert_eq!(policy.failover.max_attempts, Some(2));
}

#[test]
fn model_ladder_dict_step_honors_explicit_provider() {
    let step = dict(&[
        ("model", VmValue::String(arcstr::ArcStr::from("gpt-x"))),
        ("provider", VmValue::String(arcstr::ArcStr::from("openai"))),
    ]);
    let options = dict(&[(
        "models",
        VmValue::List(std::sync::Arc::new(vec![VmValue::dict(step)])),
    )]);
    let policy = build_model_ladder_policy(&options, "anthropic", "base")
        .expect("ok")
        .expect("ladder");
    assert_eq!(policy.chain[0].provider, "openai");
    assert_eq!(policy.chain[0].model, "gpt-x");
}

#[test]
fn model_ladder_and_ladder_are_mutually_exclusive() {
    let options = dict(&[
        ("models", str_list(&["a", "b"])),
        ("ladder", VmValue::String(arcstr::ArcStr::from("frugal"))),
    ]);
    let err = build_model_ladder_policy(&options, "anthropic", "base").unwrap_err();
    assert!(format!("{err:?}").contains("mutually exclusive"));
}

#[test]
fn model_ladder_step_rejects_unknown_override_key() {
    let step = dict(&[
        ("model", VmValue::String(arcstr::ArcStr::from("m"))),
        (
            "options",
            VmValue::dict(dict(&[(
                "tools",
                VmValue::List(std::sync::Arc::new(vec![])),
            )])),
        ),
    ]);
    let options = dict(&[(
        "models",
        VmValue::List(std::sync::Arc::new(vec![VmValue::dict(step)])),
    )]);
    let err = build_model_ladder_policy(&options, "anthropic", "base").unwrap_err();
    assert!(format!("{err:?}").contains("not a supported"));
}

#[test]
fn model_ladder_step_accepts_scalar_overrides() {
    let step = dict(&[
        ("model", VmValue::String(arcstr::ArcStr::from("m"))),
        ("provider", VmValue::String(arcstr::ArcStr::from("mock"))),
        (
            "options",
            VmValue::dict(dict(&[
                ("max_tokens", VmValue::Int(256)),
                ("temperature", VmValue::Float(0.0)),
            ])),
        ),
    ]);
    let options = dict(&[(
        "models",
        VmValue::List(std::sync::Arc::new(vec![VmValue::dict(step)])),
    )]);
    let policy = build_model_ladder_policy(&options, "mock", "base")
        .expect("ok")
        .expect("ladder");
    let overrides = policy.chain[0].overrides.as_ref().expect("overrides");
    assert_eq!(
        overrides.get("max_tokens").and_then(VmValue::as_int),
        Some(256)
    );
    // The override is applied over the base options at link-dispatch time.
    let mut base = policy_base_opts();
    base.max_tokens = 16384;
    let linked = auth::link_options(&base, &policy, &policy.chain[0]);
    assert_eq!(linked.max_tokens, 256);
    assert_eq!(linked.temperature, Some(0.0));
}

/// Minimal `LlmCallOptions` for `link_options` unit tests. Mirrors the
/// production `base_opts` constructor closely enough to exercise the
/// per-step override application without pulling in option normalization.
fn policy_base_opts() -> LlmCallOptions {
    crate::llm::api::options::base_opts("mock")
}

#[test]
fn named_ladder_resolves_from_catalog() {
    // `frugal` ships in the embedded catalog
    // (catalog_sources/62-ladders). Resolve it and confirm the chain is
    // the declared haiku -> sonnet -> opus escalation.
    let options = dict(&[("ladder", VmValue::String(arcstr::ArcStr::from("frugal")))]);
    let policy = build_model_ladder_policy(&options, "anthropic", "base")
        .expect("ok")
        .expect("frugal ladder present in catalog");
    assert!(policy.is_ladder);
    assert_eq!(policy.chain.len(), 3);
    // Aliases resolve to their canonical anthropic routes.
    assert_eq!(policy.chain[0].provider, "anthropic");
    assert!(policy.chain[2].model.contains("opus"));
}

#[test]
fn unknown_named_ladder_errors_with_hint() {
    let options = dict(&[(
        "ladder",
        VmValue::String(arcstr::ArcStr::from("does-not-exist")),
    )]);
    let err = build_model_ladder_policy(&options, "anthropic", "base").unwrap_err();
    assert!(format!("{err:?}").contains("no model ladder named"));
}

#[test]
fn catalog_step_options_thread_into_overrides() {
    // A catalog step's `options` table lowers to per-step overrides,
    // exactly like an inline `models:` step — no longer silently dropped.
    let mut options = std::collections::BTreeMap::new();
    options.insert("temperature".to_string(), toml::Value::Float(0.25));
    options.insert("max_tokens".to_string(), toml::Value::Integer(128));
    let overrides = super::catalog_step_overrides(Some(&options), "frugal", 0)
        .expect("ok")
        .expect("some overrides");
    assert!(matches!(
        overrides.get(&crate::value::intern_key("temperature")),
        Some(VmValue::Float(f)) if (*f - 0.25).abs() < 1e-9
    ));
    assert!(matches!(
        overrides.get(&crate::value::intern_key("max_tokens")),
        Some(VmValue::Int(128))
    ));
}

#[test]
fn catalog_step_unknown_option_errors_loudly() {
    let mut options = std::collections::BTreeMap::new();
    options.insert("tools".to_string(), toml::Value::Boolean(true));
    let err = super::catalog_step_overrides(Some(&options), "frugal", 1).unwrap_err();
    assert!(format!("{err:?}").contains("supported per-step override"));
}

#[test]
fn catalog_step_absent_options_is_none() {
    assert!(super::catalog_step_overrides(None, "frugal", 0)
        .expect("ok")
        .is_none());
    let empty = std::collections::BTreeMap::new();
    assert!(super::catalog_step_overrides(Some(&empty), "frugal", 0)
        .expect("ok")
        .is_none());
}

fn failed_attempt(index: usize, provider: &str, model: &str) -> RoutingAttempt {
    RoutingAttempt {
        index,
        provider: provider.to_string(),
        model: model.to_string(),
        label: format!("{provider}:{model}"),
        status: AttemptStatus::Failed,
        duration_ms: 5,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: None,
        verifier_signals: Vec::new(),
        verifier_outcome: None,
    }
}

/// F3: the terminal route diverges from the base/requested route. When the
/// routed backup (attempt 2) produced the terminal error, the top-level
/// provider/model must name that routed attempt — not the base route (attempt
/// 1). Matched by `.index`, so configured order never misattributes.
#[test]
fn terminal_attempt_stamps_the_routed_provider_and_model() {
    let snapshot = RoutingErrorSnapshot {
        category: "circuit_open".to_string(),
        code: Some("provider_exhausted".to_string()),
        reason: Some("overloaded".to_string()),
        attempt_count: Some(1),
        message: "overloaded".to_string(),
        status: None,
    };
    let trace = RoutingTrace {
        label: "test".to_string(),
        attempts: vec![
            failed_attempt(1, "base-provider", "base-model"),
            failed_attempt(2, "routed-provider", "routed-model"),
        ],
        selected: None,
        terminal: Some(TerminalRoute::Attempt(2)),
        session_cost_usd: 0.0,
    };

    let VmError::Thrown(VmValue::Dict(fields)) =
        provider_exhausted_routing_error(&trace, Some(&snapshot))
    else {
        panic!("expected typed provider exhaustion");
    };
    assert_eq!(
        fields.get("provider").map(VmValue::display).as_deref(),
        Some("routed-provider"),
        "top-level provider must be the routed terminal attempt, not the base"
    );
    assert_eq!(
        fields.get("model").map(VmValue::display).as_deref(),
        Some("routed-model")
    );
    assert!(
        fields.get("no_single_route").is_none(),
        "a single-route terminal must not set the composite flag"
    );
}

/// F2 (consumption): a `Composite` terminal (no single route is responsible)
/// must never stamp a provider/model, and must set `no_single_route` so the
/// outer `build_llm_error_dict` skips its base-route fill instead of fabricating
/// a route.
#[test]
fn composite_terminal_never_fabricates_a_route() {
    let trace = RoutingTrace {
        label: "test".to_string(),
        attempts: vec![
            failed_attempt(1, "primary-provider", "primary-model"),
            failed_attempt(2, "backup-provider", "backup-model"),
        ],
        selected: None,
        terminal: Some(TerminalRoute::Composite),
        session_cost_usd: 0.0,
    };

    let VmError::Thrown(VmValue::Dict(fields)) = provider_exhausted_routing_error(&trace, None)
    else {
        panic!("expected typed provider exhaustion");
    };
    assert!(
        fields.get("provider").is_none(),
        "composite terminal must not fabricate a provider"
    );
    assert!(
        fields.get("model").is_none(),
        "composite terminal must not fabricate a model"
    );
    assert!(
        matches!(fields.get("no_single_route"), Some(VmValue::Bool(true))),
        "composite terminal must set the no_single_route signal"
    );
}
