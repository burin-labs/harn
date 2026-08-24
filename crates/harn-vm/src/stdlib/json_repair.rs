//! Mechanical repair of near-JSON text.
//!
//! Split out of `stdlib::json` so the repair machinery — which is entirely
//! byte-level and shares no state with the builtins — has its own owner.
//! Repair is best-effort and syntactic only: schema validation stays with the
//! caller so a plausible-but-wrong shape is never accepted here.

use crate::stdlib::json::extract_json_from_text;

/// Mechanically repair common LLM JSON mistakes. Returns `Some` only when the
/// result parses as JSON. Schema validation stays with the caller so a
/// plausible-but-wrong shape is not accepted here.
pub(crate) fn locally_repair_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let extracted = extract_json_from_text(trimmed);
    for candidate in unique_candidates([trimmed.to_string(), extracted]) {
        if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
            return Some(candidate);
        }
        // `continue`, not `?`. A candidate this cannot fix says nothing about
        // the next one, and the raw text failing must not stop the extracted
        // span from being tried.
        let Some(repaired) = apply_mechanical_json_fixes(&candidate) else {
            continue;
        };
        if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
            return Some(repaired);
        }
    }
    None
}

fn unique_candidates(items: [String; 2]) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    for item in items {
        if !out.iter().any(|seen| seen == &item) {
            out.push(item);
        }
    }
    out
}

fn apply_mechanical_json_fixes(text: &str) -> Option<String> {
    let stripped = strip_trailing_commas(text);
    let quoted = quote_unquoted_keys(&stripped);
    close_truncated_containers(&quoted)
}

fn strip_trailing_commas(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            out.push(b as char);
            escape = false;
            i += 1;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && matches!(bytes[j], b'}' | b']') {
                i += 1;
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn quote_unquoted_keys(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 8);
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            out.push(b as char);
            escape = false;
            i += 1;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if matches!(b, b'{' | b',') {
            out.push(b as char);
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() && is_ident_start(bytes[i]) {
                let start = i;
                i += 1;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let ident = std::str::from_utf8(&bytes[start..i])
                    .expect("unquoted-key scan only accepts ASCII identifier bytes");
                if j < bytes.len() && bytes[j] == b':' {
                    out.push('"');
                    out.push_str(ident);
                    out.push('"');
                    continue;
                }
                out.push_str(ident);
                continue;
            }
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn close_truncated_containers(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    let mut last_significant = 0u8;
    for &b in bytes {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
                // A closed string is a complete value. Without this the last
                // significant byte stays whatever preceded the string — for
                // `{"k": "v"` that is the `:`, and the truncation would be
                // rejected as a dangling key rather than closed.
                last_significant = b'"';
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                match stack.pop() {
                    Some(expected) if expected == b => {}
                    _ => return None,
                }
                last_significant = b;
            }
            b if b.is_ascii_whitespace() => {}
            other => last_significant = other,
        }
    }
    if in_string || matches!(last_significant, b':' | b',') {
        return None;
    }
    let mut out = text.to_string();
    while let Some(close) = stack.pop() {
        out.push(close as char);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locally_repairs_trailing_comma_unquoted_key_preamble_and_truncation() {
        assert_eq!(
            locally_repair_json(r#"{"verdict": "PASS",}"#).as_deref(),
            Some(r#"{"verdict": "PASS"}"#)
        );
        assert_eq!(
            locally_repair_json("{verdict: \"PASS\"}").as_deref(),
            Some(r#"{"verdict": "PASS"}"#)
        );
        assert_eq!(
            locally_repair_json("Here you go: {\"verdict\": \"PASS\"} thanks.").as_deref(),
            Some(r#"{"verdict": "PASS"}"#)
        );
        assert_eq!(
            locally_repair_json("Here you go: {\"verdict\": \"PASS\",} thanks.").as_deref(),
            Some(r#"{"verdict": "PASS"}"#)
        );
        assert_eq!(
            locally_repair_json(r#"{"verdict": "PASS""#).as_deref(),
            Some(r#"{"verdict": "PASS"}"#)
        );
    }

    #[test]
    fn locally_repair_rejects_unrecoverable_and_incomplete_values() {
        assert_eq!(locally_repair_json("nothing useful here at all"), None);
        assert_eq!(locally_repair_json(r#"{"verdict":"#), None);
        assert_eq!(locally_repair_json(r#"{"verdict": "PA"#), None);
    }
}
