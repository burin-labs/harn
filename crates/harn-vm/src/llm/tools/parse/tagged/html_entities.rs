//! HTML entity decoding for text-channel tool-call JSON arguments.
//!
//! Text-format models escape markup delimiters inside JSON string values;
//! this module is the single owning decode boundary for that channel.

/// Decode the standard HTML character references a text-format model emits when
/// it escapes its own markup delimiters. Text-tool models trained on
/// angle-bracket framing (Harmony/gpt-oss `<|channel|>…<|message|>`, the
/// `<tool_call>` wrapper) routinely HTML-escape `<`, `>`, and `&` inside their
/// tool-call *string arguments* so a literal operator cannot be mistaken for a
/// frame or tag boundary — shipping `if (a &lt;= b)`, `xs.map(x =&gt; x)`, or
/// `a &amp;&amp; b` as the file content. Left encoded, that source cannot
/// compile.
///
/// The scan is SINGLE-PASS and resolves each reference exactly once: after
/// substituting `&amp;` it advances past the produced `&` without re-scanning
/// it, so a value the model *double*-escaped — `&amp;lt;`, the on-the-wire form
/// of a literal `&lt;` — decodes to `&lt;` (the intended literal) rather than
/// collapsing to `<`. An `&` that does not open a recognized reference (a bare
/// `R&D`, a shell `a & b`, a truncated `&amp`) is emitted verbatim, so only
/// genuine references are touched. Callers MUST invoke this at most once per
/// value; a second pass is not idempotent for double-escaped input.
pub(super) fn decode_html_entities(raw: &str) -> String {
    // Named/numeric references this decoder recognizes. Kept to the operators a
    // model escapes to protect markup framing plus the two quote forms; an
    // unlisted reference is left verbatim rather than guessed at.
    const NAMED: &[(&str, char)] = &[
        ("amp;", '&'),
        ("lt;", '<'),
        ("gt;", '>'),
        ("quot;", '"'),
        ("apos;", '\''),
        ("#39;", '\''),
        ("#34;", '"'),
    ];
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some((token, decoded)) = NAMED.iter().find(|(token, _)| after.starts_with(token)) {
            out.push(*decoded);
            rest = &after[token.len()..];
        } else {
            // Not a recognized reference: emit the `&` literally and keep
            // scanning the remainder for further references.
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Decode HTML character references in every string value reachable in a parsed
/// tool-call `arguments` payload, recursing through nested objects and arrays.
/// This is the single owning boundary for the operator-corruption class: the
/// value arrived through a JSON-string channel where the model escaped its
/// markup delimiters, so it is decoded here exactly once as the call is
/// finalized. Heredoc/raw-body channels never pass through here — their
/// delimiters are sentinel lines, not angle brackets, so the model writes raw
/// operators there and any literal reference (e.g. authored HTML) must survive
/// untouched.
pub(super) fn decode_html_entities_in_args(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let decoded = decode_html_entities(text);
            if decoded != *text {
                *text = decoded;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                decode_html_entities_in_args(item);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                decode_html_entities_in_args(item);
            }
        }
        _ => {}
    }
}
