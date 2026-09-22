//! Materializing a matched capability rule into a [`Capabilities`] value.
//!
//! Split out of `rule.rs` so the resolution engine and the field-by-field
//! projection can grow independently. `rule.rs` owns which rule wins; this
//! owns what that rule means.

use super::effort::rule_thinking_modes;
use super::model::{
    Capabilities, LiveEndpointFamily, ProviderDefaults, StructuredOutputStrategy, WireDialect,
};
use super::pattern::ModelPatterns;
use super::rule::{
    rule_structured_output, rule_structured_output_mode, rule_thinking_block_style,
    rule_tool_mode_parity, rule_vision, ProviderRule,
};

pub(super) fn defaults_to_caps(defaults: &ProviderDefaults) -> Capabilities {
    let empty = ProviderRule {
        model_match: ModelPatterns::One("*".to_string()),
        version_min: None,
        extends: false,
        native_tools: None,
        message_wire_format: None,
        live_endpoint_family: None,
        decision_protocol: None,
        decision_question_kinds: None,
        decision_limits: None,
        native_tool_wire_format: None,
        defer_loading: None,
        tool_search: None,
        responses_api: None,
        hosted_tools: None,
        remote_mcp: None,
        conversation_state: None,
        compaction: None,
        background_mode: None,
        tool_approval_policy: None,
        batch_api: None,
        batch_wire_format: None,
        batch_input_mode: None,
        batch_discount_percent: None,
        batch_turnaround_hours: None,
        batch_max_requests: None,
        batch_max_input_bytes: None,
        batch_result_retention_days: None,
        batch_result_ordering: None,
        batch_partial_failure: None,
        batch_cancellation: None,
        batch_security_notes: None,
        batch_operational_notes: None,
        batch_regions: None,
        max_tools: None,
        prompt_caching: None,
        prompt_cache_ttls: None,
        prompt_cache_min_prefix_tokens: None,
        cache_breakpoint_style: None,
        vision: None,
        audio: None,
        pdf: None,
        video: None,
        files_api_supported: None,
        file_upload_wire_format: None,
        structured_output: None,
        prefers_xml_scaffolding: None,
        reserved_tool_call_token: None,
        prefers_markdown_scaffolding: None,
        structured_output_mode: None,
        supports_assistant_prefill: None,
        prefers_role_developer: None,
        prefers_xml_tools: None,
        thinking_block_style: None,
        json_schema: None,
        thinking_modes: None,
        interleaved_thinking_supported: None,
        anthropic_beta_features: None,
        thinking: None,
        vision_supported: None,
        image_url_input_supported: None,
        preserve_thinking: None,
        honors_preserve_thinking_kwarg: None,
        reasoning_round_trip: None,
        reasoning_history_wire_field: None,
        server_parser: None,
        honors_chat_template_kwargs: None,
        chat_template_options_field: None,
        requires_completion_tokens: None,
        chat_completions_unsupported: None,
        reasoning_tools_require_responses: None,
        requires_streaming: None,
        reasoning_effort_supported: None,
        reasoning_effort_levels: None,
        reasoning_none_supported: None,
        max_thinking_budget: None,
        reasoning_disable_supported: None,
        reasoning_required_for_tools: None,
        reasoning_text_promotable: None,
        reasoning_wire_format: None,
        reasoning_excluded_portable_options: None,
        seed_supported: None,
        top_k_supported: None,
        temperature_supported: None,
        top_p_supported: None,
        frequency_penalty_supported: None,
        presence_penalty_supported: None,
        stop_supported: None,
        advanced_generation_options: None,
        allowed_tool_choice_modes: None,
        requires_tool_result_adjacency: None,
        supports_parallel_tool_calls: None,
        requires_parallel_tool_calls_false: None,
        tools_exclude_response_format: None,
        recommended_endpoint: None,
        text_tool_wire_format_supported: None,
        preferred_tool_format: None,
        tool_mode_parity: None,
        tool_mode_parity_notes: None,
        tool_format_justification: None,
        thinking_disable_directive: None,
        auto_reasoning_overrides: None,
        provider_route_denylist: None,
        openrouter_provider_order: None,
        serving_precision: None,
        computer_use_style: None,
        screenshot_scaling: None,
        safety_ack_flow: None,
        system_message_placement: None,
    };
    let mut caps = rule_to_caps(&empty, defaults);
    caps.preferred_tool_format = None;
    caps.tool_mode_parity = None;
    caps
}

pub(super) fn rule_to_caps(rule: &ProviderRule, defaults: &ProviderDefaults) -> Capabilities {
    let (parity_verdict, parity_source) = rule_tool_mode_parity(rule);
    let thinking_modes = rule_thinking_modes(rule);
    let reasoning_effort_supported = thinking_modes.iter().any(|mode| mode == "effort");
    let thinking_block_style = rule_thinking_block_style(rule);
    let prompt_caching = rule.prompt_caching.unwrap_or(false);
    // A route that represents reasoning as inline `<think>` blocks in prompt
    // context is exactly the one that emits inline `<think>` in its responses,
    // so derive the response-splitting quirk from the resolved style rather
    // than adding a second, drift-prone catalog field.
    let emits_inline_reasoning = thinking_block_style == "inline";
    let message_wire_format = WireDialect::from_message_wire_format(
        &rule
            .message_wire_format
            .clone()
            .or_else(|| defaults.message_wire_format.clone())
            .unwrap_or_else(|| "openai".to_string()),
    );
    // Only the Gemini dialect serves two live endpoint families, so an unset
    // value is meaningful only there — and there it means the legacy
    // `:generateContent` path. Deriving it once here (rather than defaulting
    // per call site) is what keeps `provider_capabilities` output, the dispatch
    // report, and the transport switch reading the same value.
    let live_endpoint_family = rule
        .live_endpoint_family
        .or(defaults.live_endpoint_family)
        .or_else(|| {
            (message_wire_format == WireDialect::Gemini)
                .then_some(LiveEndpointFamily::GeminiGenerateContent)
        });
    Capabilities {
        native_tools: rule.native_tools.unwrap_or(false),
        message_wire_format,
        live_endpoint_family,
        decision_protocol: rule.decision_protocol,
        decision_question_kinds: rule.decision_question_kinds.clone().unwrap_or_default(),
        decision_limits: rule.decision_limits,
        native_tool_wire_format: rule
            .native_tool_wire_format
            .clone()
            .or_else(|| defaults.native_tool_wire_format.clone())
            .unwrap_or_else(|| "openai".to_string()),
        defer_loading: rule.defer_loading.unwrap_or(false),
        tool_search: rule.tool_search.clone().unwrap_or_default(),
        responses_api: rule.responses_api.unwrap_or(false),
        hosted_tools: rule.hosted_tools.clone().unwrap_or_default(),
        remote_mcp: rule.remote_mcp.unwrap_or(false),
        conversation_state: rule.conversation_state.unwrap_or(false),
        compaction: rule.compaction.unwrap_or(false),
        background_mode: rule.background_mode.unwrap_or(false),
        batch_api: rule.batch_api.or(defaults.batch_api).unwrap_or(false),
        batch_wire_format: rule
            .batch_wire_format
            .clone()
            .or_else(|| defaults.batch_wire_format.clone()),
        batch_input_mode: rule
            .batch_input_mode
            .clone()
            .or_else(|| defaults.batch_input_mode.clone()),
        batch_discount_percent: rule
            .batch_discount_percent
            .or(defaults.batch_discount_percent),
        batch_turnaround_hours: rule
            .batch_turnaround_hours
            .or(defaults.batch_turnaround_hours),
        batch_max_requests: rule.batch_max_requests.or(defaults.batch_max_requests),
        batch_max_input_bytes: rule
            .batch_max_input_bytes
            .or(defaults.batch_max_input_bytes),
        batch_result_retention_days: rule
            .batch_result_retention_days
            .or(defaults.batch_result_retention_days),
        batch_result_ordering: rule
            .batch_result_ordering
            .clone()
            .or_else(|| defaults.batch_result_ordering.clone()),
        batch_partial_failure: rule
            .batch_partial_failure
            .clone()
            .or_else(|| defaults.batch_partial_failure.clone()),
        batch_cancellation: rule
            .batch_cancellation
            .clone()
            .or_else(|| defaults.batch_cancellation.clone()),
        batch_security_notes: rule
            .batch_security_notes
            .clone()
            .or_else(|| defaults.batch_security_notes.clone())
            .unwrap_or_default(),
        batch_operational_notes: rule
            .batch_operational_notes
            .clone()
            .or_else(|| defaults.batch_operational_notes.clone())
            .unwrap_or_default(),
        batch_regions: rule
            .batch_regions
            .clone()
            .or_else(|| defaults.batch_regions.clone())
            .unwrap_or_default(),
        tool_approval_policy: rule.tool_approval_policy.clone(),
        max_tools: rule.max_tools,
        prompt_caching,
        prompt_cache_ttls: if prompt_caching {
            rule.prompt_cache_ttls
                .clone()
                .or_else(|| defaults.prompt_cache_ttls.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        prompt_cache_min_prefix_tokens: if prompt_caching {
            rule.prompt_cache_min_prefix_tokens
                .or(defaults.prompt_cache_min_prefix_tokens)
        } else {
            None
        },
        cache_breakpoint_style: rule
            .cache_breakpoint_style
            .or(defaults.cache_breakpoint_style)
            .unwrap_or_default(),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
        video: rule.video.unwrap_or(false),
        files_api_supported: rule
            .files_api_supported
            .or(defaults.files_api_supported)
            .unwrap_or(false),
        file_upload_wire_format: rule
            .file_upload_wire_format
            .clone()
            .or_else(|| defaults.file_upload_wire_format.clone()),
        structured_output: rule_structured_output(rule),
        structured_output_strategy: StructuredOutputStrategy::from_declaration(
            rule.structured_output
                .as_deref()
                .or(rule.json_schema.as_deref()),
        ),
        json_schema: rule_structured_output(rule),
        prefers_xml_scaffolding: rule.prefers_xml_scaffolding.unwrap_or(false),
        reserved_tool_call_token: rule.reserved_tool_call_token.unwrap_or(false),
        prefers_markdown_scaffolding: rule.prefers_markdown_scaffolding.unwrap_or(false),
        structured_output_mode: rule_structured_output_mode(rule),
        supports_assistant_prefill: rule.supports_assistant_prefill.unwrap_or(false),
        prefers_role_developer: rule.prefers_role_developer.unwrap_or(false),
        prefers_xml_tools: rule.prefers_xml_tools.unwrap_or(false),
        thinking_block_style,
        emits_inline_reasoning,
        thinking_modes,
        interleaved_thinking_supported: rule.interleaved_thinking_supported.unwrap_or(false),
        anthropic_beta_features: rule.anthropic_beta_features.clone().unwrap_or_default(),
        vision_supported: rule.vision_supported.unwrap_or(false),
        image_url_input_supported: rule
            .image_url_input_supported
            .or(defaults.image_url_input_supported)
            .unwrap_or(true),
        preserve_thinking: rule.preserve_thinking.unwrap_or(false),
        honors_preserve_thinking_kwarg: rule.honors_preserve_thinking_kwarg.unwrap_or(false),
        reasoning_round_trip: rule
            .reasoning_round_trip
            .or(defaults.reasoning_round_trip)
            .unwrap_or_default(),
        reasoning_history_wire_field: rule.reasoning_history_wire_field,
        server_parser: rule
            .server_parser
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        honors_chat_template_kwargs: rule.honors_chat_template_kwargs.unwrap_or(false),
        chat_template_options_field: rule.chat_template_options_field.clone(),
        requires_completion_tokens: rule.requires_completion_tokens.unwrap_or(false),
        chat_completions_unsupported: rule.chat_completions_unsupported.unwrap_or(false),
        reasoning_tools_require_responses: rule.reasoning_tools_require_responses.unwrap_or(false),
        requires_streaming: rule.requires_streaming.unwrap_or(false),
        reasoning_effort_supported,
        reasoning_effort_levels: rule.reasoning_effort_levels.clone().unwrap_or_default(),
        reasoning_none_supported: rule.reasoning_none_supported.unwrap_or(false),
        max_thinking_budget: rule.max_thinking_budget,
        reasoning_disable_supported: rule.reasoning_disable_supported.unwrap_or(true),
        reasoning_required_for_tools: rule.reasoning_required_for_tools.unwrap_or(false),
        reasoning_text_promotable: rule.reasoning_text_promotable.unwrap_or(false),
        reasoning_wire_format: rule
            .reasoning_wire_format
            .clone()
            .or_else(|| defaults.reasoning_wire_format.clone()),
        reasoning_excluded_portable_options: rule
            .reasoning_excluded_portable_options
            .clone()
            .unwrap_or_default(),
        seed_supported: rule
            .seed_supported
            .or(defaults.seed_supported)
            .unwrap_or(true),
        top_k_supported: rule
            .top_k_supported
            .or(defaults.top_k_supported)
            .unwrap_or(true),
        temperature_supported: rule
            .temperature_supported
            .or(defaults.temperature_supported)
            .unwrap_or(true),
        top_p_supported: rule
            .top_p_supported
            .or(defaults.top_p_supported)
            .unwrap_or(true),
        frequency_penalty_supported: rule
            .frequency_penalty_supported
            .or(defaults.frequency_penalty_supported)
            .unwrap_or(true),
        presence_penalty_supported: rule
            .presence_penalty_supported
            .or(defaults.presence_penalty_supported)
            .unwrap_or(true),
        stop_supported: rule
            .stop_supported
            .or(defaults.stop_supported)
            .unwrap_or(true),
        advanced_generation_options: rule
            .advanced_generation_options
            .clone()
            .or(defaults.advanced_generation_options.clone())
            .unwrap_or_default(),
        allowed_tool_choice_modes: rule.allowed_tool_choice_modes.clone().unwrap_or_default(),
        requires_tool_result_adjacency: rule.requires_tool_result_adjacency.unwrap_or(false),
        supports_parallel_tool_calls: rule
            .supports_parallel_tool_calls
            .or(defaults.supports_parallel_tool_calls)
            .unwrap_or(true),
        requires_parallel_tool_calls_false: rule
            .requires_parallel_tool_calls_false
            .or(defaults.requires_parallel_tool_calls_false)
            .unwrap_or(false),
        tools_exclude_response_format: rule.tools_exclude_response_format.unwrap_or(false),
        recommended_endpoint: rule.recommended_endpoint.clone(),
        text_tool_wire_format_supported: rule.text_tool_wire_format_supported.unwrap_or(true),
        preferred_tool_format: Some(rule_preferred_tool_format(rule)),
        tool_mode_parity: Some(parity_verdict),
        tool_mode_parity_source: Some(parity_source),
        tool_mode_parity_notes: rule.tool_mode_parity_notes.clone(),
        thinking_disable_directive: rule.thinking_disable_directive.clone(),
        auto_reasoning_overrides: rule.auto_reasoning_overrides.clone().unwrap_or_default(),
        provider_route_denylist: rule.provider_route_denylist.clone().unwrap_or_default(),
        openrouter_provider_order: rule.openrouter_provider_order.clone().unwrap_or_default(),
        serving_precision: rule
            .serving_precision
            .clone()
            .unwrap_or_else(|| "unverified".to_string()),
        computer_use_style: rule.computer_use_style,
        screenshot_scaling: rule.screenshot_scaling,
        safety_ack_flow: rule.safety_ack_flow.unwrap_or(false),
        system_message_placement: rule.system_message_placement,
        runtime_probe: None,
    }
}

pub(super) fn rule_preferred_tool_format(rule: &ProviderRule) -> String {
    // This is the `caps.preferred_tool_format` the runtime `lookup` returns for
    // a matched capability row. When the row pins a format, honor it (including
    // an explicit `text` — the reverse safety valve). Otherwise derive: native
    // models get `native`, text-channel models get `json` (fenced-JSON), the
    // GLOBAL text-channel default. Heredoc `text` is never auto-derived.
    rule.preferred_tool_format.clone().unwrap_or_else(|| {
        if rule.native_tools.unwrap_or(false) {
            "native".to_string()
        } else {
            "json".to_string()
        }
    })
}
