//! A `(provider, model)` capability row rendered as `VmValue` — the shape
//! scripts branch on to decide what a route supports.

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
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
    dict.insert(
        crate::value::intern_key("native_tools"),
        VmValue::Bool(caps.native_tools),
    );
    dict.put_str("message_wire_format", caps.message_wire_format.as_str());
    dict.put_str(
        "native_tool_wire_format",
        caps.native_tool_wire_format.clone(),
    );
    dict.insert(
        crate::value::intern_key("text_tool_wire_format_supported"),
        VmValue::Bool(caps.text_tool_wire_format_supported),
    );
    dict.insert(
        crate::value::intern_key("preferred_tool_format"),
        caps.preferred_tool_format
            .as_deref()
            .map(|format| VmValue::String(arcstr::ArcStr::from(format)))
            .unwrap_or(VmValue::Nil),
    );
    insert_tool_mode_parity_fields(&mut dict, caps);
    dict.insert(crate::value::intern_key("tools"), tools_value(caps));
    dict.insert(
        crate::value::intern_key("defer_loading"),
        VmValue::Bool(caps.defer_loading),
    );
    dict.insert(
        crate::value::intern_key("tool_search"),
        string_list_to_vm_value(caps.tool_search.clone()),
    );
    dict.insert(
        crate::value::intern_key("responses_api"),
        VmValue::Bool(caps.responses_api),
    );
    dict.insert(
        crate::value::intern_key("hosted_tools"),
        string_list_to_vm_value(caps.hosted_tools.clone()),
    );
    dict.insert(
        crate::value::intern_key("remote_mcp"),
        VmValue::Bool(caps.remote_mcp),
    );
    dict.insert(
        crate::value::intern_key("conversation_state"),
        VmValue::Bool(caps.conversation_state),
    );
    dict.insert(
        crate::value::intern_key("compaction"),
        VmValue::Bool(caps.compaction),
    );
    dict.insert(
        crate::value::intern_key("background_mode"),
        VmValue::Bool(caps.background_mode),
    );
    insert_batch_support_fields(&mut dict, batch_api, caps);
    dict.insert(
        crate::value::intern_key("tool_approval_policy"),
        caps.tool_approval_policy
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("max_tools"),
        caps.max_tools
            .map(|n| VmValue::Int(n as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("prompt_caching"),
        VmValue::Bool(caps.prompt_caching),
    );
    dict.put_str(
        "cache_breakpoint_style",
        caps.cache_breakpoint_style.as_str(),
    );
    // Full cache-control profile (min useful prefix, TTL notes, usage-field
    // mapping) derived from this one capability path, so Burin dogfood (#3532)
    // and Harn Cloud receipts (#1106) read cache requirements without a
    // provider-specific branch of their own.
    let cache_profile = crate::llm::cache_conformance::CacheControlProfile::from_capabilities(caps);
    let mut cache_control = crate::value::DictMap::new();
    cache_control.insert(
        crate::value::intern_key("prompt_caching"),
        VmValue::Bool(cache_profile.prompt_caching),
    );
    cache_control.put_str(
        "cache_breakpoint_style",
        &cache_profile.cache_breakpoint_style,
    );
    cache_control.insert(
        crate::value::intern_key("min_useful_prefix_tokens"),
        cache_profile
            .min_useful_prefix_tokens
            .map(|n| VmValue::Int(n as i64))
            .unwrap_or(VmValue::Nil),
    );
    cache_control.insert(
        crate::value::intern_key("ttl_notes"),
        cache_profile
            .ttl_notes
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
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
    dict.insert(
        crate::value::intern_key("cache_control"),
        VmValue::dict(cache_control),
    );
    dict.insert(
        crate::value::intern_key("prefers_xml_scaffolding"),
        VmValue::Bool(caps.prefers_xml_scaffolding),
    );
    dict.insert(
        crate::value::intern_key("prefers_markdown_scaffolding"),
        VmValue::Bool(caps.prefers_markdown_scaffolding),
    );
    dict.put_str(
        "structured_output_mode",
        caps.structured_output_mode.as_str(),
    );
    dict.insert(
        crate::value::intern_key("supports_assistant_prefill"),
        VmValue::Bool(caps.supports_assistant_prefill),
    );
    dict.insert(
        crate::value::intern_key("prefers_role_developer"),
        VmValue::Bool(caps.prefers_role_developer),
    );
    dict.insert(
        crate::value::intern_key("prefers_xml_tools"),
        VmValue::Bool(caps.prefers_xml_tools),
    );
    dict.insert(
        crate::value::intern_key("thinking"),
        VmValue::Bool(!caps.thinking_modes.is_empty()),
    );
    dict.put_str("thinking_block_style", caps.thinking_block_style.as_str());
    dict.insert(
        crate::value::intern_key("thinking_modes"),
        string_list_to_vm_value(caps.thinking_modes.clone()),
    );
    dict.insert(
        crate::value::intern_key("interleaved_thinking_supported"),
        VmValue::Bool(caps.interleaved_thinking_supported),
    );
    dict.insert(
        crate::value::intern_key("anthropic_beta_features"),
        string_list_to_vm_value(caps.anthropic_beta_features.clone()),
    );
    dict.insert(
        crate::value::intern_key("vision_supported"),
        VmValue::Bool(caps.vision_supported),
    );
    dict.insert(crate::value::intern_key("audio"), VmValue::Bool(caps.audio));
    dict.insert(crate::value::intern_key("pdf"), VmValue::Bool(caps.pdf));
    dict.insert(crate::value::intern_key("video"), VmValue::Bool(caps.video));
    dict.insert(
        crate::value::intern_key("files_api_supported"),
        VmValue::Bool(caps.files_api_supported),
    );
    dict.insert(
        crate::value::intern_key("file_upload_wire_format"),
        caps.file_upload_wire_format
            .as_ref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value.clone())))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("structured_output"),
        caps.structured_output
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("json_schema"),
        caps.json_schema
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("prefers_xml_scaffolding"),
        VmValue::Bool(caps.prefers_xml_scaffolding),
    );
    dict.insert(
        crate::value::intern_key("prefers_markdown_scaffolding"),
        VmValue::Bool(caps.prefers_markdown_scaffolding),
    );
    dict.put_str(
        "structured_output_mode",
        caps.structured_output_mode.as_str(),
    );
    dict.insert(
        crate::value::intern_key("supports_assistant_prefill"),
        VmValue::Bool(caps.supports_assistant_prefill),
    );
    dict.insert(
        crate::value::intern_key("prefers_role_developer"),
        VmValue::Bool(caps.prefers_role_developer),
    );
    dict.insert(
        crate::value::intern_key("prefers_xml_tools"),
        VmValue::Bool(caps.prefers_xml_tools),
    );
    dict.put_str("thinking_block_style", caps.thinking_block_style.as_str());
    dict.insert(
        crate::value::intern_key("preserve_thinking"),
        VmValue::Bool(caps.preserve_thinking),
    );
    dict.insert(
        crate::value::intern_key("reasoning_history_wire_field"),
        reasoning_history_wire_field_value(caps),
    );
    dict.insert(
        crate::value::intern_key("requires_completion_tokens"),
        VmValue::Bool(caps.requires_completion_tokens),
    );
    dict.insert(
        crate::value::intern_key("chat_completions_unsupported"),
        VmValue::Bool(caps.chat_completions_unsupported),
    );
    dict.insert(
        crate::value::intern_key("requires_streaming"),
        VmValue::Bool(caps.requires_streaming),
    );
    dict.insert(
        crate::value::intern_key("reasoning_effort_supported"),
        VmValue::Bool(caps.reasoning_effort_supported),
    );
    dict.insert(
        crate::value::intern_key("reasoning_none_supported"),
        VmValue::Bool(caps.reasoning_none_supported),
    );
    dict.insert(
        crate::value::intern_key("reasoning_disable_supported"),
        VmValue::Bool(caps.reasoning_disable_supported),
    );
    dict.insert(
        crate::value::intern_key("reasoning_text_promotable"),
        VmValue::Bool(caps.reasoning_text_promotable),
    );
    dict.insert(
        crate::value::intern_key("reasoning_wire_format"),
        caps.reasoning_wire_format
            .as_ref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value.clone())))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("seed_supported"),
        VmValue::Bool(caps.seed_supported),
    );
    dict.insert(
        crate::value::intern_key("top_k_supported"),
        VmValue::Bool(caps.top_k_supported),
    );
    dict.insert(
        crate::value::intern_key("temperature_supported"),
        VmValue::Bool(caps.temperature_supported),
    );
    dict.insert(
        crate::value::intern_key("top_p_supported"),
        VmValue::Bool(caps.top_p_supported),
    );
    dict.insert(
        crate::value::intern_key("frequency_penalty_supported"),
        VmValue::Bool(caps.frequency_penalty_supported),
    );
    dict.insert(
        crate::value::intern_key("presence_penalty_supported"),
        VmValue::Bool(caps.presence_penalty_supported),
    );
    dict.insert(
        crate::value::intern_key("stop_supported"),
        VmValue::Bool(caps.stop_supported),
    );
    dict.insert(
        crate::value::intern_key("allowed_tool_choice_modes"),
        VmValue::List(std::sync::Arc::new(
            caps.allowed_tool_choice_modes
                .iter()
                .map(|mode| VmValue::String(arcstr::ArcStr::from(mode.as_str())))
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("requires_tool_result_adjacency"),
        VmValue::Bool(caps.requires_tool_result_adjacency),
    );
    dict.insert(
        crate::value::intern_key("auto_reasoning_overrides"),
        VmValue::dict(
            caps.auto_reasoning_overrides
                .iter()
                .map(|(task, mode)| {
                    (
                        crate::value::intern_key(task),
                        VmValue::String(arcstr::ArcStr::from(mode.clone())),
                    )
                })
                .collect::<crate::value::DictMap>(),
        ),
    );
    // Accelerated-serving (`fast`) tier, read from the generalized
    // `serving_tiers` catalog so callers can branch on `llm_call(...,
    // { fast: true })` support without re-parsing the model row.
    let fast_tier = crate::llm::serving_tiers::fast_tier(model);
    let fast_tier_supported = matches!(
        crate::llm::serving_tiers::fast_gate(model),
        crate::llm::serving_tiers::ServingTierGate::Usable
    );
    dict.insert(
        crate::value::intern_key("fast_tier_supported"),
        VmValue::Bool(fast_tier_supported),
    );
    if let Some(fast_tier) = fast_tier {
        dict.insert(
            crate::value::intern_key("fast_tier"),
            serving_tier_to_vm_value(&fast_tier),
        );
    }
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
    dict.insert(crate::value::intern_key("api"), VmValue::Bool(true));
    dict.insert(
        crate::value::intern_key("wire_format"),
        VmValue::String(arcstr::ArcStr::from(wire_format)),
    );
    dict.insert(
        crate::value::intern_key("input_mode"),
        VmValue::String(arcstr::ArcStr::from(input_mode)),
    );
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
    dict.insert(
        crate::value::intern_key("batch_api"),
        VmValue::Bool(batch_api),
    );
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
