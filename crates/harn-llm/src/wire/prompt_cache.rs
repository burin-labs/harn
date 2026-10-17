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

use crate::request::LlmRequestPayload;
use harn_provider_catalog::capabilities::{CacheBreakpointStyle, Capabilities};

/// What happened to the prompt-cache breakpoint for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PromptCacheBreakpoint {
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
pub enum BreakpointPlacement {
    TopLevel,
    LastBlock,
}

impl PromptCacheBreakpoint {
    pub fn resolve(request: &LlmRequestPayload, caps: &Capabilities) -> Self {
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
pub fn apply_prompt_cache_breakpoint(
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

/// A caller marker counts only where every marker-placing adapter carries it
/// to the provider: on a content block, on a block nested in a tool result, or
/// on a tool definition. Two positions are excluded because egress removes
/// them after this decision: a message-level key, which the OpenAI-compatible
/// adapter's message-key allowlist strips, and a whitespace-only text block,
/// which the Anthropic adapter drops. Deferring to either would leave the
/// request with no breakpoint at all. A key named `cache_control` inside tool
/// arguments or a JSON schema is user data and does not count either.
///
/// `every_counted_marker_survives_both_marker_placing_adapters` builds each
/// counted shape through both adapters and fails if one stops carrying it.
fn request_carries_cache_marker(request: &LlmRequestPayload) -> bool {
    request.messages.iter().any(message_carries_cache_marker)
        || request
            .native_tools
            .iter()
            .flatten()
            .any(|tool| tool.get("cache_control").is_some())
}

fn message_carries_cache_marker(message: &serde_json::Value) -> bool {
    match message.get("content") {
        Some(serde_json::Value::Array(blocks)) => blocks.iter().any(block_carries_cache_marker),
        Some(block @ serde_json::Value::Object(_)) => block_carries_cache_marker(block),
        _ => false,
    }
}

fn block_carries_cache_marker(block: &serde_json::Value) -> bool {
    (block.get("cache_control").is_some() && !is_whitespace_text_block(block))
        || block
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|nested| nested.iter().any(block_carries_cache_marker))
}

fn is_whitespace_text_block(block: &serde_json::Value) -> bool {
    block.get("type").and_then(serde_json::Value::as_str) == Some("text")
        && block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| text.trim().is_empty())
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
