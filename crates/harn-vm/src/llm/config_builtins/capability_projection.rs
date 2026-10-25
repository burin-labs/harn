//! A `(provider, model)` capability row rendered as `VmValue` — the shape
//! scripts branch on to decide what a route supports.

use crate::llm_config;
use crate::schema::json_to_vm_value;
use crate::value::{VmDictExt, VmValue};

use super::batch_projection::{
    optional_batch_string, optional_batch_string_list, optional_batch_u32, optional_batch_u64,
    string_list_to_vm_value,
};
use super::catalog_projection::{
    insert_tool_mode_parity_fields, reasoning_history_wire_field_value, tools_value,
};

pub(crate) fn capabilities_to_vm_value(
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    let batch_api = crate::llm_config::effective_batch_api_supported(provider, caps);
    dict.put_str("provider", provider);
    dict.put_str("model", model);
    dict.put_bool("native_tools", caps.native_tools);
    dict.put_nullable(
        "runtime_probe",
        caps.runtime_probe.as_ref().map(runtime_probe_to_vm_value),
    );
    dict.put_str("message_wire_format", caps.message_wire_format.as_str());
    // Absent for every dialect that serves one live endpoint; only the Gemini
    // routes carry a value, so scripts can tell `:generateContent` from
    // Interactions without matching on model names.
    dict.put_nullable(
        "live_endpoint_family",
        caps.live_endpoint_family
            .map(|family| VmValue::string(family.as_str())),
    );
    // Decision request contract. Absent on every route that answers no
    // decision request, so a script can tell "this route cannot decide" from
    // "this route decides over some other protocol" without a name match.
    dict.put_nullable(
        "decision_protocol",
        caps.decision_protocol
            .map(|protocol| VmValue::string(protocol.as_str())),
    );
    dict.put(
        "decision_question_kinds",
        VmValue::strings(
            caps.decision_question_kinds
                .iter()
                .map(|kind| kind.as_str()),
        ),
    );
    dict.put_nullable_str(
        "decision_upstream_provider",
        caps.decision_upstream_provider.as_deref(),
    );
    dict.put_nullable(
        "decision_limits",
        caps.decision_limits.map(decision_limits_to_vm_value),
    );
    dict.put_str(
        "native_tool_wire_format",
        caps.native_tool_wire_format.clone(),
    );
    dict.put_bool(
        "text_tool_wire_format_supported",
        caps.text_tool_wire_format_supported,
    );
    dict.put_nullable_str(
        "preferred_tool_format",
        caps.preferred_tool_format.as_deref(),
    );
    insert_tool_mode_parity_fields(&mut dict, caps);
    dict.insert(crate::value::intern_key("tools"), tools_value(caps));
    dict.put_bool("defer_loading", caps.defer_loading);
    dict.insert(
        crate::value::intern_key("tool_search"),
        string_list_to_vm_value(caps.tool_search.clone()),
    );
    dict.put_bool("responses_api", caps.responses_api);
    dict.insert(
        crate::value::intern_key("hosted_tools"),
        string_list_to_vm_value(caps.hosted_tools.clone()),
    );
    dict.put_bool("remote_mcp", caps.remote_mcp);
    dict.put_bool("conversation_state", caps.conversation_state);
    dict.put_bool("compaction", caps.compaction);
    dict.put_bool("background_mode", caps.background_mode);
    insert_batch_support_fields(&mut dict, batch_api, caps);
    dict.put_nullable_str("tool_approval_policy", caps.tool_approval_policy.as_deref());
    dict.put_nullable("max_tools", caps.max_tools.map(|n| VmValue::Int(n as i64)));
    dict.put_bool("prompt_caching", caps.prompt_caching);
    dict.put_str(
        "cache_breakpoint_style",
        caps.cache_breakpoint_style.as_str(),
    );
    dict.put_str("reasoning_round_trip", caps.reasoning_round_trip.as_str());
    // Full cache-control profile (min useful prefix, TTL notes, usage-field
    // mapping) derived from this one capability path, so Burin dogfood (#3532)
    // and Harn Cloud receipts (#1106) read cache requirements without a
    // provider-specific branch of their own.
    let cache_profile = crate::llm::cache_conformance::CacheControlProfile::from_capabilities(caps);
    let mut cache_control = crate::value::DictMap::new();
    cache_control.put_bool("prompt_caching", cache_profile.prompt_caching);
    cache_control.put_str(
        "cache_breakpoint_style",
        &cache_profile.cache_breakpoint_style,
    );
    cache_control.put_nullable(
        "min_useful_prefix_tokens",
        cache_profile
            .min_useful_prefix_tokens
            .map(|n| VmValue::Int(n as i64)),
    );
    cache_control.put_nullable_str("ttl_notes", cache_profile.ttl_notes.as_deref());
    cache_control.insert(
        crate::value::intern_key("supported_ttls"),
        string_list_to_vm_value(cache_profile.supported_ttls),
    );
    cache_control.put_str(
        "cache_read_usage_field",
        &cache_profile.cache_read_usage_field,
    );
    cache_control.put_str(
        "cache_write_usage_field",
        &cache_profile.cache_write_usage_field,
    );
    dict.put("cache_control", VmValue::dict(cache_control));
    dict.put_bool("prefers_xml_scaffolding", caps.prefers_xml_scaffolding);
    dict.put_bool(
        "prefers_markdown_scaffolding",
        caps.prefers_markdown_scaffolding,
    );
    dict.put_str(
        "structured_output_mode",
        caps.structured_output_mode.as_str(),
    );
    dict.put_bool(
        "supports_assistant_prefill",
        caps.supports_assistant_prefill,
    );
    dict.put_bool("prefers_role_developer", caps.prefers_role_developer);
    dict.put_bool("prefers_xml_tools", caps.prefers_xml_tools);
    dict.put_bool("thinking", !caps.thinking_modes.is_empty());
    dict.put_str("thinking_block_style", caps.thinking_block_style.as_str());
    dict.insert(
        crate::value::intern_key("thinking_modes"),
        string_list_to_vm_value(caps.thinking_modes.clone()),
    );
    dict.put_bool(
        "interleaved_thinking_supported",
        caps.interleaved_thinking_supported,
    );
    dict.insert(
        crate::value::intern_key("anthropic_beta_features"),
        string_list_to_vm_value(caps.anthropic_beta_features.clone()),
    );
    dict.put_bool("vision_supported", caps.vision_supported);
    dict.put_bool("audio", caps.audio);
    dict.put_bool("pdf", caps.pdf);
    dict.put_bool("video", caps.video);
    dict.put_bool("files_api_supported", caps.files_api_supported);
    dict.put_nullable(
        "file_upload_wire_format",
        caps.file_upload_wire_format
            .as_ref()
            .map(|value| VmValue::string(value.clone())),
    );
    dict.put_nullable_str("structured_output", caps.structured_output.as_deref());
    dict.put(
        "structured_output_strategy",
        VmValue::string(caps.structured_output_strategy.as_str()),
    );
    dict.put_nullable_str("json_schema", caps.json_schema.as_deref());
    dict.put_bool("prefers_xml_scaffolding", caps.prefers_xml_scaffolding);
    dict.put_bool(
        "prefers_markdown_scaffolding",
        caps.prefers_markdown_scaffolding,
    );
    dict.put_str(
        "structured_output_mode",
        caps.structured_output_mode.as_str(),
    );
    dict.put_bool(
        "supports_assistant_prefill",
        caps.supports_assistant_prefill,
    );
    dict.put_bool("prefers_role_developer", caps.prefers_role_developer);
    dict.put_bool("prefers_xml_tools", caps.prefers_xml_tools);
    dict.put_str("thinking_block_style", caps.thinking_block_style.as_str());
    dict.put_bool("preserve_thinking", caps.preserve_thinking);
    dict.put_bool(
        "honors_preserve_thinking_kwarg",
        caps.honors_preserve_thinking_kwarg,
    );
    dict.put_bool(
        "requires_parallel_tool_calls_false",
        caps.requires_parallel_tool_calls_false,
    );
    dict.insert(
        crate::value::intern_key("reasoning_history_wire_field"),
        reasoning_history_wire_field_value(caps),
    );
    dict.put_bool(
        "requires_completion_tokens",
        caps.requires_completion_tokens,
    );
    dict.put_bool(
        "chat_completions_unsupported",
        caps.chat_completions_unsupported,
    );
    dict.put_bool("requires_streaming", caps.requires_streaming);
    dict.put_bool(
        "reasoning_effort_supported",
        caps.reasoning_effort_supported,
    );
    dict.insert(
        crate::value::intern_key("reasoning_effort_levels"),
        string_list_to_vm_value(caps.reasoning_effort_levels.clone()),
    );
    dict.put_bool("reasoning_none_supported", caps.reasoning_none_supported);
    dict.put_bool(
        "reasoning_disable_supported",
        caps.reasoning_disable_supported,
    );
    dict.put_str("thinking_off_type", caps.thinking_off_type.as_str());
    dict.put_bool("reasoning_text_promotable", caps.reasoning_text_promotable);
    dict.put_nullable(
        "reasoning_wire_format",
        caps.reasoning_wire_format
            .as_ref()
            .map(|value| VmValue::string(value.clone())),
    );
    dict.insert(
        crate::value::intern_key("reasoning_excluded_portable_options"),
        string_list_to_vm_value(
            caps.reasoning_excluded_portable_options
                .iter()
                .map(|option| option.name().to_string())
                .collect(),
        ),
    );
    dict.put_bool("seed_supported", caps.seed_supported);
    dict.insert(
        crate::value::intern_key("portable_option_qualification"),
        json_to_vm_value(
            &serde_json::to_value(caps.portable_option_qualification)
                .expect("typed option qualification is serializable"),
        ),
    );
    dict.put_bool("top_k_supported", caps.top_k_supported);
    dict.insert(
        crate::value::intern_key("portable_option_effort_support"),
        json_to_vm_value(
            &serde_json::to_value(&caps.portable_option_effort_support)
                .expect("typed option effort requirements are serializable"),
        ),
    );
    dict.put_bool("temperature_supported", caps.temperature_supported);
    dict.put_bool("top_p_supported", caps.top_p_supported);
    dict.put_bool(
        "frequency_penalty_supported",
        caps.frequency_penalty_supported,
    );
    dict.put_bool(
        "presence_penalty_supported",
        caps.presence_penalty_supported,
    );
    dict.put_bool("stop_supported", caps.stop_supported);
    dict.put(
        "advanced_generation_options",
        VmValue::strings(
            caps.advanced_generation_options
                .iter()
                .map(|option| option.name()),
        ),
    );
    dict.put(
        "allowed_tool_choice_modes",
        VmValue::strings(
            caps.allowed_tool_choice_modes
                .iter()
                .map(|mode| mode.as_str()),
        ),
    );
    dict.put(
        "auto_reasoning_overrides",
        VmValue::dict(caps.auto_reasoning_overrides.iter().map(|(task, mode)| {
            (
                crate::value::intern_key(task),
                VmValue::string(mode.clone()),
            )
        })),
    );
    // Accelerated-serving (`fast`) tier, read from the generalized
    // `serving_tiers` catalog so callers can branch on `harness.llm.call(...,
    // { fast: true })` support without re-parsing the model row.
    let fast_tier = crate::llm::serving_tiers::fast_tier(model);
    let fast_tier_supported = matches!(
        crate::llm::serving_tiers::fast_gate(model),
        crate::llm::serving_tiers::ServingTierGate::Usable
    );
    dict.put_bool("fast_tier_supported", fast_tier_supported);
    if let Some(fast_tier) = fast_tier {
        dict.insert(
            crate::value::intern_key("fast_tier"),
            serving_tier_to_vm_value(&fast_tier),
        );
    }
    VmValue::dict(dict)
}

fn runtime_probe_to_vm_value(
    receipt: &crate::llm::capabilities::CapabilityProbeReceipt,
) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("status", receipt.status.as_str());
    dict.put_str("endpoint", &receipt.endpoint);
    dict.put_str("detail", &receipt.detail);
    dict.put_nullable("native_tools", receipt.native_tools.map(VmValue::Bool));
    dict.put_nullable(
        "supports_parallel_tool_calls",
        receipt.supports_parallel_tool_calls.map(VmValue::Bool),
    );
    VmValue::dict(dict)
}

fn batch_support_to_vm_value(
    batch_api: bool,
    caps: &crate::llm::capabilities::Capabilities,
) -> VmValue {
    if !batch_api {
        return VmValue::Nil;
    }
    let (Some(wire_format), Some(input_mode)) = (
        caps.batch_wire_format.as_deref(),
        caps.batch_input_mode.as_deref(),
    ) else {
        return VmValue::Nil;
    };
    let mut dict = crate::value::DictMap::new();
    dict.put_bool("api", true);
    dict.put_str("wire_format", wire_format);
    dict.put_str("input_mode", input_mode);
    dict.insert(
        crate::value::intern_key("discount_percent"),
        optional_batch_u32(batch_api, caps.batch_discount_percent),
    );
    dict.insert(
        crate::value::intern_key("turnaround_hours"),
        optional_batch_u32(batch_api, caps.batch_turnaround_hours),
    );
    dict.insert(
        crate::value::intern_key("max_requests"),
        optional_batch_u64(batch_api, caps.batch_max_requests),
    );
    dict.insert(
        crate::value::intern_key("max_input_bytes"),
        optional_batch_u64(batch_api, caps.batch_max_input_bytes),
    );
    dict.insert(
        crate::value::intern_key("result_retention_days"),
        optional_batch_u32(batch_api, caps.batch_result_retention_days),
    );
    dict.insert(
        crate::value::intern_key("result_ordering"),
        optional_batch_string(batch_api, caps.batch_result_ordering.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("partial_failure"),
        optional_batch_string(batch_api, caps.batch_partial_failure.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("cancellation"),
        optional_batch_string(batch_api, caps.batch_cancellation.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("security_notes"),
        optional_batch_string_list(batch_api, &caps.batch_security_notes),
    );
    dict.insert(
        crate::value::intern_key("operational_notes"),
        optional_batch_string_list(batch_api, &caps.batch_operational_notes),
    );
    dict.insert(
        crate::value::intern_key("regions"),
        optional_batch_string_list(batch_api, &caps.batch_regions),
    );
    VmValue::dict(dict)
}

pub(super) fn insert_batch_support_fields(
    dict: &mut crate::value::DictMap,
    batch_api: bool,
    caps: &crate::llm::capabilities::Capabilities,
) {
    dict.put_bool("batch_api", batch_api);
    dict.insert(
        crate::value::intern_key("batch_wire_format"),
        optional_batch_string(batch_api, caps.batch_wire_format.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("batch_input_mode"),
        optional_batch_string(batch_api, caps.batch_input_mode.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("batch_discount_percent"),
        optional_batch_u32(batch_api, caps.batch_discount_percent),
    );
    dict.insert(
        crate::value::intern_key("batch_turnaround_hours"),
        optional_batch_u32(batch_api, caps.batch_turnaround_hours),
    );
    dict.insert(
        crate::value::intern_key("batch_max_requests"),
        optional_batch_u64(batch_api, caps.batch_max_requests),
    );
    dict.insert(
        crate::value::intern_key("batch_max_input_bytes"),
        optional_batch_u64(batch_api, caps.batch_max_input_bytes),
    );
    dict.insert(
        crate::value::intern_key("batch_result_retention_days"),
        optional_batch_u32(batch_api, caps.batch_result_retention_days),
    );
    dict.insert(
        crate::value::intern_key("batch_result_ordering"),
        optional_batch_string(batch_api, caps.batch_result_ordering.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("batch_partial_failure"),
        optional_batch_string(batch_api, caps.batch_partial_failure.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("batch_cancellation"),
        optional_batch_string(batch_api, caps.batch_cancellation.as_deref()),
    );
    dict.insert(
        crate::value::intern_key("batch_security_notes"),
        optional_batch_string_list(batch_api, &caps.batch_security_notes),
    );
    dict.insert(
        crate::value::intern_key("batch_operational_notes"),
        optional_batch_string_list(batch_api, &caps.batch_operational_notes),
    );
    dict.insert(
        crate::value::intern_key("batch_regions"),
        optional_batch_string_list(batch_api, &caps.batch_regions),
    );
    dict.insert(
        crate::value::intern_key("batch"),
        batch_support_to_vm_value(batch_api, caps),
    );
}

fn serving_tier_to_vm_value(tier: &llm_config::ServingTierDef) -> VmValue {
    json_to_vm_value(&serde_json::to_value(tier).unwrap_or_else(|_| serde_json::json!({})))
}

/// Declared decision ceilings as a nested dict. `max_questions` stays absent
/// when the provider publishes no question-count ceiling, so a script can tell
/// "no published limit" from a number someone invented.
fn decision_limits_to_vm_value(limits: crate::llm::capabilities::DecisionLimits) -> VmValue {
    json_to_vm_value(&serde_json::to_value(limits).unwrap_or_else(|_| serde_json::json!({})))
}
