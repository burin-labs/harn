//! HTML character-reference decoding for the text tool-call lane.
//!
//! A text-format model trained on angle-bracket framing (Harmony/gpt-oss
//! `<|channel|>…<|message|>`, the `<tool_call>` wrapper) escapes its own markup
//! delimiters inside tool-call *string arguments* so a literal operator cannot
//! be mistaken for a frame or tag boundary. Left encoded, that source reaches
//! disk as `if (a &lt;= b)` / `xs.map(x =&gt; x)` / `a &amp;&amp; b` — bytes
//! that cannot compile. These pin the decode at the JSON-string parse boundary:
//! operators are recovered exactly once, while raw-body (heredoc) content and
//! genuinely-literal references survive untouched.

use super::{
    json, parse_bare_calls_in_body, parse_text_tool_calls_with_tools, sample_tool_registry,
};

/// Parse a single `<tool_call>{…json…}</tool_call>` block and return the decoded
/// `arguments` object, asserting the call was not dropped.
fn json_call_args(json_body: &str) -> serde_json::Value {
    let tools = sample_tool_registry();
    let src = format!("<tool_call>{json_body}</tool_call>");
    let result = parse_text_tool_calls_with_tools(&src, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "tool call must parse cleanly: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1, "exactly one call expected");
    result.calls[0]["arguments"].clone()
}

/// Entity-encoded operators in JSON-string edit content decode to real source.
#[test]
fn encoded_operators_in_json_content_decode() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"create","path":"a.ts","content":"xs.map(x =&gt; x)"}}"#,
    );
    assert_eq!(args["content"], json!("xs.map(x => x)"));
}

/// The three operator classes from the field forensics — `&lt;`, `&gt;`, and a
/// doubled `&amp;&amp;` — all decode in one pass.
#[test]
fn all_operator_classes_decode() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"create","path":"a.ts","content":"if (a &lt;= b &amp;&amp; b &gt;= 0) {}"}}"#,
    );
    assert_eq!(args["content"], json!("if (a <= b && b >= 0) {}"));
}

/// A value the model DOUBLE-escaped — `&amp;lt;`, the on-the-wire form of a
/// literal `&lt;` — decodes to `&lt;`, not `<`. The single-pass scan advances
/// past the `&` it produced from `&amp;` without re-reading it, so the intended
/// literal reference survives (e.g. authored HTML/Markdown that really wants
/// `&lt;`). This is the property that makes "decode exactly once" load-bearing.
#[test]
fn double_escaped_reference_survives_as_literal() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"create","path":"page.html","content":"<p>a &amp;lt; b &amp;amp; c</p>"}}"#,
    );
    assert_eq!(args["content"], json!("<p>a &lt; b &amp; c</p>"));
}

/// A bare `&` that opens no recognized reference is emitted verbatim: a shell
/// `a & b`, an `R&D`, and a truncated `&amp` (no closing `;`) are untouched.
#[test]
fn bare_ampersands_untouched() {
    let args = json_call_args(
        r#"{"name":"run","arguments":{"command":"echo R&D && sleep 1 & wait; printf '&amp'"}}"#,
    );
    assert_eq!(
        args["command"],
        json!("echo R&D && sleep 1 & wait; printf '&amp'")
    );
}

/// The quote references models emit (`&quot;`, `&#39;`/`&apos;`) decode too, so
/// a string-literal-heavy edit is not shipped with encoded quotes.
#[test]
fn quote_references_decode() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"create","path":"a.py","content":"print(&quot;a&quot;, &#39;b&#39;, &apos;c&apos;)"}}"#,
    );
    assert_eq!(args["content"], json!("print(\"a\", 'b', 'c')"));
}

/// Decoding recurses into nested arrays/objects: an `edit` with a batch `ops`
/// list decodes the operator inside each op's content.
#[test]
fn nested_arguments_decode_recursively() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"patch","path":"a.ts","ops":[{"content":"x =&gt; x"},{"content":"a &lt; b"}]}}"#,
    );
    assert_eq!(args["ops"][0]["content"], json!("x => x"));
    assert_eq!(args["ops"][1]["content"], json!("a < b"));
}

/// A `<tool_call><edit>{…}</edit>` nested-XML wrapper shares the JSON-string
/// channel, so its content decodes on the same boundary.
#[test]
fn nested_xml_wrapper_content_decodes() {
    let tools = sample_tool_registry();
    let src = r#"<tool_call><edit>{"action":"create","path":"a.ts","content":"x =&gt; x"}</edit></tool_call>"#;
    let result = parse_text_tool_calls_with_tools(src, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["content"], json!("x => x"));
}

/// SCOPE GUARD: heredoc raw-body content is NOT entity-decoded. The heredoc's
/// delimiters are sentinel lines, not angle brackets, so a model writes raw
/// operators there and never HTML-escapes to protect framing. Decoding it would
/// corrupt legitimately-authored references (e.g. an HTML file that really
/// contains `&lt;`). A `&lt;` delivered through a heredoc must survive verbatim.
#[test]
fn heredoc_raw_body_is_not_decoded() {
    let tools = sample_tool_registry();
    let src =
        "edit({ action: \"create\", path: \"page.html\", content: <<EOF\n<p>a &lt; b</p>\nEOF\n })";
    let result = parse_bare_calls_in_body(src, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("<p>a &lt; b</p>"),
        "heredoc content must reach the tool byte-for-byte"
    );
}

/// The whole point: a compile-breaking edit becomes compilable. Before the
/// decode this content reached disk as `xs.filter(x =&gt; x &gt; 0)` and failed
/// to parse; after, it is real TypeScript.
#[test]
fn operator_corruption_class_is_repaired() {
    let args = json_call_args(
        r#"{"name":"edit","arguments":{"action":"replace_body","path":"a.ts","new_body":"const f = (xs) =&gt; xs.filter(x =&gt; x &gt; 0)"}}"#,
    );
    assert_eq!(
        args["new_body"],
        json!("const f = (xs) => xs.filter(x => x > 0)")
    );
}
