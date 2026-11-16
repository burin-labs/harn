use super::Language;

pub(super) const INLINE_RECORDS: &[&str] = &[
    "/$defs/data_controls/properties/request_controls/items",
    "/$defs/alias/properties/tool_calling",
    "/$defs/pricing/properties/hosted_tool_fees/additionalProperties",
    "/$defs/pricing/properties/modality_rates",
    "/$defs/pricing/properties/input_token_bands/items",
];

// Public language names and decoder policy are projections; field shapes belong to the schema.
pub(super) fn inline_record(path: &str, language: Language) -> bool {
    language == Language::TypeScript
        && matches!(
            path,
            "/$defs/modalities"
                | "/$defs/tool_support"
                | "/$defs/format_preferences"
                | "/$defs/reasoning"
                | "/$defs/deprecation"
        )
}

pub(super) fn named_record(path: &str, language: Language) -> Option<&'static str> {
    if inline_record(path, language) {
        return None;
    }
    Some(match path {
        "" => "HarnProviderCatalog",
        "/$defs/automatic_default" => "HarnCatalogAutomaticDefault",
        "/$defs/portal_default" => "HarnCatalogPortalDefault",
        "/$defs/provider" => "HarnCatalogProvider",
        "/$defs/data_controls" => "HarnProviderDataControls",
        "/$defs/model_data_controls" => "HarnModelDataControls",
        "/$defs/data_controls/properties/request_controls/items" => "HarnProviderDataControl",
        "/$defs/healthcheck" => "HarnProviderHealthcheck",
        "/$defs/local_runtime" => "HarnLocalRuntime",
        "/$defs/endpoint" => "HarnProviderEndpoint",
        "/$defs/endpoint_region" => "HarnProviderEndpointRegion",
        "/$defs/auth" => "HarnProviderAuth",
        "/$defs/alias" => "HarnCatalogAlias",
        "/$defs/alias/properties/tool_calling" => "HarnAliasToolCalling",
        "/$defs/model" => "HarnCatalogModel",
        "/$defs/model_batch_support" => "HarnModelBatchSupport",
        "/$defs/modalities" => "HarnModelModalities",
        "/$defs/tool_support" => "HarnModelToolSupport",
        "/$defs/tool_empirical_parity" => "HarnToolEmpiricalParity",
        "/$defs/format_preferences" => "HarnModelFormatPreferences",
        "/$defs/reasoning" => "HarnModelReasoning",
        "/$defs/pricing" => "HarnModelPricing",
        "/$defs/pricing/properties/hosted_tool_fees/additionalProperties" => "HarnHostedToolFee",
        "/$defs/pricing/properties/modality_rates" => "HarnModalityRates",
        "/$defs/recurring_pricing_window" => "HarnRecurringPricingWindow",
        "/$defs/promotional_pricing" => "HarnPromotionalPricing",
        "/$defs/pricing/properties/input_token_bands/items" => "HarnInputTokenPricingBand",
        "/$defs/rate_limits" => "HarnRateLimits",
        "/$defs/performance" => "HarnServingPerformance",
        "/$defs/architecture" => "HarnModelArchitecture",
        "/$defs/local_memory" => "HarnLocalMemory",
        "/$defs/deprecation" => "HarnModelDeprecation",
        "/$defs/completion_review" => "HarnCompletionReview",
        "/$defs/reasoning_mode_request" => "HarnModelReasoningModeRequest",
        "/$defs/reasoning_mode" => "HarnModelReasoningMode",
        "/$defs/serving_tier_request" => "HarnModelServingTierRequest",
        "/$defs/serving_tier" => "HarnModelServingTier",
        "/$defs/variant" => "HarnCatalogVariant",
        "/$defs/automatic_model_eligibility" => "HarnAutomaticModelEligibility",
        "/$defs/model_eligibility_measurement" => "HarnModelEligibilityMeasurement",
        "/$defs/model_family" => "HarnCatalogModelFamily",
        "/$defs/model_family_dimension" => "HarnModelFamilyDimension",
        "/$defs/model_family_value" => "HarnModelFamilyValue",
        "/$defs/model_family_preset" => "HarnModelFamilyPreset",
        "/$defs/routing_route" => "HarnCatalogRoutingRoute",
        _ => return None,
    })
}

pub(super) fn swift_field(record: &str, wire: &str) -> String {
    let renamed = match (record, wire) {
        ("/$defs/provider", "cache_usage_accounting") => Some("encodedCacheUsageAccounting"),
        ("/$defs/provider", "stream_usage_accounting") => Some("encodedStreamUsageAccounting"),
        ("/$defs/automatic_default" | "/$defs/portal_default", "model_id") => Some("modelId"),
        (_, "model_id") => Some("modelID"),
        ("/$defs/pricing/properties/hosted_tool_fees/additionalProperties", "source_url") => {
            Some("sourceUrl")
        }
        (_, "source_url") => Some("sourceURL"),
        (_, "base_url") => Some("baseURL"),
        (_, "base_url_env") => Some("baseURLEnv"),
        (_, "chat_api_adapter") => Some("chatAPIAdapter"),
        (_, "prefers_xml_scaffolding") => Some("prefersXMLScaffolding"),
        (_, "prefers_xml_tools") => Some("prefersXMLTools"),
        (_, "measured_resident_gib") => Some("measuredResidentGiB"),
        (_, "base_resident_gib") => Some("baseResidentGiB"),
        (_, "safety_margin_gib") => Some("safetyMarginGiB"),
        (_, "kv_cache_gib_per_1k_ctx") => Some("kvCacheGiBPer1KContext"),
        _ => None,
    };
    if let Some(name) = renamed {
        return name.into();
    }
    let mut parts = wire.split('_');
    let mut name = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        if part == "mtok" {
            name.push_str("MTok");
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
        }
        name.push_str(chars.as_str());
    }
    name
}

pub(super) fn swift_default(record: &str, wire: &str) -> Option<&'static str> {
    match (record, wire) {
        ("/$defs/model", "display_name") => Some("name"),
        ("/$defs/model", "benchmarks") => Some("[:]"),
        (
            "/$defs/model",
            "complementary_with"
            | "avoid_as_reviewer_for"
            | "strengths"
            | "serving_tiers"
            | "reasoning_modes",
        ) => Some("[]"),
        ("/$defs/completion_review", "scrutiny") => Some("\"standard\""),
        (
            "/$defs/reasoning_mode" | "/$defs/serving_tier",
            "suitable_workloads" | "unsuitable_workloads",
        ) => Some("[]"),
        _ => None,
    }
}
