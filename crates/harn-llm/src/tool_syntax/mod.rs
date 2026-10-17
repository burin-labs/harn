//! Byte-oriented response scanning and tool argument syntax.
//!
//! Dialect composition remains in `std/llm/tool_parse`. Vocabulary arrives as
//! data and is validated once before the scanner consumes any response bytes.

mod scan;
mod syntax;
mod ts_value_parser;

pub use syntax::{
    balanced_json_object_len, collapse_blank_lines, find_close_tag, has_object_literal_arg_start,
    ident_length, parse_object_literal_from, parse_ts_call_from, preview_str,
    render_canonical_call, scan_heredoc, strip_empty_fences, strip_thinking_tags,
    strip_tool_call_wrappers, unescape_heredoc_body, unknown_tool_feedback,
    unwrap_exact_code_wrapper, unwrap_fully_wrapping_heredoc, CloseScan, HeredocError, HeredocSpan,
};

/// Delimit a response using the dialect spec supplied by the script.
pub fn scan_units(text: &str, spec: &serde_json::Value) -> Result<serde_json::Value, String> {
    let spec = scan::ScanSpec::from_json(spec)?;
    Ok(scan::scan_units(text, &spec).to_json())
}

/// Project the leading call name and separator, or an empty object when absent.
pub fn call_head(text: &str) -> serde_json::Value {
    match scan::call_head(text) {
        Some(head) => serde_json::json!({"name": head.name, "sep": head.sep.to_string()}),
        None => serde_json::json!({}),
    }
}
