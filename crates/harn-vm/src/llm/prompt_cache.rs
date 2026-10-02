//! Canonical request-side prompt-cache marker lowering.
//!
//! Provider adapters supply the marker payload (for example Anthropic's TTL),
//! while the capability matrix owns whether and where it is placed. Explicit
//! caller-authored markers always win.
//!
//! The decision is a [`PromptCacheBreakpoint`] resolved from the neutral
//! request, not from a provider body, so the provider request receipt records
//! the same value the adapter lowers. A breakpoint that was never placed and
//! one placed on a prefix too short to cache both come back as
//! `cache_read_input_tokens: 0`; the receipt is what tells them apart.

use crate::llm::api::LlmRequestPayload;
use crate::llm::capabilities::{CacheBreakpointStyle, Capabilities};

/// What happened to the prompt-cache breakpoint for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum PromptCacheBreakpoint {
    /// Harn placed its own marker with this style.
    Placed { style: BreakpointPlacement },
    /// The caller turned caching off.
    NotRequested,
    /// The route does not declare prompt caching.
    Unsupported,
    /// The route caches without a request marker, so none is sent.
    Automatic,
    /// The request already carries a caller marker, which wins. Harn placed
    /// none of its own, so caching covers only the caller's prefixes.
    DeferredToExistingMarker,
}

/// Where a placed breakpoint went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BreakpointPlacement {
    TopLevel,
    LastBlock,
}

impl PromptCacheBreakpoint {
    /// Resolve against the catalog-owned capabilities of the request's route.
    pub(crate) fn for_request(request: &LlmRequestPayload) -> Self {
        let caps = crate::llm::managed_supply::capabilities_for(&request.provider, &request.model);
        Self::resolve(request, &caps)
    }

    pub(crate) fn resolve(request: &LlmRequestPayload, caps: &Capabilities) -> Self {
        if !request.cache {
            return Self::NotRequested;
        }
        if !caps.prompt_caching {
            return Self::Unsupported;
        }
        let style = match caps.cache_breakpoint_style {
            // A route that caches automatically takes no marker, and on OpenAI
            // an unexpected `cache_control` is a hard 400 (`Unknown parameter:
            // 'cache_control'.`), not a field the provider ignores. Refuse to
            // emit one here rather than leaving it to whether a provider row
            // happens to omit `cache_breakpoint_style`: every caller shares
            // this arm, so a new OpenAI-compatible provider cannot pick up the
            // Anthropic-shaped marker its adapter passes just by sitting next
            // to a row that declares a style.
            CacheBreakpointStyle::None => return Self::Automatic,
            CacheBreakpointStyle::TopLevel => BreakpointPlacement::TopLevel,
            CacheBreakpointStyle::LastBlock => BreakpointPlacement::LastBlock,
        };
        if request_carries_cache_marker(request) {
            return Self::DeferredToExistingMarker;
        }
        Self::Placed { style }
    }
}

/// Lower a resolved breakpoint onto a provider body.
pub(crate) fn apply_prompt_cache_breakpoint(
    body: &mut serde_json::Value,
    breakpoint: PromptCacheBreakpoint,
    marker: serde_json::Value,
) {
    match breakpoint {
        PromptCacheBreakpoint::Placed {
            style: BreakpointPlacement::TopLevel,
        } => body["cache_control"] = marker,
        PromptCacheBreakpoint::Placed {
            style: BreakpointPlacement::LastBlock,
        } => {
            insert_last_message_cache_control(body, &marker);
        }
        PromptCacheBreakpoint::NotRequested
        | PromptCacheBreakpoint::Unsupported
        | PromptCacheBreakpoint::Automatic
        | PromptCacheBreakpoint::DeferredToExistingMarker => {}
    }
}

/// A caller marker sits where providers read one: on a message, on a message
/// content block, on a block nested in a tool result, or on a tool definition.
/// A key named `cache_control` inside tool arguments or a JSON schema is user
/// data and does not count.
fn request_carries_cache_marker(request: &LlmRequestPayload) -> bool {
    request.messages.iter().any(message_carries_cache_marker)
        || request
            .native_tools
            .iter()
            .flatten()
            .any(|tool| tool.get("cache_control").is_some())
}

fn message_carries_cache_marker(message: &serde_json::Value) -> bool {
    message.get("cache_control").is_some()
        || match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => blocks.iter().any(block_carries_cache_marker),
            Some(block @ serde_json::Value::Object(_)) => block_carries_cache_marker(block),
            _ => false,
        }
}

fn block_carries_cache_marker(block: &serde_json::Value) -> bool {
    block.get("cache_control").is_some()
        || block
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|nested| {
                nested
                    .iter()
                    .any(|inner| inner.get("cache_control").is_some())
            })
}

fn insert_last_message_cache_control(
    body: &mut serde_json::Value,
    marker: &serde_json::Value,
) -> bool {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    messages
        .iter_mut()
        .rev()
        .any(|message| insert_message_cache_control(message, marker))
}

fn insert_message_cache_control(
    message: &mut serde_json::Value,
    marker: &serde_json::Value,
) -> bool {
    let Some(content) = message
        .as_object_mut()
        .and_then(|object| object.get_mut("content"))
    else {
        return false;
    };
    match content {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = text.clone();
            *content = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": marker,
            }]);
            true
        }
        serde_json::Value::Array(blocks) => blocks.iter_mut().rev().any(|block| {
            let Some(object) = block.as_object_mut() else {
                return false;
            };
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| marker.clone());
            true
        }),
        serde_json::Value::Object(object) => {
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| marker.clone());
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(style: CacheBreakpointStyle) -> Capabilities {
        Capabilities {
            prompt_caching: true,
            cache_breakpoint_style: style,
            ..Capabilities::default()
        }
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
        let message_level = request(vec![serde_json::json!({
            "role": "user", "content": "hello", "cache_control": marker(),
        })]);
        let tool_result_block = request(vec![serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "text", "text": "out", "cache_control": marker()},
            ]}],
        })]);
        let mut tool_definition = ordinary();
        tool_definition.native_tools = Some(vec![serde_json::json!({
            "name": "lookup",
            "input_schema": {"type": "object"},
            "cache_control": marker(),
        })]);
        for (label, request) in [
            ("message", message_level),
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
            let off = Capabilities {
                prompt_caching: false,
                cache_breakpoint_style: style,
                ..Capabilities::default()
            };
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
                PromptCacheBreakpoint::for_request(&payload),
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
            PromptCacheBreakpoint::for_request(&payload),
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
