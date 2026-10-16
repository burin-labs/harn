use super::extract::extract_llm_options;
use crate::llm::api::PromptCacheTtl;
use crate::value::{DictMap, VmDictExt, VmError, VmValue};

fn opts_with(options: DictMap) -> crate::llm::api::LlmCallOptions {
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options")
}

fn try_opts_with(options: DictMap) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ])
}

fn thrown_message(err: VmError) -> String {
    match err {
        VmError::Thrown(VmValue::String(message)) => message.to_string(),
        VmError::Thrown(VmValue::Dict(fields)) => fields
            .get("message")
            .map(VmValue::display)
            .unwrap_or_else(|| "missing structured error message".to_string()),
        other => format!("{other:?}"),
    }
}

// Install an authored mock route so admission sees both prompt caching and
// the provider-specific TTL lowering without requiring a live API key. A
// bare mock model still falls through to non-caching defaults.
fn caching_route() -> DictMap {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.mock]]
model_match = "claude-sonnet-4.6"
prompt_caching = true
prompt_cache_ttls = ["5m", "1h"]
"#,
    )
    .expect("mock cache capability override");
    let mut options = DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "claude-sonnet-4.6");
    options
}

fn non_caching_route() -> DictMap {
    let mut options = DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "no-cache-model");
    options
}

#[test]
fn rate_limit_consumer_identity_defaults_to_session_and_can_be_overridden() {
    let mut options = non_caching_route();
    options.put_str("session_id", "session-a");
    let session_scoped = opts_with(options);
    assert_eq!(
        session_scoped.rate_limit_consumer_id.as_deref(),
        Some("session-a")
    );

    let mut options = non_caching_route();
    options.put_str("session_id", "session-b");
    options.put_str("rate_limit_consumer_id", "tenant-7");
    let tenant_scoped = opts_with(options);
    assert_eq!(
        tenant_scoped.rate_limit_consumer_id.as_deref(),
        Some("tenant-7")
    );
}

#[test]
fn semantic_call_role_is_shared_with_fixture_scope_and_never_inferred() {
    let unattributed = opts_with(non_caching_route());
    assert_eq!(
        unattributed.context_manifest.call_role(),
        "unattributed",
        "absence must stay observable instead of masquerading as a valid purpose"
    );
    assert_eq!(unattributed.mock_scope, None);

    let mut router_options = non_caching_route();
    router_options.put_str("call_role", "model.router");
    let router = opts_with(router_options);
    assert_eq!(router.context_manifest.call_role(), "model.router");
    assert_eq!(router.mock_scope.as_deref(), Some("model.router"));

    let mut agent_options = non_caching_route();
    agent_options.put_str("mock_scope", "agent.main");
    let agent = opts_with(agent_options);
    assert_eq!(agent.context_manifest.call_role(), "agent.main");

    let mut judge_options = non_caching_route();
    judge_options.put_str("mock_scope", "completion.judge");
    let judge = opts_with(judge_options);
    assert_eq!(judge.context_manifest.call_role(), "completion.judge");

    let roles = [
        router.context_manifest.call_role(),
        agent.context_manifest.call_role(),
        judge.context_manifest.call_role(),
    ];
    assert_eq!(
        roles
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "router, agent-under-test, and completion judge must be distinguishable by role alone"
    );
}

#[test]
fn call_role_and_fixture_scope_must_not_disagree() {
    let mut options = non_caching_route();
    options.put_str("call_role", "model.router");
    options.put_str("mock_scope", "agent.main");
    let error = try_opts_with(options).expect_err("semantic purpose mismatch must fail");
    assert!(
        thrown_message(error).contains("call_role `model.router` disagrees"),
        "unexpected error"
    );
}

#[test]
fn cache_defaults_off_for_non_supporting_route() {
    // A route whose capability matrix says `prompt_caching = false` must
    // resolve the default to OFF so the outgoing request stays byte-
    // identical and the strict capability gate never trips on a default.
    assert!(
        !crate::llm::capabilities::lookup("mock", "no-cache-model").prompt_caching,
        "precondition: route must not support caching"
    );
    assert!(
        !opts_with(non_caching_route()).cache,
        "cache default must be OFF when the route does not support caching"
    );
}

#[test]
fn cache_defaults_on_for_supporting_route() {
    // When the route supports prompt caching, the stable system+tools+
    // history prefix is marked cacheable by default so multi-turn loops
    // and the rubric grader pay the discounted cached-input rate.
    assert!(
        crate::llm::capabilities::lookup("mock", "claude-sonnet-4.6").prompt_caching,
        "precondition: route must support caching"
    );
    assert!(
        opts_with(caching_route()).cache,
        "cache must default ON for a caching-capable route"
    );
}

#[test]
fn explicit_cache_false_opts_out_on_supporting_route() {
    let mut options = caching_route();
    options.put("cache", VmValue::Bool(false));
    assert!(
        !opts_with(options).cache,
        "explicit `cache: false` must opt out"
    );
}

#[test]
fn models_ladder_lowers_to_routing_policy() {
    let mut options = DictMap::new();
    options.put(
        "models",
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from("mock-cheap")),
            VmValue::String(arcstr::ArcStr::from("mock-strong")),
        ])),
    );
    let opts = opts_with(options);
    let policy = opts
        .routing_policy
        .expect("ladder lowered to routing policy");
    assert!(policy.is_ladder);
    assert_eq!(policy.chain.len(), 2);
    // Base provider/model snap to the first rung.
    assert_eq!(opts.model, "mock-cheap");
}

#[test]
fn models_ladder_conflicts_with_explicit_model() {
    let mut options = DictMap::new();
    options.put(
        "models",
        VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            arcstr::ArcStr::from("mock-cheap"),
        )])),
    );
    options.put_str("model", "pinned-model");
    let result = try_opts_with(options);
    let err = match result {
        Ok(_) => panic!("models + model must be rejected as ambiguous"),
        Err(err) => err,
    };
    assert!(format!("{err:?}").contains("cannot be combined"));
}

#[test]
fn explicit_cache_true_errors_on_non_supporting_route() {
    // The strict capability gate is preserved: an explicit `cache: true`
    // on a route that cannot cache surfaces a loud error rather than a
    // silent no-op (unchanged behavior).
    let mut options = non_caching_route();
    options.put("cache", VmValue::Bool(true));
    assert!(
        try_opts_with(options).is_err(),
        "explicit `cache: true` on a non-supporting route must error"
    );
}

#[test]
fn prompt_cache_ttl_one_hour_parses_for_anthropic_route() {
    let mut options = caching_route();
    options.put_str("prompt_cache_ttl", "1h");
    let opts = opts_with(options);
    assert_eq!(opts.prompt_cache_ttl, Some(PromptCacheTtl::OneHour));
    assert!(opts.cache, "TTL requests keep provider prompt caching on");
}

#[test]
fn prompt_cache_ttl_rejects_invalid_values() {
    let mut options = caching_route();
    options.put_str("prompt_cache_ttl", "24h");
    let err = match try_opts_with(options) {
        Ok(_) => panic!("invalid TTL must error"),
        Err(err) => err,
    };
    assert!(thrown_message(err).contains("must be \"5m\" or \"1h\""));
}

#[test]
fn prompt_cache_ttl_conflicts_with_cache_false() {
    let mut options = caching_route();
    options.put("cache", VmValue::Bool(false));
    options.put_str("prompt_cache_ttl", "1h");
    let err = match try_opts_with(options) {
        Ok(_) => panic!("cache false + TTL must error"),
        Err(err) => err,
    };
    assert!(thrown_message(err).contains("requires provider prompt caching"));
}

#[test]
fn prompt_cache_ttl_errors_on_non_supporting_route() {
    let mut options = non_caching_route();
    options.put_str("prompt_cache_ttl", "1h");
    let err = match try_opts_with(options) {
        Ok(_) => panic!("unsupported TTL must error"),
        Err(err) => err,
    };
    assert!(thrown_message(err).contains("prompt_cache_ttl"));
}
