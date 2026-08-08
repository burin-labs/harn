use arcstr::ArcStr;

use super::api::options::base_opts;

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn install_missing_key_providers(providers: &[(&str, &str)]) {
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    for (provider, env) in providers {
        overlay.providers.insert(
            (*provider).to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("https://{provider}.example/v1"),
                auth_style: "bearer".to_string(),
                auth_env: crate::llm_config::AuthEnv::Single((*env).to_string()),
                chat_endpoint: "/chat/completions".to_string(),
                ..Default::default()
            },
        );
    }
    crate::llm_config::set_user_overrides(Some(overlay));
}

#[test]
fn routing_skips_uncredentialed_primary_without_transport_call() {
    use super::fake::{
        fake_llm_captured_calls, install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn,
        FakeStopReason,
    };

    let _guard = super::env_guard();
    let _missing_key = ScopedEnvVar::remove("HARN_TEST_ROUTING_MISSING_PRIMARY_KEY");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        install_missing_key_providers(&[(
            "missing-primary",
            "HARN_TEST_ROUTING_MISSING_PRIMARY_KEY",
        )]);
        let _fake = install_fake_llm_script(FakeLlmScript::new().push(FakeLlmTurn::stream(vec![
            FakeLlmEvent::Token("backup won".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ])));

        let mut opts = base_opts("missing-primary");
        opts.model = "missing-primary-model".to_string();
        opts.stream = false;
        let policy = super::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::api::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-backup-model".to_string(),
            }],
            &[],
        )
        .expect("backup creates routing policy");

        let local = tokio::task::LocalSet::new();
        let (result, trace) = local
            .run_until(super::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect("routing must skip missing credentials and use backup");

        assert_eq!(result.provider, "fake");
        assert_eq!(result.text, "backup won");
        assert_eq!(fake_llm_captured_calls().len(), 1);
        assert_eq!(trace.attempts.len(), 2);
        let primary = &trace.attempts[0];
        assert!(matches!(
            primary.status,
            super::routing::AttemptStatus::Skipped
        ));
        let error = primary.error.as_ref().expect("skip error receipt");
        assert_eq!(error.code.as_deref(), Some("route_unavailable"));
        assert_eq!(error.reason.as_deref(), Some("missing_credentials"));
        assert_eq!(error.attempt_count, Some(0));
        assert!(matches!(
            trace.attempts[1].status,
            super::routing::AttemptStatus::Succeeded
        ));

        crate::llm_config::clear_user_overrides();
    });
}

#[test]
fn routing_all_uncredentialed_routes_exhaust_with_zero_physical_attempts() {
    let _guard = super::env_guard();
    let _primary_key = ScopedEnvVar::remove("HARN_TEST_ROUTING_MISSING_A_KEY");
    let _backup_key = ScopedEnvVar::remove("HARN_TEST_ROUTING_MISSING_B_KEY");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        install_missing_key_providers(&[
            ("missing-a", "HARN_TEST_ROUTING_MISSING_A_KEY"),
            ("missing-b", "HARN_TEST_ROUTING_MISSING_B_KEY"),
        ]);

        let mut opts = base_opts("missing-a");
        opts.model = "missing-a-model".to_string();
        opts.stream = false;
        let chain = crate::value::VmValue::List(std::sync::Arc::new(vec![
            route("missing-a", "missing-a-model"),
            route("missing-b", "missing-b-model"),
        ]));
        let tagged = super::routing::build_routing_policy(&crate::value::DictMap::from_iter([(
            crate::value::intern_key("chain"),
            chain,
        )]))
        .expect("explicit routing policy validates");
        let policy = super::routing::extract_routing_policy(Some(
            &crate::value::DictMap::from_iter([(crate::value::intern_key("routing"), tagged)]),
        ))
        .expect("routing policy extracts")
        .expect("routing policy present");

        let local = tokio::task::LocalSet::new();
        let error = local
            .run_until(super::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect_err("all unavailable routes must exhaust structurally");

        let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
            panic!("expected structured provider exhaustion");
        };
        assert_eq!(
            fields.get("code").map(crate::value::VmValue::display),
            Some("provider_exhausted".to_string())
        );
        assert_eq!(
            fields
                .get("attempt_count")
                .and_then(crate::value::VmValue::as_int),
            Some(0)
        );
        let Some(crate::value::VmValue::List(attempts)) = fields.get("attempts") else {
            panic!("expected route ledger");
        };
        assert_eq!(attempts.len(), 2);
        for attempt in attempts.iter() {
            let attempt = attempt.as_dict().expect("attempt receipt");
            assert_eq!(
                attempt.get("status").map(crate::value::VmValue::display),
                Some("skipped".to_string())
            );
            let error = attempt
                .get("error")
                .and_then(crate::value::VmValue::as_dict)
                .expect("attempt error");
            assert_eq!(
                error.get("code").map(crate::value::VmValue::display),
                Some("route_unavailable".to_string())
            );
            assert_eq!(
                error
                    .get("attempt_count")
                    .and_then(crate::value::VmValue::as_int),
                Some(0)
            );
        }

        crate::llm_config::clear_user_overrides();
    });
}

fn explicit_policy(
    routes: Vec<crate::value::VmValue>,
) -> std::sync::Arc<super::routing::RoutingPolicyConfig> {
    let chain = crate::value::VmValue::List(std::sync::Arc::new(routes));
    let tagged = super::routing::build_routing_policy(&crate::value::DictMap::from_iter([(
        crate::value::intern_key("chain"),
        chain,
    )]))
    .expect("explicit routing policy validates");
    super::routing::extract_routing_policy(Some(&crate::value::DictMap::from_iter([(
        crate::value::intern_key("routing"),
        tagged,
    )])))
    .expect("routing policy extracts")
    .expect("routing policy present")
}

/// F1 + F3 integrated: two routes both fail (sequential failover). The routed
/// backup produced the terminal error, so the top-level provider/model of the
/// thrown exhaustion error must name the BACKUP route, not the base/primary
/// route the session requested.
#[test]
fn exhausted_routing_error_names_the_terminal_backup_route_not_the_base() {
    use super::fake::{install_fake_llm_script, FakeLlmScript, FakeLlmTurn};

    let _guard = super::env_guard();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let _fake = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::error(
                    crate::value::ErrorCategory::CircuitOpen,
                    "primary route down",
                ))
                .push(FakeLlmTurn::error(
                    crate::value::ErrorCategory::CircuitOpen,
                    "backup route down",
                )),
        );

        let mut opts = base_opts("fake");
        opts.model = "primary-model".to_string();
        opts.stream = false;
        let policy = explicit_policy(vec![
            route("fake", "primary-model"),
            route("fake", "backup-model"),
        ]);

        let local = tokio::task::LocalSet::new();
        let error = local
            .run_until(super::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect_err("both routes failing must exhaust the chain");

        let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
            panic!("expected structured provider exhaustion");
        };
        assert_eq!(
            fields.get("model").map(crate::value::VmValue::display),
            Some("backup-model".to_string()),
            "terminal route must name the backup (routed) model, not the base"
        );
        assert!(
            fields.get("no_single_route").is_none(),
            "a resolved single terminal route must not set the composite flag"
        );
    });
}

/// F4: a single-route call that fails is honest and unchanged — the top-level
/// provider/model name that one route (which is also the base), with no
/// composite signal.
#[test]
fn single_route_exhaustion_names_the_only_route() {
    use super::fake::{install_fake_llm_script, FakeLlmScript, FakeLlmTurn};

    let _guard = super::env_guard();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let _fake = install_fake_llm_script(FakeLlmScript::new().push(FakeLlmTurn::error(
            crate::value::ErrorCategory::CircuitOpen,
            "only route down",
        )));

        let mut opts = base_opts("fake");
        opts.model = "only-model".to_string();
        opts.stream = false;
        let policy = explicit_policy(vec![route("fake", "only-model")]);

        let local = tokio::task::LocalSet::new();
        let error = local
            .run_until(super::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect_err("the single route failing must exhaust the chain");

        let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
            panic!("expected structured provider exhaustion");
        };
        assert_eq!(
            fields.get("model").map(crate::value::VmValue::display),
            Some("only-model".to_string())
        );
        assert!(fields.get("no_single_route").is_none());
    });
}

fn route(provider: &'static str, model: &'static str) -> crate::value::VmValue {
    crate::value::VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            crate::value::VmValue::String(ArcStr::from(provider)),
        ),
        (
            crate::value::intern_key("model"),
            crate::value::VmValue::String(ArcStr::from(model)),
        ),
    ]))
}
