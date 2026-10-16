use super::*;

#[test]
fn october_refresh_preserves_route_capabilities_and_access_limits() {
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    for (provider, id, window, native) in [
        ("openrouter", "meta/muse-spark-open", 1048576, true),
        (
            "fireworks",
            "accounts/fireworks/models/muse-spark-open",
            1048576,
            true,
        ),
        (
            "together",
            "together/meta-models/Muse-Spark-Open-816B-A42B",
            1048576,
            true,
        ),
        (
            "huggingface",
            "huggingface/meta-models/Muse-Spark-Open-816B-A42B",
            1048576,
            true,
        ),
        (
            "vercel_ai_gateway",
            "vercel/meta/muse-spark-open",
            1048576,
            true,
        ),
        ("groq", "groq/meta-models/muse-spark-open", 131072, false),
        (
            "baseten",
            "baseten/meta-models/Muse-Spark-Open-816B-A42B",
            1048576,
            false,
        ),
    ] {
        let row = config.models.get(id).expect(id);
        assert_eq!(row.provider, provider, "{id}");
        assert_eq!(row.context_window, window, "{id}");
        let caps = view.capabilities(provider, id).capabilities();
        assert_eq!(caps.native_tools, native, "{id}");
        assert!(!caps.reasoning_disable_supported, "{id}");
        assert!(caps.vision_supported, "{id}");
    }
    // Inference accepts shaped names even without a row. These controls pin
    // catalog membership and a real older generation's different contract.
    assert!(!config.models.contains_key("meta/muse-spark-open-2"));
    assert!(!config.models.contains_key("muse-wonder-full"));
    let old = view.capabilities("meta", "muse-spark-1.3").capabilities();
    assert!(!old.reasoning_effort_supported);
    let argon = view.capabilities("gemini", "gemini-4-argon").capabilities();
    assert_eq!(
        argon.live_endpoint_family,
        Some(crate::capabilities::LiveEndpointFamily::GeminiInteractions)
    );
    assert_eq!(argon.reasoning_effort_levels, ["low", "medium", "high"]);
    let qwen = view
        .capabilities("openrouter", "qwen/qwen3.8-max-prime")
        .capabilities();
    assert_eq!(qwen.allowed_tool_choice_modes, ["auto"]);
    let mercury = view
        .capabilities("openrouter", "inception/mercury-2.5")
        .capabilities();
    assert!(!mercury.native_tools);
    let promotion = config.models["gemini-4-argon"].pricing.as_ref().unwrap();
    assert_eq!(promotion.input_per_mtok, 4.0);
    assert_eq!(
        promotion.promotions[0].ends_on.as_deref(),
        Some("2026-11-30")
    );
    for (month, day, input, output, cache) in [
        (10, 12, 4.0, 20.0, 0.20),
        (10, 13, 2.0, 10.0, 0.10),
        (11, 30, 2.0, 10.0, 0.10),
        (12, 1, 4.0, 20.0, 0.20),
    ] {
        let card = promotion.effective_on(NaiveDate::from_ymd_opt(2026, month, day).unwrap());
        assert_eq!(card.input_per_mtok, input);
        assert_eq!(card.output_per_mtok, output);
        assert_eq!(card.cache_read_per_mtok, Some(cache));
    }
}

#[test]
fn hosted_family_refresh_reaches_native_rules_instead_of_generic_fallback() {
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    for id in [
        "unbiased/pareto-26.9",
        "perceptron/perceptron-mk1.5",
        "aion-labs/aion-3.5",
        "upstage/solar-mini4",
        "upstage/solar-pro4",
        "xiaomi/mimo-v2.6-pro-ultraspeed",
        "qwen/qwen3.8-omni-flash",
        "qwen/qwen3.8-flash",
        "qwen/qwen3.8-2.4t-a95b",
        "tencent/hy4-preview",
        "nex-agi/nex-n2.5-pro",
        "ibm-granite/granite-4.2-8b",
        "meta/muse-spark-1.3",
        "bytedance-seed/seed-2-1-turbo",
    ] {
        assert_eq!(config.models[id].provider, "openrouter", "{id}");
        let caps = view.capabilities("openrouter", id).capabilities();
        assert!(caps.native_tools, "{id}");
        assert_eq!(
            caps.preferred_tool_format.as_deref(),
            Some("native"),
            "{id}"
        );
    }
    assert!(!config.models.contains_key("qwen/qwen3.9-flash"));
    // Generic Qwen rules also match shaped names with no catalog row. Pin
    // the new route's contrasting restriction rather than inference alone.
    let flash = view
        .capabilities("openrouter", "qwen/qwen3.8-flash")
        .capabilities();
    assert_eq!(flash.allowed_tool_choice_modes, ["auto"]);
    assert!(flash.vision_supported);
}

#[test]
fn native_decision_refresh_never_admits_chat_and_keeps_selector_identity() {
    use crate::config::ModelOperation;
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    let mut measured = 0;
    for (id, row) in &config.models {
        if row.normalized_operations() != [ModelOperation::Decision] {
            continue;
        }
        measured += 1;
        assert!(
            !row.supports_operation(ModelOperation::TextGeneration),
            "{id}"
        );
        let caps = view.capabilities(&row.provider, id).capabilities();
        let contract = decision_contract::resolved_decision_contract(id, row, &caps).expect(id);
        assert!(contract.protocol.is_native(), "{id}");
        assert_eq!(contract.output_price_per_mtok, Some(0.0), "{id}");
    }
    assert!(measured >= 30, "native decision census: {measured}");
    assert_eq!(
        config.models["typesafe/jev-latest"]
            .current_snapshot
            .as_deref(),
        Some("typesafe/jev-1.13.0")
    );
    assert_eq!(
        config.models["typesafe/jev-preview"]
            .current_snapshot
            .as_deref(),
        Some("typesafe/jev-1.14.0")
    );
    assert_eq!(
        config.models["openrouter/typesafe/jev-latest"]
            .current_snapshot
            .as_deref(),
        Some("openrouter/typesafe/jev-1.14")
    );
    assert!(!config.models.contains_key("typesafe/jev-1.15.0"));
    for (id, choices) in [
        ("openrouter/togethercomputer/tev1-4b-experimental", 20),
        ("openrouter/upstage/solar-decide", 26),
        ("openrouter/upstage/solar-decide-flash", 26),
    ] {
        assert_eq!(
            view.capabilities("openrouter", id)
                .capabilities()
                .decision_limits
                .unwrap()
                .max_choice_options,
            choices,
            "{id}"
        );
    }
    let mut laya = view
        .capabilities("vercel_ai_gateway", "vercel/convaiinnovations/laya")
        .capabilities();
    assert_eq!(laya.decision_limits.unwrap().max_choice_options, 64);
    assert_eq!(
        laya.decision_upstream_provider.as_deref(),
        Some("boundless")
    );
    laya.decision_upstream_provider = None;
    assert!(decision_contract::resolved_decision_contract(
        "vercel/convaiinnovations/laya",
        &config.models["vercel/convaiinnovations/laya"],
        &laya,
    )
    .is_none());
    let jev = view
        .capabilities("vercel_ai_gateway", "vercel/typesafe-ai/jev")
        .capabilities();
    assert_eq!(
        jev.decision_upstream_provider.as_deref(),
        Some("typesafe-ai")
    );
}
