//! ANSI escape-sequence stripping.

/// Remove ANSI/VT escape sequences from `text`, leaving the printable payload.
///
/// Backed by a real VT parser, so this covers OSC (`ESC ]`), DCS, and 8-bit
/// CSI introducers in addition to the SGR colour codes a hand-rolled
/// `ESC [` … alpha scanner catches. That matters wherever captured terminal
/// output is forwarded into an LLM prompt: an unstripped OSC title sequence
/// is model-visible noise at best.
pub fn strip_ansi(text: &str) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(text)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_colour_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m text"), "red text");
    }

    #[test]
    fn strips_osc_sequences() {
        // The gap in the hand-rolled scanner this replaced: it only understood
        // `ESC [`, so an OSC window-title sequence leaked through verbatim.
        assert_eq!(strip_ansi("\x1b]0;title\x07after"), "after");
        // String-terminator form as well as BEL.
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\after"), "after");
    }

    #[test]
    fn leaves_plain_text_alone() {
        assert_eq!(strip_ansi("plain [not] an escape"), "plain [not] an escape");
    }
}
