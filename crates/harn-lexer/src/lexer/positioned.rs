use super::Lexer;

impl<'src> Lexer<'src> {
    /// Start counting at a source position when re-lexing interpolated expressions.
    pub fn with_position(source: &'src str, line: usize, column: usize) -> Self {
        Self::with_position_and_offset(source, line, column, 0)
    }

    /// Start counting at an absolute source position when re-lexing a slice.
    ///
    /// `byte_offset` is the slice's byte offset in the owning source file. This
    /// keeps every emitted span internally consistent: byte offsets and
    /// line/column coordinates describe the same file-level location.
    pub fn with_position_and_offset(
        source: &'src str,
        line: usize,
        column: usize,
        byte_offset: usize,
    ) -> Self {
        Self {
            src: source,
            pos: 0,
            base: byte_offset,
            line,
            column,
        }
    }
}
