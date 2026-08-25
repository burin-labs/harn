//! Span narrowing shared by declaration-level lints.

use harn_lexer::Span;

use super::Linter;

impl Linter<'_> {
    /// Narrow a declaration span to its keyword and name for focused lint
    /// rendering. Synthetic nodes fall back to their original span.
    #[expect(
        clippy::string_slice,
        reason = "search offsets come from find results and whole-char advances over haystack"
    )]
    pub(super) fn name_anchored_span(&self, name: &str, span: Span) -> Span {
        let Some(source) = self.source else {
            return span;
        };
        if name.is_empty() || span.start >= span.end {
            return span;
        }
        let Some(region) = source.get(span.start..span.end) else {
            return span;
        };
        let haystack = region.split('\n').next().unwrap_or(region);
        let mut search_from = 0;
        while let Some(relative) = haystack[search_from..].find(name) {
            let match_start = search_from + relative;
            let match_end = match_start + name.len();
            let prev_ok = match_start == 0
                || !haystack[..match_start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_alphanumeric() || character == '_');
            let next_ok = !haystack[match_end..]
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
            if prev_ok && next_ok {
                return Span::with_offsets(
                    span.start,
                    span.start + match_end,
                    span.line,
                    span.column,
                );
            }
            search_from = match_start + name.chars().next().map_or(1, char::len_utf8);
        }
        span
    }
}
