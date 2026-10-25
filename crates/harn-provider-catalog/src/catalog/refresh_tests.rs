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

#[test]
fn readonly_october_routes_do_not_inherit_verified_family_capabilities() {
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    for (id, provider, wire) in [
        (
            "accounts/fireworks/models/deepseek-v4p1-flash-1017",
            "fireworks",
            "accounts/fireworks/models/deepseek-v4p1-flash-1017",
        ),
        (
            "accounts/fireworks/models/minimax-m3p1-flash",
            "fireworks",
            "accounts/fireworks/models/minimax-m3p1-flash",
        ),
        (
            "minimax/minimax-m3.1-flash:free",
            "openrouter",
            "minimax/minimax-m3.1-flash:free",
        ),
        (
            "deepseek/deepseek-v4.1-flash-1017",
            "openrouter",
            "deepseek/deepseek-v4.1-flash-1017",
        ),
        ("elorian/glimpse", "openrouter", "elorian/glimpse"),
        (
            "xingchen-agi/xing4.0-29b-a4b:free",
            "openrouter",
            "xingchen-agi/xing4.0-29b-a4b:free",
        ),
        (
            "reflection/beam-1.0-501b-a23b",
            "openrouter",
            "reflection/beam-1.0-501b-a23b",
        ),
        ("inception/mercury-3", "openrouter", "inception/mercury-3"),
        (
            "unbiased/pareto-26.10",
            "openrouter",
            "unbiased/pareto-26.10",
        ),
        (
            "vercel/minimax/minimax-m3.1-flash",
            "vercel_ai_gateway",
            "minimax/minimax-m3.1-flash",
        ),
        (
            "vercel/deepseek/deepseek-v4.1-flash-1017",
            "vercel_ai_gateway",
            "deepseek/deepseek-v4.1-flash-1017",
        ),
        (
            "vercel/deepseek/deepseek-v4.1-flash-1017-fast",
            "vercel_ai_gateway",
            "deepseek/deepseek-v4.1-flash-1017-fast",
        ),
        (
            "vercel/inception/mercury-3",
            "vercel_ai_gateway",
            "inception/mercury-3",
        ),
        (
            "vercel/elorian/glimpse",
            "vercel_ai_gateway",
            "elorian/glimpse",
        ),
        ("MiniMax-M3.1-Flash", "minimax", "MiniMax-M3.1-Flash"),
        ("mercury-3", "inception", "mercury-3"),
        (
            "together/deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "together",
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        ),
        (
            "deepinfra/deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "deepinfra",
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        ),
        (
            "huggingface/deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "huggingface",
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        ),
        (
            "huggingface/reflection/Beam-1.0-501B-A23B",
            "huggingface",
            "reflection/Beam-1.0-501B-A23B",
        ),
    ] {
        let row = config.models.get(id).expect(id);
        assert_eq!(row.provider, provider, "{id}");
        assert_eq!(row.wire_model.as_deref().unwrap_or(id), wire, "{id}");
        assert_eq!(
            row.availability,
            crate::config::ModelAvailability::Unknown,
            "{id}"
        );
        assert!(row.capabilities.is_empty(), "{id}");
        let resolution = view.capabilities(provider, id);
        assert!(resolution.matched(), "{id}");
        let caps = resolution.capabilities();
        // The one route a live tool call has since verified (2026-10-25).
        let probed = id == "vercel/deepseek/deepseek-v4.1-flash-1017";
        assert_eq!(caps.native_tools, probed, "{id}");
        // Only native calls were probed, so text tools stay undeclared everywhere.
        assert!(!caps.text_tool_wire_format_supported, "{id}");
        assert!(!caps.vision_supported, "{id}");
        assert!(!caps.reasoning_effort_supported, "{id}");
        assert!(!caps.reasoning_disable_supported, "{id}");
    }
    // Shape-based provider inference is not proof of catalog membership.
    for id in [
        "MiniMax-M3.2-Flash",
        "minimax/minimax-m3.1-flash",
        "deepseek-v4.1-flash-1017",
        "deepseek/deepseek-v4.1-flash-1018",
        "inception/mercury-4",
    ] {
        assert!(!config.models.contains_key(id), "{id}");
    }
    // Older measured routes retain their contrasting native-tool contract.
    assert!(
        view.capabilities("minimax", "MiniMax-M3")
            .capabilities()
            .native_tools
    );
    assert!(
        view.capabilities("openrouter", "unbiased/pareto-26.9")
            .capabilities()
            .native_tools
    );
    assert_eq!(config.aliases["minimax"].id, "MiniMax-M3");
    assert_eq!(config.aliases["minimax31"].id, "MiniMax-M3.1-Flash");
    assert_eq!(config.aliases["mercury3"].id, "mercury-3");
    assert!(!config.models["MiniMax-M3.1-Flash"].open_weight.unwrap());
    let minimax = view
        .capabilities("minimax", "MiniMax-M3.1-Flash")
        .capabilities();
    assert!(!minimax.temperature_supported);
    assert!(!minimax.top_p_supported);
    assert!(minimax.requires_completion_tokens);
}

#[test]
fn readonly_fireworks_routes_do_not_inherit_provider_batch_support() {
    let catalog = artifact();
    for id in [
        "accounts/fireworks/models/deepseek-v4p1-flash-1017",
        "accounts/fireworks/models/minimax-m3p1-flash",
    ] {
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == id)
            .expect(id);
        assert!(
            !model.capability_tags.iter().any(|tag| tag == "batch"),
            "{id}"
        );
        assert!(model.batch.is_none(), "{id}");
    }
    // Keep the established provider fallback for routes without an opt-out.
    let older = catalog
        .models
        .iter()
        .find(|model| model.id == "accounts/fireworks/models/gpt-oss-120b")
        .expect("existing Fireworks route");
    assert!(older.capability_tags.iter().any(|tag| tag == "batch"));
    assert!(older.batch.is_some());
    assert!(!catalog
        .models
        .iter()
        .any(|model| { model.id == "accounts/fireworks/models/deepseek-v4p1-flash-1018" }));
}

#[test]
fn readonly_refresh_keeps_peak_prices_distinct_from_launch_and_offpeak_quotes() {
    let config = llm_config::packaged_config();
    for (id, input, output, cache) in [
        ("deepseek/deepseek-v4.1-flash-1017", 0.30, 1.20, 0.006),
        (
            "vercel/deepseek/deepseek-v4.1-flash-1017",
            0.15,
            0.60,
            0.003,
        ),
    ] {
        let price = config.models[id].pricing.as_ref().unwrap();
        for (at, multiplier) in [
            ("2026-10-21T00:59:00Z", 0.5),
            ("2026-10-21T01:00:00Z", 1.0),
            ("2026-10-21T04:00:00Z", 0.5),
            ("2026-10-21T06:00:00Z", 1.0),
            ("2026-10-21T10:00:00Z", 0.5),
            ("2026-10-24T02:00:00Z", 0.5),
        ] {
            let card = price.effective_at(OffsetDateTime::parse(at, &Rfc3339).unwrap());
            assert_eq!(card.input_per_mtok, input * multiplier, "{id} {at}");
            assert_eq!(card.output_per_mtok, output * multiplier, "{id} {at}");
            assert_eq!(
                card.cache_read_per_mtok,
                Some(cache * multiplier),
                "{id} {at}"
            );
        }
    }
    let price = config.models["mercury-3"].pricing.as_ref().unwrap();
    assert_eq!(price.input_per_mtok, 0.20);
    let before = price.effective_on(NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
    let launch = price.effective_on(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap());
    assert_eq!(before.input_per_mtok, 0.20);
    assert_eq!(launch.input_per_mtok, 0.10);
    assert_eq!(launch.output_per_mtok, 0.35);
    assert_eq!(launch.cache_read_per_mtok, Some(0.01));
    assert!(price.promotions[0].ends_on.is_none());
    let fast = config.models["vercel/deepseek/deepseek-v4.1-flash-1017-fast"]
        .pricing
        .as_ref()
        .unwrap();
    assert!(fast.schedules.is_empty());
    assert_eq!(fast.input_per_mtok, 0.60);
    assert!(config.models["deepseek-ai/DeepSeek-V4.1-Flash"]
        .pricing
        .is_some());
}
