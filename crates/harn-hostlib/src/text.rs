//! Small text utilities shared across hostlib capabilities.

/// Count the number of lines in `bytes`.
///
/// Empty input has zero lines. Otherwise the count is the number of `\n`
/// bytes plus one for a final line that is not newline-terminated, so a
/// trailing newline does not introduce a phantom empty line. This is the
/// single source of truth for line counting across the scanner, code
/// index, and process-artifact surfaces, which all expose `line_count`.
#[allow(clippy::naive_bytecount)]
pub(crate) fn count_lines(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    if bytes.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}

#[cfg(test)]
mod tests {
    use super::count_lines;

    #[test]
    fn empty_input_has_no_lines() {
        assert_eq!(count_lines(b""), 0);
    }

    #[test]
    fn trailing_newline_does_not_add_phantom_line() {
        assert_eq!(count_lines(b"line1\nline2\n"), 2);
        assert_eq!(count_lines(b"line1\n"), 1);
    }

    #[test]
    fn missing_trailing_newline_still_counts_final_line() {
        assert_eq!(count_lines(b"line1\nline2"), 2);
        assert_eq!(count_lines(b"line1"), 1);
    }
}
