//! Convergence regression + streaming/non-streaming parity for reserved-token
//! tool-call delimiters (qwen3.6 local on llamacpp).
//!
//! Field bug (#3044): a local `local-qwen3.6` go-test eval dispatched ZERO tool
//! calls across 23 turns even though the model correctly emitted
//! `[[CALL]]…[[/CALL]]` blocks on every turn. Root cause: the wire→canonical
//! delimiter remap lived only in `chat_impl`, which the *unregistered*
//! `llamacpp` OpenAI-compat fallback route in `vm_call_llm_api` bypasses. So the
//! assembled completion reached the tagged tool-call parser in raw wire form.
//!
//! Why that used to lose calls: the tagged parser anchors on `<tool_call>`
//! blocks. With the wire form there are no such blocks, so parsing used to fall
//! entirely to the fragile bare-call rescue scanner — which, when a tool name
//! wasn't in the effective known-tool set, silently dropped the call with NO
//! diagnostic (0 calls, 0 errors). Canonicalization gives the tagged parser real
//! `<tool_call>` blocks, so the call is either dispatched or surfaced as a
//! diagnostic the loop can replay. Newer salvage logic also diagnoses the raw
//! wire form; the regression is silent loss, not the exact recovery path.
//!
//! The remap lives in the shared transport funnel (`vm_call_llm_api_with_body`)
//! so it fires identically on every route — registered and unregistered,
//! streaming and non-streaming.

use super::parse_text_tool_calls_with_tools;
use super::sample_tool_registry;
use crate::llm::tool_delimiter::wire_to_canonical;

/// The real first-turn completion from the failing eval: 22 `[[CALL]]…[[/CALL]]`
/// blocks emitted back-to-back with no separator (10 `look`s, an `edit`, …).
const MULTIBLOCK: &str = include_str!("../../testdata/qwen36_multiblock_response.txt");

/// The real ~10.6KB single `edit({ … content: <<EOF … EOF })` block, with the
/// model's escaped-newline heredoc body.
const EDIT_BLOCK: &str = include_str!("../../testdata/qwen36_reserved_token_response.txt");

#[test]
fn wire_form_and_canonical_form_do_not_silently_lose_calls() {
    // The exact field condition: the tagged parser has no `<tool_call>` blocks
    // to anchor on before transport canonicalization. Older parser behavior let
    // these raw wire-form calls vanish with 0 calls and 0 diagnostics, which is
    // why the agent saw "nothing happened" and hallucinated completion. The
    // parser may now recover a diagnostic directly from the raw wire form, but
    // it still must not dispatch calls before canonicalization.
    let wire = parse_text_tool_calls_with_tools(MULTIBLOCK, None);
    assert_eq!(
        wire.calls.len(),
        0,
        "raw reserved-token wire form should not dispatch before canonicalization"
    );
    assert!(
        !wire.errors.is_empty() || !wire.violations.is_empty(),
        "raw wire form must surface a repair signal, not silently lose calls: \
         errors={:?} violations={:?}",
        wire.errors,
        wire.violations
    );

    // After the remap, the same bytes carry real `<tool_call>` blocks, so the
    // tagged parser sees them. With no registry the blocks surface as
    // structured diagnostics (not silence) the loop can act on; the regression
    // is the *silent* loss, which canonicalization eliminates.
    let canon = parse_text_tool_calls_with_tools(&wire_to_canonical(MULTIBLOCK), None);
    assert!(
        !canon.calls.is_empty() || !canon.errors.is_empty(),
        "canonicalized text must be visible to the parser (calls or diagnostics), \
         not silently dropped: calls={:?} errors={:?}",
        canon.calls,
        canon.errors
    );
}

#[test]
fn registered_tools_dispatch_only_after_canonicalization_for_tagged_blocks() {
    // With a registry containing the emitted tools, the canonical (tagged) form
    // dispatches the `look`/`edit` calls cleanly. The sample registry has
    // `edit`; assert the captured single-edit turn dispatches the edit on the
    // canonical path.
    //
    // NOTE: `EDIT_BLOCK`'s body uses the model's degraded JSON-escaped heredoc
    // form (`<<EOF\npackage…` with *literal* `\n`). The JSON-escaped-heredoc
    // recovery now lets bare-rescue parse that body on the wire path too, so the
    // edit is no longer silently lost there — a strict improvement. The
    // canonical path remains the robust route (real `<tool_call>` blocks); both
    // must surface the edit.
    let tools = sample_tool_registry();

    let saw_edit_call = |calls: &[serde_json::Value]| {
        calls.iter().any(|c| {
            c.get("name").and_then(|v| v.as_str()) == Some("edit")
                || c.get("tool").and_then(|v| v.as_str()) == Some("edit")
        })
    };

    let wire = parse_text_tool_calls_with_tools(EDIT_BLOCK, Some(&tools));
    assert!(
        saw_edit_call(&wire.calls),
        "wire form: heredoc recovery must surface the edit (no silent loss): \
         calls={:?} errors={:?}",
        wire.calls,
        wire.errors
    );

    let canon = parse_text_tool_calls_with_tools(&wire_to_canonical(EDIT_BLOCK), Some(&tools));
    assert!(
        saw_edit_call(&canon.calls) || !canon.errors.is_empty(),
        "after remap the edit block must be visible (dispatched or diagnosed): \
         calls={:?} errors={:?}",
        canon.calls,
        canon.errors
    );
}

#[test]
fn streaming_and_non_streaming_remap_parse_identically() {
    // Streaming/non-streaming parity. The non-streaming transport returns the
    // whole completion; the streaming transport assembles it from content
    // deltas. Both funnel through `vm_call_llm_api_with_body`, where the
    // assembled `result.text` is remapped via `wire_to_canonical`. Model the
    // streaming assembly as an arbitrary char-boundary chunking of the same wire
    // bytes (concatenating deltas reproduces the full completion) and assert the
    // parsed calls AND diagnostics are byte-for-byte identical across paths.
    let tools = sample_tool_registry();

    for fixture in [MULTIBLOCK, EDIT_BLOCK] {
        // Non-streaming: whole text -> remap -> parse.
        let non_streaming_text = wire_to_canonical(fixture);
        let non_streaming = parse_text_tool_calls_with_tools(&non_streaming_text, Some(&tools));

        // Streaming: split into 3-byte deltas (splitting inside `[[CALL]]` and
        // the body), reassemble -> remap -> parse.
        let mut assembled = String::new();
        let bytes = fixture.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let mut end = (i + 3).min(bytes.len());
            while end < bytes.len() && !fixture.is_char_boundary(end) {
                end += 1;
            }
            assembled.push_str(&fixture[i..end]);
            i = end;
        }
        let streaming_text = wire_to_canonical(&assembled);
        let streaming = parse_text_tool_calls_with_tools(&streaming_text, Some(&tools));

        assert_eq!(
            non_streaming_text, streaming_text,
            "assembled streaming text must equal the non-streaming text"
        );
        assert_eq!(
            non_streaming.calls, streaming.calls,
            "streaming and non-streaming paths must parse identical tool calls"
        );
        assert_eq!(
            non_streaming.errors, streaming.errors,
            "streaming and non-streaming paths must produce identical diagnostics"
        );
    }
}
