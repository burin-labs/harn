//! Building the tool-result message a provider will accept, and repairing the
//! transcript when a turn's tool calls were never dispatched.
//!
//! The shape a result takes is not the caller's choice: it follows the channel
//! the run is on. On the text channel a result rides back as an ordinary
//! `user` message, because there is no provider tool-result role there. On the
//! native channel it takes the provider's own role — Anthropic
//! `tool_result`/`tool_use_id`, OpenAI `tool`/`tool_call_id`.
//!
//! The repair half exists because Anthropic rejects an assistant `tool_use`
//! block that is not immediately followed by its `tool_result` with a
//! non-retryable 400. Any inject site that declines to dispatch a turn's calls
//! has to close them first.

use super::{dict_get, list_items, vm_to_json};
use crate::value::{VmDictExt, VmValue};

pub(super) fn tool_result_message_for_provider(
    provider: &str,
    model: &str,
    tool_format: &str,
    name: &str,
    tool_call_id: &str,
    observation: &str,
    screenshots: &[VmValue],
) -> VmValue {
    let mut msg = crate::value::DictMap::new();
    // A text-channel tool_format (`text` or `json`) carries tool results back
    // as an ordinary `user` message — there is no provider tool-result role on
    // the text channel. `native` uses the provider's tool_result/tool role.
    let is_text_channel = matches!(
        crate::llm_config::tool_format_channel(tool_format),
        Some(crate::llm_config::ToolFormatChannel::Text)
    );
    if is_text_channel {
        msg.put_str("role", "user");
    } else if crate::llm::provider::provider_uses_anthropic_messages(provider, model) {
        msg.put_str("role", "tool_result");
        msg.put_str("tool_use_id", tool_call_id);
    } else {
        msg.put_str("role", "tool");
        msg.put_str("name", name);
        if !tool_call_id.is_empty() {
            msg.put_str("tool_call_id", tool_call_id);
        }
    }
    // A tool that returned a screenshot (the computer tool) carries the image
    // back to the model as a `[text, screenshot]` content list on EVERY channel,
    // including the text channel (`role:"user"`). Computer use is only meaningful
    // for a vision-capable model — the surface never offers the tool to a
    // non-vision route — and every provider accepts an image content part in the
    // message role this builds (Anthropic `tool_result`/`user`, OpenAI `user`
    // after relocation, text-channel `user`). The provider content mappers
    // (`anthropic_content`/`openai_content`) project the neutral screenshot dict
    // into the provider's image block; Anthropic's egress additionally wraps both
    // `role:"tool"` and `role:"tool_result"` messages into a `tool_result` block,
    // so delivery does not depend on the session's last provider/model being set
    // at record time (it often is not). `is_text_channel` only decides the ROLE
    // above, not whether the image rides along.
    //
    // The text part is a SHORT summary, never the raw observation: a screenshot
    // observation is ~800 KB of base64 (the display-string of the ScreenImage),
    // which would bloat every subsequent turn and is redundant once the image
    // itself rides along. A result with no screenshot is byte-identical to before.
    if screenshots.is_empty() {
        msg.put_str("content", observation);
    } else {
        // `[text, image, image, ...]` — a tool that returns more than one frame
        // (rare today; one-per-action) delivers every image, not just the first.
        let summary = screenshot_result_summary(observation);
        let mut text_block = crate::value::DictMap::new();
        text_block.put_str("type", "text");
        text_block.put_str("text", &summary);
        let mut content = Vec::with_capacity(1 + screenshots.len());
        content.push(VmValue::dict(text_block));
        content.extend(screenshots.iter().cloned());
        msg.put("content", VmValue::List(std::sync::Arc::new(content)));
    }
    VmValue::dict(msg)
}

/// A short text summary for a screenshot tool result, stripped of the giant
/// base64 payload that the display-stringified `ScreenImage` carries. Keeps any
/// leading `[result of ...]` framing and the `text` field's human summary if one
/// is recoverable, else a generic note; the image itself rides alongside as a
/// content block, so the text only needs to orient the model.
fn screenshot_result_summary(observation: &str) -> String {
    // Pull the handler's short `text: "..."` summary out of the display string
    // when present (e.g. `text: Clicked left at (100, 200). Captured ...`).
    if let Some(idx) = observation.find("text: ") {
        let rest = &observation[idx + "text: ".len()..];
        let end = rest.find(", screenshot:").unwrap_or(rest.len());
        let candidate = rest[..end].trim();
        if !candidate.is_empty() && candidate.len() < 400 {
            return format!("{candidate} (screenshot attached below)");
        }
    }
    "Screenshot captured (attached below).".to_string()
}

/// The neutral screenshot dict a computer-use tool returns (`ScreenImage`:
/// `{base64, media_type, width, height, scale_factor}`), pulled off a raw tool
/// result so it can ride back to the model as an image content block. Searches
/// the WHOLE result tree for a dict that actually carries a non-empty `base64`
/// plus `scale_factor` (the distinctive ScreenImage signature). A recursive
/// search — not a fixed `result.screenshot` path — because the live dispatch
/// wraps the handler's return through the tool-caller middleware stack
/// (redaction/summary layers nest it under `result`/`value`), so the screenshot
/// sits at a path the caller cannot assume. The `scale_factor` + non-empty
/// `base64` pair is distinctive enough not to misfire on an unrelated field, and
/// the base64 the model needs is elided to a marker string in the rendered TEXT,
/// so this never matches that. Returns EVERY screenshot found (in tree order) as
/// unconverted `VmValue` dicts — the provider content mappers do the image-block
/// projection at egress. A tool that returns one frame yields one; a
/// multi-frame result delivers them all rather than dropping the extras.
pub(super) fn screenshots_from_tool_result(result: &VmValue) -> Vec<VmValue> {
    fn collect(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        if crate::llm::content::is_screenshot_dict(value) {
            out.push(value.clone());
            // A screenshot dict has no nested screenshots — don't recurse in.
            return;
        }
        match value {
            serde_json::Value::Object(map) => {
                for nested in map.values() {
                    collect(nested, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            _ => {}
        }
    }
    let json = vm_to_json(result);
    let mut found = Vec::new();
    collect(&json, &mut found);
    found.iter().map(crate::stdlib::json_to_vm_value).collect()
}

/// The `(id, name)` of one provider-native tool-call block carried on an
/// assistant message, recovered across the three wire shapes the transcript
/// builder emits (`build_assistant_tool_message`):
///
///   - Anthropic: `content` is a list of blocks; `{type: "tool_use", id, name}`.
///   - OpenAI / Ollama: a top-level `tool_calls` list of
///     `{id, function: {name}}`.
///   - Gemini: `content` list of `{functionCall: {name, id?}}` (id optional).
pub(super) struct AssistantToolUse {
    id: String,
    name: String,
}

/// Extract every provider-native tool-call block declared on an assistant
/// message, regardless of the provider wire shape it was persisted in. Text-
/// channel turns keep their calls inline in `content` (a plain string), so they
/// carry no structured blocks and yield an empty list — which is exactly why the
/// repair below is a no-op for homogeneous text-format runs.
pub(super) fn assistant_tool_use_blocks(message: &VmValue) -> Vec<AssistantToolUse> {
    let mut blocks = Vec::new();
    // OpenAI / Ollama: top-level `tool_calls`.
    for call in list_items(
        &dict_get(message, "tool_calls")
            .cloned()
            .unwrap_or(VmValue::Nil),
    ) {
        let id = dict_get(&call, "id")
            .map(|v| v.display())
            .unwrap_or_default();
        let name = dict_get(&call, "name")
            .map(|v| v.display())
            .or_else(|| {
                dict_get(&call, "function").and_then(|f| dict_get(f, "name").map(|v| v.display()))
            })
            .unwrap_or_default();
        blocks.push(AssistantToolUse { id, name });
    }
    // Anthropic / Gemini: structured `content` blocks.
    if let Some(content) = dict_get(message, "content") {
        for block in list_items(content) {
            // Anthropic `tool_use`.
            let block_type = dict_get(&block, "type")
                .map(|v| v.display())
                .unwrap_or_default();
            if block_type == "tool_use" {
                let id = dict_get(&block, "id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let name = dict_get(&block, "name")
                    .map(|v| v.display())
                    .unwrap_or_default();
                blocks.push(AssistantToolUse { id, name });
                continue;
            }
            // Gemini `functionCall` part (id is optional on this wire).
            if let Some(function_call) = dict_get(&block, "functionCall") {
                let id = dict_get(function_call, "id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let name = dict_get(function_call, "name")
                    .map(|v| v.display())
                    .unwrap_or_default();
                blocks.push(AssistantToolUse { id, name });
            }
        }
    }
    blocks
}

/// The set of `tool_use`/`tool_call` ids that ALREADY have a paired tool-result
/// message somewhere in the transcript. Used so the repair only synthesizes a
/// result for a genuinely orphaned block and stays a no-op when the loop already
/// dispatched (and recorded) the call. Covers both provider tool-result roles
/// (`tool_result`/`tool_use_id`, `tool`/`tool_call_id`) and the text-channel
/// `user` echo (which carries no id, so it never satisfies a native id — correct,
/// since a native tool_use ALWAYS needs a real tool-result role).
pub(super) fn paired_tool_result_ids(messages: &[VmValue]) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for message in messages {
        let role = dict_get(message, "role")
            .map(|v| v.display())
            .unwrap_or_default();
        if role != "tool_result" && role != "tool" {
            continue;
        }
        let id = dict_get(message, "tool_use_id")
            .or_else(|| dict_get(message, "tool_call_id"))
            .map(|v| v.display())
            .unwrap_or_default();
        if !id.is_empty() {
            ids.insert(id);
        }
    }
    ids
}

/// Synthesize a provider-valid tool-result message for each orphaned
/// `tool_use`/`tool_call` block on `last_assistant`, carrying `feedback` as the
/// observation. Blocks whose id already has a paired result (`already_paired`)
/// are skipped. Returns the messages to append, in block order. Pure over its
/// inputs so the invariant is unit-testable without a live session.
///
/// A block reaching this function came from `assistant_tool_use_blocks`, which
/// only yields provider-native `tool_use`/`tool_call` structures — text/json
/// channels carry their calls inline in `content` and produce NO structured
/// blocks. So an orphaned block here is native *by definition*, and its result
/// MUST be synthesized in the provider's native shape (anthropic
/// `tool_result`+`tool_use_id`, openai `tool`+`tool_call_id`) regardless of the
/// session's tool_format lock. That lock is pinned to the PRIMARY model's format
/// at session init (`claim_tool_format`) and never re-claimed on escalation, so
/// on a text-primary run it stays `"text"` for the whole run — passing it here
/// would (mis)route the synthesis through the text-channel `role:"user"` echo,
/// leaving the native `tool_use` block orphaned and re-triggering the exact
/// Anthropic 400 this repair exists to prevent. Force `"native"` instead.
pub(super) fn synthesize_orphan_tool_results(
    last_assistant: &VmValue,
    provider: &str,
    model: &str,
    feedback: &str,
    already_paired: &std::collections::BTreeSet<String>,
) -> Vec<VmValue> {
    let mut out = Vec::new();
    for block in assistant_tool_use_blocks(last_assistant) {
        if !block.id.is_empty() && already_paired.contains(&block.id) {
            continue;
        }
        out.push(tool_result_message_for_provider(
            provider,
            model,
            // The orphaned block is a structured native tool_use; its result
            // must ride the provider's native tool-result role, not the
            // session-locked (possibly `text`) channel.
            "native",
            &block.name,
            &block.id,
            feedback,
            // A synthesized orphan-repair result is plain feedback text — never
            // an image.
            &[],
        ));
    }
    out
}
