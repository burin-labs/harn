use harn_lexer::Lexer;

pub(super) fn interpolation_lexer(
    source: Option<&str>,
    segment: &str,
    line: usize,
    column: usize,
) -> Lexer {
    let byte_offset = source
        .and_then(|source| harn_lexer::byte_offset_for_position(source, line, column))
        .unwrap_or_default();
    Lexer::with_position_and_offset(segment, line, column, byte_offset)
}
