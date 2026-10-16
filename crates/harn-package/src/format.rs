//! Source and command encoding shared with command-line projections.

use std::path::Path;

/// Quote a path using the same POSIX encoding as command-line output.
pub fn shell_quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

/// Escape every control character forbidden in a TOML basic string.
pub fn escape_toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Render a full TOML basic string literal.
pub fn toml_basic_string_literal(value: &str) -> String {
    format!("\"{}\"", escape_toml_basic_string(value))
}

/// Distinguish drive paths from URL schemes before package source parsing.
pub fn looks_like_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}
