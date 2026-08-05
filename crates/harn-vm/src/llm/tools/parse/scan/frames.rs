//! Harmony frame-token consumption, driven by the vocabulary in [`HarmonyVocab`].
//!
//! Some OpenAI-compatible gpt-oss routes leak Harmony frame tokens into the
//! visible text channel. These helpers answer only "how far does this frame
//! marker extend", never "what does it mean" — the unit the scanner emits for
//! a consumed marker carries no interpretation.
//!
//! The shapes mirror `parse::harmony`, which holds the same rules against the
//! constants the current parser still uses. That module stays live until the
//! composition lanes cut over; the difference here is that every marker,
//! prefix, and corrupted opener arrives as data.

use super::spec::HarmonyVocab;

/// A `<|message|>` that closes a `tool_call to=…` header the caller already
/// scanned past. Returns the offset just after the marker.
pub(super) fn message_marker_after_tool_call_header(
    vocab: &HarmonyVocab,
    src: &str,
    start: usize,
    cursor: usize,
) -> Option<usize> {
    let closes_header = src[cursor..].starts_with(&vocab.message_marker)
        && src[start..cursor].contains(&vocab.tool_call_header_prefix);
    closes_header.then(|| cursor + vocab.message_marker.len())
}

/// Consume a frame marker at `cursor`. A header marker takes its header tail
/// with it; a standalone marker consumes only itself. An unlisted marker is
/// not a frame at all.
pub(super) fn frame_marker(
    vocab: &HarmonyVocab,
    call_tag_opens: &[String],
    src: &str,
    cursor: usize,
) -> Option<usize> {
    if vocab.frame_prefix.is_empty() || vocab.frame_suffix.is_empty() {
        return None;
    }
    let rest = src.get(cursor..)?;
    if !rest.starts_with(&vocab.frame_prefix) {
        return None;
    }
    let marker_end_rel = rest.find(&vocab.frame_suffix)?;
    if marker_end_rel < vocab.frame_prefix.len() {
        return None;
    }
    let marker = &rest[vocab.frame_prefix.len()..marker_end_rel];
    let after_marker = cursor + marker_end_rel + vocab.frame_suffix.len();

    if vocab.header_markers.iter().any(|entry| entry == marker) {
        Some(header_tail(vocab, call_tag_opens, src, after_marker))
    } else if vocab.standalone_markers.iter().any(|entry| entry == marker) {
        Some(after_marker)
    } else {
        None
    }
}

/// Consume a Harmony-native text-call header line — `<|message|>tool_call
/// to=…` through end of line. The caller reports the line as prose; the
/// downstream sniffer is what recovers any call inside it.
pub(super) fn tool_call_line(vocab: &HarmonyVocab, src: &str, cursor: usize) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if !rest.starts_with(&vocab.message_marker) {
        return None;
    }
    let after_message = cursor + vocab.message_marker.len();
    if !src[after_message..]
        .trim_start()
        .starts_with(&vocab.tool_call_header_prefix)
    {
        return None;
    }
    let rel_end = src[after_message..]
        .find('\n')
        .unwrap_or(src.len() - after_message);
    Some(after_message + rel_end)
}

/// Consume a wrapper open or close that a frame token corrupted mid-tag, e.g.
/// `<tool_call<|message|>` or `<assistant<|channel|>analysis<|message|>`.
///
/// Each corrupted opener is a tag fragment that runs into the frame prefix, so
/// the extent is the fragment plus whatever the frame marker at its tail
/// consumes. An opener that does not end in the frame prefix is not a frame
/// corruption and is left for the ordinary tag branches.
pub(super) fn corrupted_opener(
    vocab: &HarmonyVocab,
    call_tag_opens: &[String],
    src: &str,
    cursor: usize,
) -> Option<usize> {
    let rest = src.get(cursor..)?;
    let opener = vocab
        .corrupted_openers
        .iter()
        .filter(|opener| opener.ends_with(&vocab.frame_prefix))
        .find(|opener| rest.starts_with(opener.as_str()))?;
    let frame_at = cursor + opener.len() - vocab.frame_prefix.len();
    frame_marker(vocab, call_tag_opens, src, frame_at)
}

/// Where a role/channel header ends.
///
/// A header that reaches its `<|message|>` with no intervening frame, newline,
/// or call-tag open is consumed through that marker — the payload starts after
/// it. Otherwise the header stops at whichever of those boundaries comes
/// first, so a later literal `<|message|>` in prose or in a tool argument
/// cannot swallow intervening blocks.
fn header_tail(
    vocab: &HarmonyVocab,
    call_tag_opens: &[String],
    src: &str,
    after_marker: usize,
) -> usize {
    let tail = &src[after_marker..];
    let next_frame = tail.find(&vocab.frame_prefix);
    let next_call_tag = call_tag_opens
        .iter()
        .filter_map(|open| tail.find(open.as_str()))
        .min();
    if let Some(message) = tail.find(&vocab.message_marker) {
        let prefix = &tail[..message];
        let before_payload = !prefix.contains('\n')
            && !call_tag_opens
                .iter()
                .any(|open| prefix.contains(open.as_str()));
        if before_payload && next_frame == Some(message) {
            return after_marker + message + vocab.message_marker.len();
        }
    }
    [next_frame, next_call_tag, tail.find('\n')]
        .into_iter()
        .flatten()
        .min()
        .map_or(after_marker, |boundary| after_marker + boundary)
}
