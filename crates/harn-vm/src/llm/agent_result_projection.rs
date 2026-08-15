//! Projections of a finished agent session into its terminal result.
//!
//! `result.tools`, `result.trace`, and `result.visible_text` all describe one
//! run, and they disagreed when each was assembled from a different source.
//! The trace rollup in particular was fed by events nothing ever emitted, so
//! it reported no tool activity for runs whose transcript held the calls
//! (#5997). Deriving every projection here, from the session that owns the
//! facts, is what keeps them in step.

use super::trace::AgentLoopFacts;
use crate::value::VmValue;

use super::agent_session_host::{dict_get, list_items};

/// Distinct entries in first-appearance order.
///
/// `successful_tools` accumulates one entry per successful call, so a loop
/// that ran the same tool five times lists it five times. `tools_used` is the
/// set of tools the run reached, in the order it first reached them.
fn distinct_in_order(names: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    names
        .iter()
        .filter(|name| seen.insert(name.as_str()))
        .cloned()
        .collect()
}

/// Build the loop and tool facts the terminal trace summary publishes, from
/// the same session state that produces `result.tools`.
///
/// `total_duration_ms` stays `None`: nothing measures loop wall time today
/// (`started_at` is an id, not a clock reading), and a zero would read as an
/// instantaneous run rather than an absent measurement.
pub(crate) fn terminal_loop_facts(
    canonical_status: &str,
    iterations: i64,
    successful_tools: &[String],
    rejected_tools: &[String],
) -> AgentLoopFacts {
    AgentLoopFacts {
        status: canonical_status.to_string(),
        iterations: usize::try_from(iterations).unwrap_or(0),
        total_duration_ms: None,
        tool_executions: successful_tools.len(),
        tool_rejections: rejected_tools.len(),
        tools_used: distinct_in_order(successful_tools),
    }
}

/// The `String` behind a value, or `None` for every other shape.
///
/// Deliberately stricter than `display()`: a block whose `type` is not a
/// string must not be coerced into one and then matched as if it were a tag.
fn string_value(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(text) => Some(text.as_str()),
        _ => None,
    }
}

/// The user-facing prose carried by one message's `content`.
///
/// `content` is either a plain string or a list of typed content blocks, and
/// the block list is the case that bit us. Rendering it with `VmValue::display`
/// produced `[{signature: …, type: thinking}, {text: …, type: text}]`: not
/// JSON, not prose, and carrying both the model's private reasoning and the
/// opaque provider signature into the field every consumer reads as the
/// assistant's answer (#6254). Only text blocks cross this seam; everything
/// else is model-private or structural and belongs in the transcript, which
/// keeps the whole message.
fn visible_text_in_content(content: &VmValue) -> String {
    match content {
        VmValue::List(blocks) => blocks
            .iter()
            .filter_map(text_in_block)
            .collect::<Vec<_>>()
            .join("\n\n"),
        VmValue::Dict(_) => text_in_block(content).unwrap_or_default().to_string(),
        _ => content.display(),
    }
}

/// The prose in one content block, or `None` when the block is not prose.
fn text_in_block(block: &VmValue) -> Option<&str> {
    let block_type = dict_get(block, "type").and_then(string_value);
    if !crate::llm::content::is_user_facing_text_block_type(block_type) {
        return None;
    }
    dict_get(block, "text")
        .and_then(string_value)
        .filter(|text| !text.trim().is_empty())
}

/// The most recent assistant message with visible (non-reasoning) text.
pub(crate) fn last_assistant_text(snapshot: &VmValue) -> Option<String> {
    let messages_value = dict_get(snapshot, "messages")?;
    let messages = list_items(messages_value);
    for msg in messages.iter().rev() {
        let role = dict_get(msg, "role")
            .map(|v| v.display())
            .unwrap_or_default();
        if role == "assistant" {
            let visible = dict_get(msg, "content")
                .map(|v| {
                    crate::visible_text::sanitize_visible_assistant_text(
                        &visible_text_in_content(v),
                        false,
                    )
                })
                .unwrap_or_default();
            if !visible.trim().is_empty() {
                return Some(visible);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_used_collapses_repeats_and_keeps_first_use_order() {
        let successful = vec!["read".to_string(), "write".to_string(), "read".to_string()];
        let facts = terminal_loop_facts("done", 3, &successful, &["explode".to_string()]);
        assert_eq!(facts.tools_used, vec!["read", "write"]);
        // Executions count calls, not distinct tools.
        assert_eq!(facts.tool_executions, 3);
        assert_eq!(facts.tool_rejections, 1);
        assert_eq!(facts.iterations, 3);
        assert_eq!(facts.status, "done");
    }

    /// A zero here would be indistinguishable from a run that finished
    /// instantly; nothing measures loop wall time yet.
    #[test]
    fn loop_duration_is_absent_rather_than_zero() {
        let facts = terminal_loop_facts("done", 1, &[], &[]);
        assert_eq!(facts.total_duration_ms, None);
    }

    #[test]
    fn a_negative_iteration_count_does_not_wrap() {
        let facts = terminal_loop_facts("failed", -1, &[], &[]);
        assert_eq!(facts.iterations, 0);
    }
}
