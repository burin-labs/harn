//! Shared, lightweight formatting helpers for CLI commands.
//!
//! Multiple subcommands had grown private copies of the same RFC3339
//! timestamp / human-friendly duration formatters; consolidating them
//! here keeps formatting consistent across the user-facing surface.

use std::path::Path;
use std::time::Duration as StdDuration;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Render a UTC instant as RFC3339, falling back to `Display` if formatting
/// fails (which `time` only does on internally inconsistent components).
pub(crate) fn format_timestamp_rfc3339(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

/// Render a unix-epoch millisecond timestamp as RFC3339.
pub(crate) fn format_unix_ms_rfc3339(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let value = OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format_timestamp_rfc3339(value)
}

/// Render a `std::time::Duration` as a coarse "5s / 2m / 3h / 1d / 1w" suffix.
///
/// Rounds toward zero on the chosen unit so status output does not overstate
/// elapsed time.
pub(crate) fn format_duration_coarse(value: StdDuration) -> String {
    if value.as_secs() == 0 {
        return format!("{}ms", value.as_millis());
    }
    let seconds = value.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 60 * 60 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 60 * 60 * 24 {
        return format!("{}h", seconds / (60 * 60));
    }
    if seconds < 60 * 60 * 24 * 7 {
        return format!("{}d", seconds / (60 * 60 * 24));
    }
    if seconds.is_multiple_of(60 * 60 * 24 * 7) {
        return format!("{}w", seconds / (60 * 60 * 24 * 7));
    }
    format!("{}d", seconds / (60 * 60 * 24))
}

/// Quote a path for inclusion in a copy-pasteable shell command line.
///
/// Delegates to `shell_words::quote` so every emitted command line shares one
/// definition of "safe to leave bare"; non-UTF-8 components are rendered
/// lossily, matching how the path would be displayed elsewhere.
pub(crate) fn shell_quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

/// Render a millisecond duration with a single decimal point of precision
/// for the ">= 1s" cases (used by portal output).
pub(crate) fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms >= 60_000 {
        format!("{:.1}m", duration_ms as f64 / 60_000.0)
    } else if duration_ms >= 1_000 {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

/// Escape the `|` characters in `value` so it can sit inside a single Markdown
/// table cell without prematurely ending the column. Several report commands
/// (eval summaries, provider matrices, diagnostics catalogs) build Markdown
/// tables and had each grown a private copy of this; keep the escaping uniform.
pub(crate) fn escape_md(value: &str) -> String {
    value.replace('|', "\\|")
}

/// Escape `&`, `<`, `>`, `"`, and `'` for embedding text in HTML or XML
/// output. Uses `&#39;` for the apostrophe because it is valid in both
/// (`&apos;` is XML-only). The eval-prompt report, MCP landing page, OAuth
/// callback page, and JUnit report writer had each grown a private copy.
pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape a string for a TOML basic (double-quoted) string: backslash,
/// double quote, and the control characters TOML forbids raw in basic
/// strings. `harn rules` and `harn connector` scaffolding had divergent
/// copies — the connector one skipped control characters, so a value with a
/// newline produced invalid TOML.
pub(crate) fn escape_toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // Remaining C0 control chars (and DEL) must be escaped in TOML
            // basic strings.
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Render a full TOML basic string literal.
pub(crate) fn toml_basic_string_literal(value: &str) -> String {
    format!("\"{}\"", escape_toml_basic_string(value))
}

/// Normalize path separators in machine-readable output. Harn package and
/// bundle artifacts use slash-separated logical paths even on Windows.
pub(crate) fn slash_separators(value: &str) -> String {
    value.replace('\\', "/")
}

/// Render a path with slash separators for deterministic JSON/report output.
pub(crate) fn slash_path(path: &Path) -> String {
    slash_separators(&path.to_string_lossy())
}

/// True when a string starts with Windows drive syntax such as `C:\`, `C:/`,
/// or drive-relative `C:foo`. URL parsers otherwise treat these as scheme `c`.
pub(crate) fn looks_like_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_uses_rfc3339() {
        let value = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(format_timestamp_rfc3339(value), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn escape_html_handles_all_five_specials() {
        assert_eq!(
            escape_html(r#"<a href="x">&'b'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;b&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn escape_toml_basic_string_escapes_control_characters() {
        assert_eq!(
            escape_toml_basic_string("a\"b\\c\nd\te\rf"),
            "a\\\"b\\\\c\\nd\\te\\rf"
        );
        // Other C0 controls (the case the old connector copy missed
        // entirely) become \uXXXX escapes.
        assert_eq!(escape_toml_basic_string("bell\u{7}"), "bell\\u0007");
    }

    #[test]
    fn toml_basic_string_literal_round_trips_windows_paths() {
        let raw = r"C:\Users\RUNNER~1\AppData\Local\Temp\.tmpJHS6sR\coding-pack";
        let parsed: toml::Value =
            toml::from_str(&format!("path = {}\n", toml_basic_string_literal(raw))).unwrap();
        assert_eq!(parsed.get("path").and_then(toml::Value::as_str), Some(raw));
    }

    #[test]
    fn slash_path_normalizes_windows_separators() {
        assert_eq!(
            slash_separators(r"C:\tmp\pkg\lib.harn"),
            "C:/tmp/pkg/lib.harn"
        );
    }

    #[test]
    fn detects_windows_drive_paths_without_url_parser() {
        assert!(looks_like_windows_drive_path(r"C:\tmp\registry.toml"));
        assert!(looks_like_windows_drive_path("D:/tmp/registry.toml"));
        assert!(looks_like_windows_drive_path("E:relative"));
        assert!(!looks_like_windows_drive_path(
            "https://example.com/index.toml"
        ));
        assert!(!looks_like_windows_drive_path("/tmp/index.toml"));
    }

    #[test]
    fn unix_ms_rounds_to_seconds() {
        assert_eq!(format_unix_ms_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_ms_rfc3339(1500), "1970-01-01T00:00:01Z");
        // Negative ms before the epoch should not panic.
        assert_eq!(format_unix_ms_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn coarse_duration_picks_a_unit() {
        assert_eq!(format_duration_coarse(StdDuration::from_millis(0)), "0ms");
        assert_eq!(format_duration_coarse(StdDuration::from_secs(5)), "5s");
        assert_eq!(format_duration_coarse(StdDuration::from_mins(2)), "2m");
        assert_eq!(format_duration_coarse(StdDuration::from_hours(2)), "2h");
        assert_eq!(format_duration_coarse(StdDuration::from_hours(72)), "3d");
        assert_eq!(format_duration_coarse(StdDuration::from_hours(336)), "2w");
    }

    #[test]
    fn ms_duration_uses_one_decimal_above_a_second() {
        assert_eq!(format_duration_ms(500), "500ms");
        assert_eq!(format_duration_ms(1_500), "1.5s");
        assert_eq!(format_duration_ms(90_000), "1.5m");
    }

    #[test]
    fn escape_md_escapes_only_pipes() {
        assert_eq!(escape_md("a|b|c"), "a\\|b\\|c");
        assert_eq!(escape_md("no pipes here"), "no pipes here");
        assert_eq!(escape_md(""), "");
    }
}
