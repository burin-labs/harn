//! Streamed-delta canonicalization across chunk boundaries.

use crate::llm::providers::openai_compat::streaming::DeltaCanonicalizer;

#[test]
fn delta_canonicalizer_reassembles_split_wire_delimiters() {
    // The wire delimiter arrives split across token deltas; the canonical
    // form must still be emitted intact in order.
    let mut c = DeltaCanonicalizer::default();
    let chunks = [
        "[[",
        "CALL",
        "]]",
        "\nlook({ a: 1 })\n",
        "[[",
        "/CALL",
        "]]",
        " done",
    ];
    let mut out = String::new();
    for ch in chunks {
        out.push_str(&c.push(ch));
    }
    out.push_str(&c.flush());
    assert_eq!(out, "<tool_call>\nlook({ a: 1 })\n</tool_call> done");
}

#[test]
fn delta_canonicalizer_matches_whole_string_remap_on_real_response() {
    // Streaming/non-streaming parity at the live-delta boundary: feeding the
    // real captured wire-form completion through the streaming
    // `DeltaCanonicalizer` (arbitrary chunk splits, including inside the
    // `[[CALL]]` opener and the heredoc body) must yield exactly the same
    // canonical text as the non-streaming whole-string `wire_to_canonical`.
    let wire = include_str!("../../../testdata/qwen36_reserved_token_response.txt");
    let expected = crate::llm::tool_delimiter::wire_to_canonical(wire);

    let mut c = DeltaCanonicalizer::default();
    let mut out = String::new();
    let bytes = wire.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let mut end = (i + 5).min(bytes.len());
        while end < bytes.len() && !wire.is_char_boundary(end) {
            end += 1;
        }
        out.push_str(&c.push(&wire[i..end]));
        i = end;
    }
    out.push_str(&c.flush());

    assert_eq!(
        out, expected,
        "streamed delta canonicalization must equal the non-streaming whole-string remap"
    );
    assert!(out.contains("<tool_call>") && !out.contains("[[CALL]]"));
}

#[test]
fn delta_canonicalizer_passes_through_plain_text() {
    let mut c = DeltaCanonicalizer::default();
    let mut out = String::new();
    for ch in ["Here is ", "some [pro", "se] text."] {
        out.push_str(&c.push(ch));
    }
    out.push_str(&c.flush());
    assert_eq!(out, "Here is some [prose] text.");
}
