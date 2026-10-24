use super::*;

const ROUTES: [(&str, &str); 4] = [
    ("fireworks", "accounts/fireworks/models/gpt-oss-120b"),
    ("groq", "groq/openai/gpt-oss-120b"),
    ("fireworks", "accounts/fireworks/models/glm-5p3-flash"),
    ("fireworks", "accounts/fireworks/models/deepseek-v4p1-flash"),
];

fn options(provider: &str, model: &str) -> LlmCallOptions {
    let mut options = opts(1.0);
    options.provider = provider.into();
    options.model = model.into();
    options.max_tokens = 4096;
    options.budget.as_mut().unwrap().max_cost_usd = Some(0.5);
    options
}

#[test]
fn direct_hosted_token_routes_reserve_full_context_and_settle_reported_usage() {
    for (provider, model) in ROUTES {
        swap_scope(AdmissionScope::default());
        let options = options(provider, model);
        let request = LlmRequestPayload::from(&options);
        let bound = AttemptBound::for_request(&request).unwrap();
        let catalog_id = crate::llm_config::model_catalog_id_for_route(provider, model).unwrap();
        let row = crate::llm_config::model_catalog_entry(&catalog_id).unwrap();
        let pricing = row.pricing.unwrap();
        let full_context = row
            .context_window
            .max(row.runtime_context_window.unwrap_or(0));
        let expected = (Decimal::from(full_context) * money(pricing.input_per_mtok).unwrap()
            + Decimal::from(4096) * money(pricing.output_per_mtok).unwrap())
            / Decimal::from(1_000_000);
        assert!(
            bound.total() >= expected,
            "never substitute a prompt estimate"
        );
        let hold = reserve(&options, &request).unwrap().unwrap();
        let scope = SCOPE.with(|slot| slot.borrow().clone());
        let reserved = scope.ledger.lock().unwrap().in_flight;
        assert_eq!(reserved, bound.total());
        assert!(reserved > money(0.01).unwrap() && reserved <= money(0.5).unwrap());
        let mut response = result(8, 4);
        response.provider = provider.into();
        response.model = model.into();
        hold.settle(&response).unwrap();
        let ledger = scope.ledger.lock().unwrap();
        assert_eq!(ledger.in_flight, Decimal::ZERO);
        assert_eq!(ledger.uncertain, Decimal::ZERO);
        assert!(ledger.settled_upper > Decimal::ZERO && ledger.settled_upper < reserved);
    }
}

#[test]
fn direct_contract_does_not_admit_compatible_aggregators_or_unknown_routes() {
    for provider in ["together", "openrouter", "custom", "groq-compatible"] {
        swap_scope(AdmissionScope::default());
        let options = options(provider, ROUTES[0].1);
        let error = reserve(&options, &LlmRequestPayload::from(&options))
            .err()
            .unwrap();
        assert!(error.to_string().contains("unsupported_billing_shape"));
    }
    for (provider, _) in ROUTES {
        swap_scope(AdmissionScope::default());
        let options = options(provider, "unregistered-model");
        let error = reserve(&options, &LlmRequestPayload::from(&options))
            .err()
            .unwrap();
        assert!(error.to_string().contains("unknown_pricing"));
    }
}

#[test]
fn direct_provider_usage_must_be_complete_consistent_and_exact_route() {
    for (provider, model) in ROUTES {
        let bound =
            AttemptBound::for_request(&LlmRequestPayload::from(&options(provider, model))).unwrap();
        let mut response = result(8, 4);
        response.provider = provider.into();
        response.model = model.into();
        for kind in [
            "missing_input",
            "missing_output",
            "input_mismatch",
            "output_mismatch",
            "negative",
            "cache",
            "total",
            "route",
            "provider",
            "premium",
        ] {
            let mut bad = response.clone();
            match kind {
                "missing_input" => bad.telemetry.server_prompt_tokens = None,
                "missing_output" => bad.telemetry.server_output_tokens = None,
                "input_mismatch" => bad.input_tokens += 1,
                "output_mismatch" => bad.output_tokens += 1,
                "negative" => bad.telemetry.server_output_tokens = Some(-1),
                "cache" => bad.cache_read_tokens = 9,
                "total" => bad.telemetry.server_total_tokens = Some(1),
                "route" => bad.model = "unregistered-model".into(),
                "provider" => bad.provider = "openai".into(),
                "premium" => bad.served_fast = true,
                _ => unreachable!(),
            }
            assert!(bound.observed_upper(&bad).is_none(), "{provider} {kind}");
            if matches!(kind, "route" | "provider") {
                assert!(bound.known_contract_violation(&bad));
            }
        }
    }
}

struct ResetEndpoints;
impl Drop for ResetEndpoints {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
    }
}

#[test]
fn direct_contract_rejects_custom_effective_endpoints_and_dialects() {
    let _env = crate::llm::test_env::env_guard();
    let _reset = ResetEndpoints;
    for (provider, model) in ROUTES {
        for endpoint in ["https://unapproved.example/v1", "http://127.0.0.1:1/v1"] {
            crate::llm_config::set_runtime_provider_endpoint_overrides(
                crate::llm_config::RuntimeProviderEndpointOverrides::single(provider, endpoint)
                    .unwrap(),
            );
            let request = LlmRequestPayload::from(&options(provider, model));
            assert!(AttemptBound::for_request(&request).is_err());
            crate::llm_config::clear_runtime_provider_endpoint_overrides();
        }
        let mut definition = crate::llm_config::provider_config(provider).unwrap();
        definition.base_url = "https://unapproved.example/v1".into();
        definition.base_url_env = None;
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.providers.insert(provider.into(), definition);
        crate::llm_config::set_user_overrides(Some(overlay));
        assert!(
            AttemptBound::for_request(&LlmRequestPayload::from(&options(provider, model))).is_err()
        );
        crate::llm_config::clear_user_overrides();
        let mut request = LlmRequestPayload::from(&options(provider, model));
        request.api_mode = crate::llm::api::LlmApiMode::Responses;
        assert!(AttemptBound::for_request(&request).is_err());
    }
    let _custom_env =
        crate::llm::test_env::ScopedEnvVar::set("GROQ_BASE_URL", "https://unapproved.example/v1");
    assert!(AttemptBound::for_request(&LlmRequestPayload::from(&options(
        ROUTES[1].0,
        ROUTES[1].1
    )))
    .is_err());
}

#[test]
fn direct_contract_rejects_dedicated_rows_with_retained_token_prices() {
    let model = "accounts/fireworks/models/nemotron-lightning-3p5-30b-a3b";
    let row = crate::llm_config::model_catalog_entry(model).unwrap();
    assert_eq!(
        row.availability,
        crate::llm_config::ModelAvailability::Dedicated
    );
    assert!(
        row.pricing.is_some(),
        "known token prices are insufficient for dedicated billing"
    );
    let request = LlmRequestPayload::from(&options("fireworks", model));
    let error = AttemptBound::for_request(&request).err().unwrap();
    assert!(error.to_string().contains("unsupported_billing_shape"));
}
