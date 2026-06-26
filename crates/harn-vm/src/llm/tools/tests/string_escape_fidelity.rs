//! Cross-channel fidelity regressions for the TypeScript tool-argument
//! parser: a tool value must decode to the SAME bytes whether the model
//! delivered it via a double/single-quoted string, a template literal, or a
//! heredoc. Two silent-corruption bugs are pinned here (both failed on
//! origin/main before the fix in `ts_value_parser.rs`):
//!
//!   1. A non-BMP scalar written as a UTF-16 surrogate pair (`😀`,
//!      the form `JSON.stringify` and most provider APIs emit) errored with
//!      `invalid \u escape` and the ENTIRE tool call was dropped.
//!   2. An unknown escape (`\d`, `\w`, `\b` regex; `\begin` LaTeX) inside a
//!      double/single-quoted string silently dropped the backslash (`"\d+"`
//!      -> `d+`), while the template-literal and heredoc channels preserved it.

use super::{json, parse_bare_calls_in_body, sample_tool_registry};

/// A non-BMP scalar delivered as a `😀` surrogate pair (😀, U+1F600)
/// must decode to that scalar, not drop the call. Source bytes literally
/// contain `😀` (non-raw Rust literal so `\\u` => backslash-u).
#[test]
fn surrogate_pair_escape_decodes_and_keeps_call() {
    let tools = sample_tool_registry();
    let src = "run({ command: \"hi \\uD83D\\uDE00 there\" })";
    let result = parse_bare_calls_in_body(src, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "surrogate pair should not error: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1, "call must not be dropped");
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("hi \u{1F600} there")
    );
}

/// The `\u{1F600}` brace form already decodes a full astral scalar — keep it
/// working (guards against a regression in the shared `parse_unicode_escape`).
#[test]
fn brace_form_nonbmp_escape_decodes() {
    let tools = sample_tool_registry();
    let src = r#"run({ command: "hi \u{1F600} there" })"#;
    let result = parse_bare_calls_in_body(src, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("hi \u{1F600} there")
    );
}

/// A lone high surrogate with no following low surrogate is genuinely invalid
/// and must still be rejected (no silent half-character, no panic).
#[test]
fn lone_high_surrogate_still_rejected() {
    let tools = sample_tool_registry();
    let src = "run({ command: \"x \\uD83D y\" })";
    let result = parse_bare_calls_in_body(src, Some(&tools));
    assert!(
        !result.errors.is_empty() || result.calls.is_empty(),
        "a lone surrogate must not silently produce a value: {:?}",
        result.calls
    );
}

/// Regex `\d+\w*` written through a double-quoted string must keep its
/// backslashes — same bytes the heredoc and template channels produce.
#[test]
fn unknown_escape_in_quoted_string_keeps_backslash() {
    let tools = sample_tool_registry();
    let result = parse_bare_calls_in_body(
        r#"edit({ action: "create", path: "re.py", content: "p = '\d+\w*'" })"#,
        Some(&tools),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("p = '\\d+\\w*'")
    );
}

/// Cross-channel byte-identity: the SAME regex content delivered via a
/// double-quoted string, a template literal, and a heredoc must decode to one
/// identical value. (Before the fix the quoted channel diverged: `d+w*`.)
#[test]
fn regex_content_identical_across_channels() {
    let tools = sample_tool_registry();
    let expected = json!("p = '\\d+\\w*'");

    let quoted = parse_bare_calls_in_body(
        r#"edit({ action: "create", path: "re.py", content: "p = '\d+\w*'" })"#,
        Some(&tools),
    );
    let template = parse_bare_calls_in_body(
        "edit({ action: \"create\", path: \"re.py\", content: `p = '\\d+\\w*'` })",
        Some(&tools),
    );
    let heredoc = parse_bare_calls_in_body(
        "edit({ action: \"create\", path: \"re.py\", content: <<EOF\np = '\\d+\\w*'\nEOF\n })",
        Some(&tools),
    );

    assert_eq!(quoted.calls[0]["arguments"]["content"], expected, "quoted");
    assert_eq!(
        template.calls[0]["arguments"]["content"], expected,
        "template"
    );
    assert_eq!(
        heredoc.calls[0]["arguments"]["content"], expected,
        "heredoc"
    );
}

/// Known escapes inside a quoted string still decode (no over-correction):
/// `\n` `\t` stay control chars, `\\` stays one backslash, `\"` stays a quote.
#[test]
fn known_escapes_in_quoted_string_unchanged() {
    let tools = sample_tool_registry();
    let result = parse_bare_calls_in_body(r#"run({ command: "a\nb\tc\\d\"e" })"#, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("a\nb\tc\\d\"e")
    );
}
