//! Script-facing capability views over the canonical serializable field values.

use crate::llm::capabilities::Capabilities;
use crate::llm_config;
use crate::value::{intern_key, DictMap, VmDictExt, VmValue};

use super::catalog_projection::tools_value;
use super::serialization::{fields, value};

pub(crate) fn capabilities_to_vm_value(
    provider: &str,
    model: &str,
    caps: &Capabilities,
) -> VmValue {
    let mut dict = fields!(caps;
        native_tools,
        runtime_probe,
        message_wire_format,
        live_endpoint_family,
        decision_protocol,
        decision_question_kinds,
        decision_upstream_provider,
        decision_limits,
        native_tool_wire_format,
        text_tool_wire_format_supported,
        preferred_tool_format,
        tool_mode_parity,
        tool_mode_parity_source,
        tool_mode_parity_notes,
        defer_loading,
        tool_search,
        responses_api,
        hosted_tools,
        remote_mcp,
        conversation_state,
        compaction,
        background_mode,
        tool_approval_policy,
        max_tools,
        prompt_caching,
        cache_breakpoint_style,
        reasoning_round_trip,
        prefers_xml_scaffolding,
        prefers_markdown_scaffolding,
        structured_output_mode,
        supports_assistant_prefill,
        prefers_role_developer,
        prefers_xml_tools,
        thinking_block_style,
        thinking_modes,
        interleaved_thinking_supported,
        anthropic_beta_features,
        vision_supported,
        audio,
        pdf,
        video,
        files_api_supported,
        file_upload_wire_format,
        structured_output,
        structured_output_strategy,
        json_schema,
        preserve_thinking,
        honors_preserve_thinking_kwarg,
        requires_parallel_tool_calls_false,
        reasoning_history_wire_field,
        requires_completion_tokens,
        chat_completions_unsupported,
        requires_streaming,
        reasoning_effort_supported,
        reasoning_effort_levels,
        reasoning_none_supported,
        reasoning_disable_supported,
        thinking_off_type,
        reasoning_text_promotable,
        reasoning_wire_format,
        reasoning_excluded_portable_options,
        seed_supported,
        portable_option_qualification,
        top_k_supported,
        portable_option_effort_support,
        temperature_supported,
        top_p_supported,
        frequency_penalty_supported,
        presence_penalty_supported,
        stop_supported,
        advanced_generation_options,
        allowed_tool_choice_modes,
        auto_reasoning_overrides,
    );
    dict.put_str("provider", provider);
    dict.put_str("model", model);
    dict.put("tools", tools_value(caps));
    dict.put_bool("thinking", !caps.thinking_modes.is_empty());
    dict.put(
        "cache_control",
        value(&crate::llm::cache_conformance::CacheControlProfile::from_capabilities(caps)),
    );
    insert_batch_support_fields(
        &mut dict,
        llm_config::effective_batch_api_supported(provider, caps),
        caps,
    );
    dict.put_bool(
        "fast_tier_supported",
        matches!(
            crate::llm::serving_tiers::fast_gate(model),
            crate::llm::serving_tiers::ServingTierGate::Usable
        ),
    );
    if let Some(tier) = crate::llm::serving_tiers::fast_tier(model) {
        dict.put("fast_tier", value(&tier));
    }
    VmValue::dict(dict)
}

pub(super) fn insert_batch_support_fields(
    dict: &mut DictMap,
    batch_api: bool,
    caps: &Capabilities,
) {
    let batch = fields!(caps;
        batch_wire_format,
        batch_input_mode,
        batch_discount_percent,
        batch_turnaround_hours,
        batch_max_requests,
        batch_max_input_bytes,
        batch_result_retention_days,
        batch_result_ordering,
        batch_partial_failure,
        batch_cancellation,
        batch_security_notes,
        batch_operational_notes,
        batch_regions,
    );
    let batch: DictMap = batch
        .into_iter()
        .map(|(key, field)| {
            let empty_list = matches!(&field, VmValue::List(items) if items.is_empty());
            let overflow = matches!(key.as_str(), "batch_max_requests" | "batch_max_input_bytes")
                && matches!(&field, VmValue::Float(_));
            (
                key,
                if !batch_api || empty_list || overflow {
                    VmValue::Nil
                } else {
                    field
                },
            )
        })
        .collect();
    let nested = if batch_api
        && matches!(batch.get("batch_wire_format"), Some(VmValue::String(_)))
        && matches!(batch.get("batch_input_mode"), Some(VmValue::String(_)))
    {
        let mut nested: DictMap = batch
            .iter()
            .map(|(key, value)| {
                (
                    intern_key(key.strip_prefix("batch_").expect("batch field prefix")),
                    value.clone(),
                )
            })
            .collect();
        nested.put_bool("api", true);
        VmValue::dict(nested)
    } else {
        VmValue::Nil
    };
    dict.extend(batch);
    dict.put_bool("batch_api", batch_api);
    dict.put("batch", nested);
}
