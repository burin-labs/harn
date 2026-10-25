//! Source text is untrusted wherever it is shown. One predicate decides which
//! characters could break a line, reorder text, or hide in it, for every
//! surface that displays signal fields: the prompt renderer and the terminal.

/// Control characters (CR, LF, tab, escape, and the C1 set including NEL),
/// Unicode line and paragraph separators, bidirectional marks, overrides and
/// isolates (including the Arabic letter mark), zero-width characters, and
/// tag characters.
pub fn is_unsafe_display_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// `text` with every [`is_unsafe_display_char`] replaced by a space.
pub fn display_safe(text: &str) -> String {
    text.chars()
        .map(|ch| if is_unsafe_display_char(ch) { ' ' } else { ch })
        .collect()
}
