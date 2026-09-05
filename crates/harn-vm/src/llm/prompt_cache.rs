//! Canonical request-side prompt-cache marker lowering.
//!
//! Provider adapters supply the marker payload (for example Anthropic's TTL),
//! while the capability matrix owns whether and where it is placed. Explicit
//! caller-authored markers always win.

use crate::llm::capabilities::{CacheBreakpointStyle, Capabilities};

pub(crate) fn apply_prompt_cache_breakpoint(
    body: &mut serde_json::Value,
    cache_requested: bool,
    caps: &Capabilities,
    marker: serde_json::Value,
) {
    if !cache_requested || !caps.prompt_caching || body_contains_cache_control(body) {
        return;
    }
    match caps.cache_breakpoint_style {
        CacheBreakpointStyle::TopLevel => body["cache_control"] = marker,
        CacheBreakpointStyle::LastBlock => {
            insert_last_message_cache_control(body, &marker);
        }
        // A route that caches automatically takes no marker, and on OpenAI an
        // unexpected `cache_control` is a hard 400 (`Unknown parameter:
        // 'cache_control'.`), not a field the provider ignores. Refuse to emit
        // one here rather than leaving it to whether a provider row happens to
        // omit `cache_breakpoint_style`: every caller shares this arm, so a new
        // OpenAI-compatible provider cannot pick up the Anthropic-shaped marker
        // its adapter passes just by sitting next to a row that declares a
        // style.
        CacheBreakpointStyle::None => {}
    }
}

fn body_contains_cache_control(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.contains_key("cache_control") || object.values().any(body_contains_cache_control)
        }
        serde_json::Value::Array(values) => values.iter().any(body_contains_cache_control),
        _ => false,
    }
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

    fn body() -> serde_json::Value {
        serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
        })
    }

    fn marker() -> serde_json::Value {
        serde_json::json!({"type": "ephemeral"})
    }

    /// A route that caches automatically must not receive a marker. OpenAI
    /// rejects an unexpected `cache_control` with
    /// `Unknown parameter: 'cache_control'.` rather than ignoring it, so this
    /// is the difference between prompt caching working and every
    /// cache-requesting call failing.
    #[test]
    fn none_style_emits_no_marker() {
        let mut value = body();
        apply_prompt_cache_breakpoint(
            &mut value,
            true,
            &caps(CacheBreakpointStyle::None),
            marker(),
        );
        assert_eq!(value, body(), "automatic-cache routes take no marker");
        assert!(!body_contains_cache_control(&value));
    }

    /// Direction control. If `none_style_emits_no_marker` passed because the
    /// marker never reaches the body at all, this fails and says so.
    #[test]
    fn top_level_style_still_emits_the_marker() {
        let mut value = body();
        apply_prompt_cache_breakpoint(
            &mut value,
            true,
            &caps(CacheBreakpointStyle::TopLevel),
            marker(),
        );
        assert_eq!(value["cache_control"], marker());
    }

    /// Second direction control, for the other marker placement.
    #[test]
    fn last_block_style_still_marks_the_final_message() {
        let mut value = body();
        apply_prompt_cache_breakpoint(
            &mut value,
            true,
            &caps(CacheBreakpointStyle::LastBlock),
            marker(),
        );
        assert!(
            body_contains_cache_control(&value),
            "last_block must mark the final message"
        );
        assert!(value.get("cache_control").is_none());
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
            let payload = crate::llm::api::LlmRequestPayload::from(&opts);
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
        let payload = crate::llm::api::LlmRequestPayload::from(&opts);
        let built =
            crate::llm::providers::anthropic::AnthropicProvider::build_request_body(&payload);
        assert!(
            body_contains_cache_control(&built),
            "an Anthropic route with caching on must still carry a cache_control marker"
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
            let mut value = body();
            let off = Capabilities {
                prompt_caching: false,
                cache_breakpoint_style: style,
                ..Capabilities::default()
            };
            apply_prompt_cache_breakpoint(&mut value, true, &off, marker());
            assert_eq!(
                value,
                body(),
                "{style:?} must stay inert while caching is off"
            );
        }
    }
}
