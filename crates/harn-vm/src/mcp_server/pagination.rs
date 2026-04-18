use super::DEFAULT_PAGE_SIZE;

/// Encode an offset as a base64 cursor string.
pub(super) fn encode_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

/// Decode a cursor from the request params, returning `(offset, page_size)`.
pub(super) fn parse_cursor(params: &serde_json::Value) -> (usize, usize) {
    let offset = params
        .get("cursor")
        .and_then(|c| c.as_str())
        .and_then(|c| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(c).ok()?;
            let s = String::from_utf8(bytes).ok()?;
            s.parse::<usize>().ok()
        })
        .unwrap_or(0);
    (offset, DEFAULT_PAGE_SIZE)
}
