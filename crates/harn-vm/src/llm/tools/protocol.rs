pub(crate) const TEXT_TOOL_CALL_TAG: &str = "tool_call";
pub(crate) const TEXT_TOOL_CALL_TAG_COMPACT: &str = "toolcall";
pub(crate) const TEXT_TOOL_CALL_OPEN: &str = "<tool_call>";
pub(crate) const TEXT_TOOL_CALL_CLOSE: &str = "</tool_call>";
pub(crate) const TEXT_TOOL_CALL_OPEN_COMPACT: &str = "<toolcall>";
pub(crate) const TEXT_TOOL_CALL_CLOSE_COMPACT: &str = "</toolcall>";
pub(crate) const ASSISTANT_PROSE_OPEN: &str = "<assistant_prose>";
pub(crate) const ASSISTANT_PROSE_CLOSE: &str = "</assistant_prose>";

pub(crate) fn text_tool_call_block(body: &str) -> String {
    format!("{TEXT_TOOL_CALL_OPEN}\n{body}\n{TEXT_TOOL_CALL_CLOSE}")
}

pub(crate) fn assistant_prose_block(body: &str) -> String {
    format!("{ASSISTANT_PROSE_OPEN}\n{body}\n{ASSISTANT_PROSE_CLOSE}")
}

pub(crate) fn text_tool_call_tag_pairs() -> [(&'static str, &'static str); 2] {
    [
        (TEXT_TOOL_CALL_OPEN, TEXT_TOOL_CALL_CLOSE),
        (TEXT_TOOL_CALL_OPEN_COMPACT, TEXT_TOOL_CALL_CLOSE_COMPACT),
    ]
}
