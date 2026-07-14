//! Recovery of tool calls whose object-literal argument is complete but whose
//! own closing `)` the model omitted, e.g. `edit({ ..., content: <<EOF ... EOF }`
//! (a bare `}` where `})` was meant). Kept in a focused file so the already
//! grandfathered-oversized `heredoc_and_messages.rs` does not keep growing.

use super::{
    json, parse_bare_calls_in_body, parse_text_tool_calls_with_tools, sample_tool_registry,
};

#[test]
fn implicit_call_paren_close_recovers_plain_object_arg() {
    // Cheap models close the object literal with `}` but forget the tool call's
    // own `)`. A structurally-complete object whose sole omission is the trailing
    // paren is recovered rather than dropped.
    let tools = sample_tool_registry();
    let text = "run({ command: \"go build ./...\" }"; // note: no closing `)`
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "omitted `)` should be recovered, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls[0]["name"], json!("run"));
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("go build ./...")
    );
}

#[test]
fn implicit_call_paren_close_recovers_heredoc_edit() {
    // The real-world shape: a heredoc `content` value, the object closed with a
    // bare `}` on its own line, and no `)` before the block ends.
    let tools = sample_tool_registry();
    let text = "edit({\n  \"action\": \"create\",\n  \"path\": \"a.scala\",\n  \"content\": <<EOF\npackage x\nfinal case class Y()\nEOF\n}";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "heredoc edit with omitted `)` should be recovered, errors: {:?}",
        result.errors
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("package x"),
        "content should carry the heredoc body: {content}"
    );
    assert!(content.contains("final case class Y()"));
}

#[test]
fn multiblock_turn_lands_every_call_when_paren_omitted() {
    // Regression fixture taken verbatim from a real dropped multi-block payload:
    // three `edit` heredoc blocks followed by a `run`, each edit block closing
    // its object with a bare `}` and omitting the call `)`. Before the
    // implicit-paren-close recovery only `run` parsed and the three authored
    // edits were dropped as "missing closing `)`".
    let text = include_str!("tool_call_multiblock_real_payload.txt");
    let tools = sample_tool_registry(); // registers `edit` + `run`
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    let names: Vec<&str> = result
        .calls
        .iter()
        .map(|c| c["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        names,
        vec!["edit", "edit", "edit", "run"],
        "all four authored calls should land, errors: {:?}",
        result.errors
    );
}

#[test]
fn omitted_paren_recovery_still_rejects_trailing_garbage() {
    // Guard against over-recovery: the implicit-close only fires at genuine
    // end-of-input. A complete object followed by stray non-`)` bytes is still a
    // hard parse error — the recovery must not swallow trailing content.
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.rs\" } and then some stray words";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.calls.is_empty(),
        "trailing garbage after `}}` must not be recovered as a call, calls: {:?}",
        result.calls
    );
}
