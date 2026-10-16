//! Resolve native catalog state before shared prompt-cache lowering.

use crate::llm::api::LlmRequestPayload;
#[cfg(test)]
use crate::llm::capabilities::{CacheBreakpointStyle, Capabilities};
#[cfg(test)]
use harn_llm::wire::prompt_cache::BreakpointPlacement;
pub(crate) use harn_llm::wire::prompt_cache::{
    apply_prompt_cache_breakpoint, PromptCacheBreakpoint,
};

pub(crate) fn for_request(request: &LlmRequestPayload) -> PromptCacheBreakpoint {
    let caps = crate::llm::managed_supply::capabilities_for(&request.provider, &request.model);
    PromptCacheBreakpoint::resolve(request, &caps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(style: CacheBreakpointStyle) -> Capabilities {
        let mut caps = Capabilities::default();
        caps.prompt_caching = true;
        caps.cache_breakpoint_style = style;
        caps
    }

    fn request(messages: Vec<serde_json::Value>) -> LlmRequestPayload {
        let mut opts = crate::llm::api::options::base_opts("anthropic");
        opts.messages = messages;
        opts.native_tools = None;
        opts.cache = true;
        LlmRequestPayload::from(&opts)
    }

    fn ordinary() -> LlmRequestPayload {
        request(vec![
            serde_json::json!({"role": "user", "content": "hello"}),
        ])
    }

    fn body() -> serde_json::Value {
        serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
        })
    }

    fn marker() -> serde_json::Value {
        serde_json::json!({"type": "ephemeral"})
    }

    fn body_contains_cache_control(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(object) => {
                object.contains_key("cache_control")
                    || object.values().any(body_contains_cache_control)
            }
            serde_json::Value::Array(values) => values.iter().any(body_contains_cache_control),
            _ => false,
        }
    }

    fn count_cache_control(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(object) => {
                usize::from(object.contains_key("cache_control"))
                    + object.values().map(count_cache_control).sum::<usize>()
            }
            serde_json::Value::Array(values) => values.iter().map(count_cache_control).sum(),
            _ => 0,
        }
    }

    fn resolve_and_apply(
        style: CacheBreakpointStyle,
    ) -> (PromptCacheBreakpoint, serde_json::Value) {
        let breakpoint = PromptCacheBreakpoint::resolve(&ordinary(), &caps(style));
        let mut value = body();
        apply_prompt_cache_breakpoint(&mut value, breakpoint, marker());
        (breakpoint, value)
    }

    /// A route that caches automatically must not receive a marker. OpenAI
    /// rejects an unexpected `cache_control` with
    /// `Unknown parameter: 'cache_control'.` rather than ignoring it, so this
    /// is the difference between prompt caching working and every
    /// cache-requesting call failing.
    #[test]
    fn none_style_emits_no_marker() {
        let (breakpoint, value) = resolve_and_apply(CacheBreakpointStyle::None);
        assert_eq!(breakpoint, PromptCacheBreakpoint::Automatic);
        assert_eq!(value, body(), "automatic-cache routes take no marker");
    }

    /// Direction control. If `none_style_emits_no_marker` passed because the
    /// marker never reaches the body at all, this fails and says so.
    #[test]
    fn top_level_style_still_emits_the_marker() {
        let (breakpoint, value) = resolve_and_apply(CacheBreakpointStyle::TopLevel);
        assert_eq!(
            breakpoint,
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::TopLevel
            }
        );
        assert_eq!(value["cache_control"], marker());
    }

    /// Second direction control, for the other marker placement.
    #[test]
    fn last_block_style_still_marks_the_final_message() {
        let (breakpoint, value) = resolve_and_apply(CacheBreakpointStyle::LastBlock);
        assert_eq!(
            breakpoint,
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::LastBlock
            }
        );
        assert_eq!(
            value["messages"][0]["content"][0]["cache_control"],
            marker()
        );
        assert!(value.get("cache_control").is_none());
    }

    /// The falsifier from #8026. A caller marker on one message content block
    /// suppresses Harn's breakpoint for the whole request, and that must be
    /// named as a deferral, not look like a placement on a too-short prefix.
    /// The ordinary request beside it is the negative control, asserted to the
    /// exact variant rather than merely to differ.
    #[test]
    fn caller_marker_defers_and_ordinary_request_places() {
        let caps = caps(CacheBreakpointStyle::LastBlock);
        let marked = request(vec![
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": "stable prefix", "cache_control": marker()},
            ]}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ]);
        assert_eq!(
            PromptCacheBreakpoint::resolve(&marked, &caps),
            PromptCacheBreakpoint::DeferredToExistingMarker
        );
        assert_eq!(
            PromptCacheBreakpoint::resolve(&ordinary(), &caps),
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::LastBlock
            }
        );
    }

    /// Every place a provider reads a caller marker defers.
    #[test]
    fn caller_markers_in_every_provider_position_defer() {
        let caps = caps(CacheBreakpointStyle::LastBlock);
        let tool_result_block = request(vec![
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "run", "input": {}},
            ]}),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "t1", "content": [
                    {"type": "text", "text": "out", "cache_control": marker()},
                ]}],
            }),
        ]);
        let mut tool_definition = ordinary();
        tool_definition.native_tools = Some(vec![serde_json::json!({
            "name": "lookup",
            "input_schema": {"type": "object"},
            "cache_control": marker(),
        })]);
        for (label, request) in [
            ("tool_result block", tool_result_block),
            ("tool definition", tool_definition),
        ] {
            assert_eq!(
                PromptCacheBreakpoint::resolve(&request, &caps),
                PromptCacheBreakpoint::DeferredToExistingMarker,
                "{label}"
            );
        }
    }

    /// A key named `cache_control` in tool arguments or a parameter schema is
    /// user data. It must not suppress the breakpoint.
    #[test]
    fn cache_control_named_user_data_does_not_defer() {
        let mut request = request(vec![
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "set", "input": {"cache_control": "on"}},
            ]}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ]);
        request.native_tools = Some(vec![serde_json::json!({
            "name": "set",
            "input_schema": {
                "type": "object",
                "properties": {"cache_control": {"type": "string"}},
            },
        })]);
        assert_eq!(
            PromptCacheBreakpoint::resolve(&request, &caps(CacheBreakpointStyle::LastBlock)),
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::LastBlock
            }
        );
    }

    /// Caller-marker shapes, each paired with whether the deferral rule must
    /// count it. Shared by the policy test and the egress drift guard below.
    fn marker_corpus() -> Vec<(
        &'static str,
        Vec<serde_json::Value>,
        Option<serde_json::Value>,
        bool,
    )> {
        let use_and_result = |result: serde_json::Value| {
            vec![
                serde_json::json!({"role": "user", "content": "run it"}),
                serde_json::json!({"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "run", "input": {}},
                ]}),
                serde_json::json!({"role": "user", "content": [result]}),
            ]
        };
        vec![
            (
                "text block",
                vec![serde_json::json!({"role": "user", "content": [
                    {"type": "text", "text": "stable prefix", "cache_control": marker()},
                    {"type": "text", "text": "hello"},
                ]})],
                None,
                true,
            ),
            (
                "tool_result block",
                use_and_result(serde_json::json!({
                    "type": "tool_result", "tool_use_id": "toolu_1", "content": "out",
                    "cache_control": marker(),
                })),
                None,
                true,
            ),
            (
                "block nested in a tool_result",
                use_and_result(serde_json::json!({
                    "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "text", "text": "out", "cache_control": marker()},
                    ],
                })),
                None,
                true,
            ),
            (
                "tool definition",
                vec![serde_json::json!({"role": "user", "content": "hello"})],
                Some(serde_json::json!({
                    "type": "function",
                    "function": {"name": "lookup", "parameters": {"type": "object"}},
                    "cache_control": marker(),
                })),
                true,
            ),
            (
                "message level",
                vec![serde_json::json!({
                    "role": "user", "content": "hello", "cache_control": marker(),
                })],
                None,
                false,
            ),
            (
                "whitespace-only text block",
                vec![serde_json::json!({"role": "user", "content": [
                    {"type": "text", "text": "  ", "cache_control": marker()},
                    {"type": "text", "text": "hello"},
                ]})],
                None,
                false,
            ),
        ]
    }

    fn corpus_request(
        provider: &str,
        model: &str,
        messages: Vec<serde_json::Value>,
        tool: Option<serde_json::Value>,
        cache: bool,
    ) -> LlmRequestPayload {
        let mut opts = crate::llm::api::options::base_opts(provider);
        opts.model = model.to_string();
        opts.messages = messages;
        opts.native_tools = tool.map(|tool| vec![tool]);
        opts.cache = cache;
        LlmRequestPayload::from(&opts)
    }

    #[test]
    fn deferral_counts_only_markers_in_provider_read_positions() {
        let caps = caps(CacheBreakpointStyle::LastBlock);
        for (label, messages, tool, counted) in marker_corpus() {
            let request = corpus_request(
                "anthropic",
                "claude-opus-4-5-20251101",
                messages,
                tool,
                true,
            );
            let expected = if counted {
                PromptCacheBreakpoint::DeferredToExistingMarker
            } else {
                PromptCacheBreakpoint::Placed {
                    style: BreakpointPlacement::LastBlock,
                }
            };
            assert_eq!(
                PromptCacheBreakpoint::resolve(&request, &caps),
                expected,
                "{label}"
            );
        }
    }

    /// Drift guard for the deferral rule. A deferral is only truthful if the
    /// caller marker it defers to reaches the provider, and the adapters
    /// sanitize after the decision: the OpenAI-compatible adapter keeps only
    /// allowlisted message keys and Anthropic drops whitespace-only blocks.
    /// Build each counted shape through both marker-placing adapters with
    /// caching off, so any marker in the body is the caller's, and require it
    /// to survive. A position an adapter starts dropping fails here instead of
    /// leaving a request with no breakpoint at all.
    #[test]
    fn every_counted_marker_survives_both_marker_placing_adapters() {
        for (label, messages, tool, counted) in marker_corpus() {
            if !counted {
                continue;
            }
            let anthropic = crate::llm::providers::anthropic::AnthropicProvider::build_request_body(
                &corpus_request(
                    "anthropic",
                    "claude-opus-4-5-20251101",
                    messages.clone(),
                    tool.clone(),
                    false,
                ),
            );
            let openai_compat =
                crate::llm::providers::openai_compat::OpenAiCompatibleProvider::build_request_body(
                    &corpus_request(
                        "openrouter",
                        "anthropic/claude-fable-5-1",
                        messages,
                        tool,
                        false,
                    ),
                );
            for (adapter, built) in [("anthropic", anthropic), ("openai_compat", openai_compat)] {
                assert!(
                    body_contains_cache_control(&built),
                    "{label}: the {adapter} adapter dropped a marker the deferral rule counts\n{built:#}"
                );
            }
        }
    }

    #[test]
    fn cache_off_is_not_requested() {
        let mut request = ordinary();
        request.cache = false;
        assert_eq!(
            PromptCacheBreakpoint::resolve(&request, &caps(CacheBreakpointStyle::LastBlock)),
            PromptCacheBreakpoint::NotRequested
        );
    }

    /// The flag is the outer gate: a route that does not declare prompt
    /// caching gets no marker whatever its style says.
    #[test]
    fn prompt_caching_off_suppresses_every_style() {
        for style in [
            CacheBreakpointStyle::TopLevel,
            CacheBreakpointStyle::LastBlock,
            CacheBreakpointStyle::None,
        ] {
            let mut off = Capabilities::default();
            off.prompt_caching = false;
            off.cache_breakpoint_style = style;
            let breakpoint = PromptCacheBreakpoint::resolve(&ordinary(), &off);
            assert_eq!(breakpoint, PromptCacheBreakpoint::Unsupported, "{style:?}");
            let mut value = body();
            apply_prompt_cache_breakpoint(&mut value, breakpoint, marker());
            assert_eq!(
                value,
                body(),
                "{style:?} must stay inert while caching is off"
            );
        }
    }

    /// The receipt names the outcome with a stable wire shape.
    #[test]
    fn outcome_serializes_as_tagged_record() {
        assert_eq!(
            serde_json::to_value(PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::LastBlock
            })
            .unwrap(),
            serde_json::json!({"outcome": "placed", "style": "last_block"})
        );
        assert_eq!(
            serde_json::to_value(PromptCacheBreakpoint::DeferredToExistingMarker).unwrap(),
            serde_json::json!({"outcome": "deferred_to_existing_marker"})
        );
    }

    /// Wire-level falsifier for the default-on flip. Declaring
    /// `prompt_caching` on the OpenAI rules also flips `cache` on by default,
    /// because `cache` resolves to `caps.prompt_caching` when the caller sets
    /// nothing. The whole flip rests on the serialized request still carrying
    /// no `cache_control`: OpenAI rejects one with
    /// `Unknown parameter: 'cache_control'.` rather than ignoring it, so a
    /// marker here is a hard 400 on every OpenAI call, not a wasted field.
    ///
    /// This asserts on the real builder with the real resolved capabilities,
    /// not on a hand-made `Capabilities`, which is the difference between
    /// testing the policy and testing the route.
    #[test]
    fn openai_route_with_cache_on_sends_no_cache_control() {
        for model in ["gpt-6-astra", "gpt-5.6-luna", "gpt-4o-mini"] {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.model = model.to_string();
            opts.cache = true;
            let payload = LlmRequestPayload::from(&opts);
            assert_eq!(
                for_request(&payload),
                PromptCacheBreakpoint::Automatic,
                "{model}"
            );
            let built =
                crate::llm::providers::openai_compat::OpenAiCompatibleProvider::build_request_body(
                    &payload,
                );
            assert!(
                !body_contains_cache_control(&built),
                "{model} must not carry cache_control anywhere in the request"
            );
        }
    }

    /// Positive control for the falsifier above. If the OpenAI assertion passed
    /// because no builder emits a marker at all, or because `cache` never
    /// reaches the builder, this fails and says which.
    #[test]
    fn anthropic_route_with_cache_on_does_send_cache_control() {
        let mut opts = crate::llm::api::options::base_opts("anthropic");
        opts.model = "claude-opus-4-5-20251101".to_string();
        opts.cache = true;
        let payload = LlmRequestPayload::from(&opts);
        let built =
            crate::llm::providers::anthropic::AnthropicProvider::build_request_body(&payload);
        assert!(
            body_contains_cache_control(&built),
            "an Anthropic route with caching on must still carry a cache_control marker"
        );
    }

    /// The deferral the receipt reports is the one the real builder obeys: a
    /// caller marker on an early block leaves exactly that one marker on the
    /// wire, and the final message stays unmarked.
    #[test]
    fn anthropic_builder_obeys_a_deferral() {
        let mut opts = crate::llm::api::options::base_opts("anthropic");
        opts.model = "claude-opus-4-5-20251101".to_string();
        opts.cache = true;
        opts.native_tools = None;
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": "stable prefix", "cache_control": marker()},
            ]}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ];
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(
            for_request(&payload),
            PromptCacheBreakpoint::DeferredToExistingMarker
        );
        let built =
            crate::llm::providers::anthropic::AnthropicProvider::build_request_body(&payload);
        assert_eq!(count_cache_control(&built), 1, "{built:#}");
        assert!(!body_contains_cache_control(
            built["messages"].as_array().unwrap().last().unwrap()
        ));
    }
}
