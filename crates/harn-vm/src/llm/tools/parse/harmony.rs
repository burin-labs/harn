const MESSAGE: &str = "<|message|>";
const FRAME_PREFIX: &str = "<|";
const FRAME_SUFFIX: &str = "|>";
const TOOL_CALL_HEADER_PREFIX: &str = "tool_call to=";
const TOOL_CALL_OPEN_PREFIX: &str = "<tool_call";
const MALFORMED_TOOL_CALL_OPEN_PREFIX: &str = "<tool_call<|";
const MALFORMED_TOOL_CALL_CLOSE_PREFIX: &str = "</tool_call<|";
const MALFORMED_ASSISTANT_OPEN_PREFIX: &str = "<assistant<|";
const HEADER_MARKERS: &[&str] = &["start", "channel", "constrain"];
const STANDALONE_MARKERS: &[&str] = &["message", "end", "call"];

pub(super) fn consume_message_marker_after_tool_call_header(
    src: &str,
    start: usize,
    cursor: usize,
) -> Option<usize> {
    if src[cursor..].starts_with(MESSAGE) && src[start..cursor].contains(TOOL_CALL_HEADER_PREFIX) {
        Some(cursor + MESSAGE.len())
    } else {
        None
    }
}

/// Consume OpenAI Harmony frame markers that some OpenAI-compatible gpt-oss
/// routes leak into the visible text channel. Header markers are bounded to
/// the role/channel header region so a later literal `<|message|>` in prose or
/// tool arguments cannot skip intervening Harn `<tool_call>` blocks.
pub(super) fn consume_frame_marker(src: &str, cursor: usize) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if !rest.starts_with(FRAME_PREFIX) {
        return None;
    }
    let marker_end_rel = rest.find(FRAME_SUFFIX)?;
    let marker = &rest[FRAME_PREFIX.len()..marker_end_rel];
    let after_marker = cursor + marker_end_rel + FRAME_SUFFIX.len();

    if HEADER_MARKERS.contains(&marker) {
        Some(consume_header_tail(src, after_marker))
    } else if STANDALONE_MARKERS.contains(&marker) {
        Some(after_marker)
    } else {
        None
    }
}

/// Consume Harmony-native text-call headers before routing them through the
/// existing bare-call parser.
pub(super) fn consume_tool_call_line(src: &str, cursor: usize) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if !rest.starts_with(MESSAGE) {
        return None;
    }
    let after_message = cursor + MESSAGE.len();
    if !src[after_message..]
        .trim_start()
        .starts_with(TOOL_CALL_HEADER_PREFIX)
    {
        return None;
    }
    let rel_end = src[after_message..]
        .find('\n')
        .unwrap_or(src.len() - after_message);
    Some(after_message + rel_end)
}

pub(super) fn consume_corrupted_tool_call_close(src: &str, cursor: usize) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if !rest.starts_with(MALFORMED_TOOL_CALL_CLOSE_PREFIX) {
        return None;
    }
    let marker_end = rest.find(FRAME_SUFFIX)?;
    Some(cursor + marker_end + FRAME_SUFFIX.len())
}

pub(super) fn is_corrupted_tool_call_close_fragment(trimmed: &str) -> bool {
    trimmed.starts_with(MALFORMED_TOOL_CALL_CLOSE_PREFIX)
}

/// Consume malformed Harn wrapper opens split by Harmony frame tokens, e.g.
/// `<tool_call<|message|>` or `<assistant<|channel|>analysis<|message|>`.
pub(super) fn consume_corrupted_wrapper_open(src: &str, cursor: usize) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if rest.starts_with(MALFORMED_TOOL_CALL_OPEN_PREFIX) {
        let after_name = &rest["<tool_call".len()..];
        let after_marker_prefix = after_name.strip_prefix(FRAME_PREFIX)?;
        let marker_len = after_marker_prefix.find(FRAME_SUFFIX)?;
        let marker = &after_marker_prefix[..marker_len];
        if marker != "message" {
            return None;
        }
        return Some(
            cursor + "<tool_call".len() + FRAME_PREFIX.len() + marker_len + FRAME_SUFFIX.len(),
        );
    }
    if rest.starts_with(MALFORMED_ASSISTANT_OPEN_PREFIX) {
        return consume_corrupted_wrapper_header(src, cursor, "<assistant");
    }
    None
}

fn consume_header_tail(src: &str, after_marker: usize) -> usize {
    let header_tail = &src[after_marker..];
    if let Some(message) = header_tail.find(MESSAGE) {
        let first_frame = header_tail.find(FRAME_PREFIX);
        let header_prefix = &header_tail[..message];
        let before_payload =
            !header_prefix.contains('\n') && !header_prefix.contains(TOOL_CALL_OPEN_PREFIX);
        if before_payload && first_frame == Some(message) {
            return after_marker + message + MESSAGE.len();
        }
    }

    let next_frame = header_tail.find(FRAME_PREFIX);
    let next_tool_call = header_tail.find(TOOL_CALL_OPEN_PREFIX);
    let next_newline = header_tail.find('\n');
    [next_frame, next_tool_call, next_newline]
        .into_iter()
        .flatten()
        .min()
        .map_or(after_marker, |boundary| after_marker + boundary)
}

fn consume_corrupted_wrapper_header(src: &str, cursor: usize, tag: &str) -> Option<usize> {
    let rest = src.get(cursor..)?;
    if !rest.starts_with(tag) {
        return None;
    }
    let after_name = &rest[tag.len()..];
    let after_marker_prefix = after_name.strip_prefix(FRAME_PREFIX)?;
    let marker_len = after_marker_prefix.find(FRAME_SUFFIX)?;
    let marker = &after_marker_prefix[..marker_len];
    if !HEADER_MARKERS.contains(&marker) {
        return None;
    }
    Some(consume_header_tail(
        src,
        cursor + tag.len() + FRAME_PREFIX.len() + marker_len + FRAME_SUFFIX.len(),
    ))
}
