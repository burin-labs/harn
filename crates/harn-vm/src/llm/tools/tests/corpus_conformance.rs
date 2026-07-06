//! Corpus-driven parser conformance tests.
//!
//! Every fixture here is a REAL cheap-model emission shape observed in the
//! Burin eval corpus (526 runs mined 2026-07-02), reduced to its structural
//! skeleton. Each test pins the tolerant outcome for one observed
//! failure class — or pins the deliberate strictness where tolerance would
//! erode the tagged protocol. When a new emission shape kills turns in the
//! wild, add its skeleton here first, then make it pass.

use super::{
    json, normalize_tool_args, parse_bare_calls_in_body, parse_text_tool_calls_with_tools, vm_bool,
    vm_dict, vm_list, vm_str, VmValue,
};
use std::collections::BTreeMap;

/// Tool registry mirroring the coding-agent surface the corpus runs used:
/// `look`, `edit` (with a bool `overwrite` and a list `ops`), `run`, `verify`.
fn corpus_tool_registry() -> VmValue {
    let mut look_params = BTreeMap::new();
    look_params.insert("file".to_string(), vm_dict(&[("type", vm_str("string"))]));
    look_params.insert(
        "intent".to_string(),
        vm_dict(&[("type", vm_str("string")), ("required", vm_bool(false))]),
    );
    let look_tool = vm_dict(&[
        ("name", vm_str("look")),
        ("description", vm_str("Read a file.")),
        ("parameters", VmValue::dict(look_params)),
    ]);

    let mut edit_params = BTreeMap::new();
    for key in [
        "action",
        "path",
        "old_string",
        "new_string",
        "content",
        "anchor",
    ] {
        edit_params.insert(
            key.to_string(),
            vm_dict(&[("type", vm_str("string")), ("required", vm_bool(false))]),
        );
    }
    for key in ["range_start", "range_end"] {
        edit_params.insert(
            key.to_string(),
            vm_dict(&[("type", vm_str("int")), ("required", vm_bool(false))]),
        );
    }
    edit_params.insert(
        "overwrite".to_string(),
        vm_dict(&[("type", vm_str("bool")), ("required", vm_bool(false))]),
    );
    edit_params.insert(
        "ops".to_string(),
        vm_dict(&[("type", vm_str("list")), ("required", vm_bool(false))]),
    );
    let edit_tool = vm_dict(&[
        ("name", vm_str("edit")),
        ("description", vm_str("Precise code edit.")),
        ("parameters", VmValue::dict(edit_params)),
    ]);

    let mut run_params = BTreeMap::new();
    run_params.insert(
        "command".to_string(),
        vm_dict(&[("type", vm_str("string"))]),
    );
    let run_tool = vm_dict(&[
        ("name", vm_str("run")),
        ("description", vm_str("Run a shell command.")),
        ("parameters", VmValue::dict(run_params)),
    ]);

    let verify_tool = vm_dict(&[
        ("name", vm_str("verify")),
        ("description", vm_str("Run the project verify command.")),
        (
            "parameters",
            VmValue::dict(BTreeMap::<String, VmValue>::new()),
        ),
    ]);

    vm_dict(&[(
        "tools",
        vm_list(vec![look_tool, edit_tool, run_tool, verify_tool]),
    )])
}

fn call_names(calls: &[serde_json::Value]) -> Vec<&str> {
    calls
        .iter()
        .filter_map(|call| call.get("name").and_then(|name| name.as_str()))
        .collect()
}

// ── Class 1: `<tool_call>` wrapper shapes ────────────────────────────────────

/// The canonical wrapper with JSON-quoted keys and a heredoc body — the single
/// most common corpus emission — parses cleanly. Pins that the wrapper tags
/// themselves were never the problem.
#[test]
fn corpus_canonical_wrapper_json_keys_heredoc_parses() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\nedit({\n    \"action\": \"exact_patch\",\n    \"path\": \"src/schema.zig\",\n    \"old_string\": <<EOF\nconst schema_json =\n    \\\\{\n    \\\\  \"sections\": {\nEOF\n,\n    \"new_string\": <<EOF2\nconst schema_json = \"\";\nEOF2\n})\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert_eq!(call_names(&result.calls), vec!["edit"]);
    assert_eq!(result.calls[0]["arguments"]["action"], json!("exact_patch"));
}

/// Back-to-back blocks with NO whitespace between `</tool_call>` and the next
/// `<tool_call>` on the same line. Observed at scale (swift-feat corpus): the
/// second open tag used to fail the line-start check and was shredded into a
/// `Stray text outside response tags: "<tool_call>"` violation.
#[test]
fn corpus_back_to_back_wrapper_blocks_same_line() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\nedit({ \"action\": \"create\", \"path\": \"a.zig\", \"content\": <<EOF\ncode\nEOF\n})\n</tool_call><tool_call>\nverify({})\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert_eq!(call_names(&result.calls), vec!["edit", "verify"]);
}

/// Same adjacency with a single space between the blocks.
#[test]
fn corpus_adjacent_wrapper_blocks_with_space() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\nverify({})\n</tool_call> <tool_call>\nrun({ command: \"zig build test\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert_eq!(call_names(&result.calls), vec!["verify", "run"]);
}

/// A `<done>` block chained directly after a call block on one line.
#[test]
fn corpus_done_block_adjacent_to_wrapper_block() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\nverify({})\n</tool_call><done>##DONE##</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert_eq!(result.done_marker.as_deref(), Some("##DONE##"));
}

// ── Class 2: stray prose alongside a well-formed call ───────────────────────

/// Untagged prose before a well-formed `<tool_call>` block is harmless
/// envelope drift. The call dispatches and the prose is canonicalized into an
/// assistant-prose block so the agent does not spend another turn on
/// parse-guidance after useful work already landed.
#[test]
fn corpus_prose_before_wrapper_block_dispatches_call_without_violation() {
    let tools = corpus_tool_registry();
    let text = "We need to create BatteryProvider.swift and wire it into LiveSystemProvider.\n\n<tool_call>\nlook({ file: \"Sources/SysMonCore/Providers.swift\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(call_names(&result.calls), vec!["look"]);
    assert!(
        result.violations.is_empty(),
        "stray-prose should not trigger parse guidance after dispatch: {:?}",
        result.violations
    );
    assert!(result.canonical.contains("<assistant_prose>"));
}

/// A bare call embedded in stray prose at line start is recovered and
/// executed (existing report_stray behavior, pinned here from the corpus
/// shape: prose paragraphs around a fenceless `look({...})`).
#[test]
fn corpus_bare_call_inside_stray_prose_is_recovered() {
    let tools = corpus_tool_registry();
    let text = "Let me inspect the parser first.\nlook({ file: \"src/parse.zig\" })\nThen I will patch it.";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(call_names(&result.calls), vec!["look"]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

// ── Class 3: chat-template `<invoke>` markup with extra attributes ──────────

/// `<parameter name="file" string="true">` — the DSML-influenced attribute
/// spelling. The old parameter regex required `>` straight after the name, so
/// this COMPLETE call was misdiagnosed as "TOOL CALL TRUNCATED" and the turn
/// died — the single largest full-turn parse-kill class in the corpus (47+
/// kills for `look` alone).
#[test]
fn corpus_invoke_markup_with_string_attribute_inside_wrapper() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\n<invoke name=\"look\">\n<parameter name=\"file\" string=\"true\">src/root.zig</parameter>\n</invoke>\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(call_names(&result.calls), vec!["look"]);
    assert_eq!(result.calls[0]["arguments"]["file"], json!("src/root.zig"));
}

/// The same markup at top level inside a `<function_calls>` wrapper (the
/// Anthropic-template vocabulary). The wrapper tags are swallowed silently;
/// the call dispatches with only the soft recovered-markup violation.
#[test]
fn corpus_invoke_markup_in_function_calls_wrapper_top_level() {
    let tools = corpus_tool_registry();
    let text = "<function_calls>\n<invoke name=\"look\">\n<parameter name=\"file\" string=\"true\">src/main.zig</parameter>\n<parameter name=\"intent\" string=\"true\">read</parameter>\n</invoke>\n</function_calls>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(call_names(&result.calls), vec!["look"]);
    assert_eq!(result.calls[0]["arguments"]["file"], json!("src/main.zig"));
    assert_eq!(result.calls[0]["arguments"]["intent"], json!("read"));
    assert!(
        result
            .violations
            .iter()
            .all(|violation| !violation.contains("Unknown top-level tag")),
        "wrapper tags must not raise unknown-tag violations: {:?}",
        result.violations
    );
}

/// A genuinely truncated `<parameter>` block (no `</parameter>` anywhere)
/// still surfaces the TRUNCATED diagnostic — tolerance must not dispatch a
/// partial argument value.
#[test]
fn corpus_truly_unterminated_parameter_still_errors() {
    let tools = corpus_tool_registry();
    let text =
        "<tool_call>\n<invoke name=\"look\">\n<parameter name=\"file\" string=\"true\">src/root.zi";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.calls.is_empty(), "calls: {:?}", result.calls);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("TRUNCATED")),
        "expected a truncation diagnostic: {:?}",
        result.errors
    );
}

// ── Class 4: compat aliases emitted through the TEXT channel ────────────────

/// `replace_range({...})` called as a top-level tool inside `<tool_call>` —
/// the edit-action-verb dialect compat already folds for the native channel.
/// 21 corpus turn-kills. Folds to `edit` with the `action` injected.
#[test]
fn corpus_edit_action_verb_as_text_tool_name_folds_to_edit() {
    let tools = corpus_tool_registry();
    let text = "<tool_call>\nreplace_range({\n    \"path\": \"src/main.zig\",\n    \"range_start\": 71,\n    \"range_end\": 84,\n    \"content\": <<EOF\n    } else {\n        try stderr.print(\"bad\", .{});\n    }\nEOF\n})\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(call_names(&result.calls), vec!["edit"]);
    let args = &result.calls[0]["arguments"];
    assert_eq!(args["action"], json!("replace_range"));
    assert_eq!(args["path"], json!("src/main.zig"));
    assert_eq!(args["range_start"], json!(71));
}

/// Shell synonyms fold the same way in the bare text channel.
#[test]
fn corpus_shell_alias_bare_call_folds_to_run() {
    let tools = corpus_tool_registry();
    let result = parse_bare_calls_in_body("bash({ script: \"zig build test\" })", Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(call_names(&result.calls), vec!["run"]);
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("zig build test")
    );
}

/// A genuinely unknown tool name keeps its actionable diagnostic — the alias
/// fold only applies when the resolved name is a registered tool.
#[test]
fn corpus_unknown_tool_name_still_errors() {
    let tools = corpus_tool_registry();
    let result = parse_bare_calls_in_body("frobnicate({ path: \"a.zig\" })", Some(&tools));
    assert!(result.calls.is_empty(), "calls: {:?}", result.calls);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("frobnicate")),
        "expected unknown-tool diagnostic: {:?}",
        result.errors
    );
}

// ── Class 5: unclosed terminal response tags ────────────────────────────────

/// The terminal-answer shape: the model writes its final `<user_response>`
/// prose, ends the turn, and omits the close tag. 27 corpus turn-kills.
/// Accepted as the block body — the answer is complete.
#[test]
fn corpus_unclosed_terminal_user_response_is_accepted() {
    let tools = corpus_tool_registry();
    let text = "<user_response>Implemented the INI schema validation system with a `Schema` definition and a `cmdCheck` command.";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert!(
        result
            .user_response
            .as_deref()
            .is_some_and(|response| response.contains("INI schema validation")),
        "user_response should carry the body: {:?}",
        result.user_response
    );
}

/// Same shape for `<assistant_prose>`.
#[test]
fn corpus_unclosed_terminal_assistant_prose_is_accepted() {
    let tools = corpus_tool_registry();
    let text = "<assistant_prose>\nImplemented full INI schema validation:\n- Added `src/schema.zig`.\n- Imported the new module in `src/main.zig`.";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert!(
        result.prose.contains("INI schema validation"),
        "prose should carry the body: {:?}",
        result.prose
    );
}

/// STRICTNESS PINNED: an unclosed response tag followed by another top-level
/// block is NOT absorbed to EOF — the trailing block must still parse and the
/// unclosed tag keeps its violation.
#[test]
fn corpus_unclosed_response_tag_before_call_stays_strict() {
    let tools = corpus_tool_registry();
    let text = "<user_response>Done, verifying now.\n<tool_call>\nverify({})\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(call_names(&result.calls), vec!["verify"]);
    assert!(
        result
            .violations
            .iter()
            .any(|violation| violation.contains("Unclosed <user_response>")),
        "unclosed-tag violation should be retained: {:?}",
        result.violations
    );
}

// ── Argument-shape coercion at the dispatch chokepoint ──────────────────────

/// `overwrite: "True"` (Python spelling) where the schema says bool — 6
/// corpus runtime kills (`TypeError: parameter 'overwrite' expected bool,
/// got string (True)`).
#[test]
fn corpus_bool_string_coerces_when_schema_expects_bool() {
    let tools = corpus_tool_registry();
    for (raw, expected) in [
        ("True", true),
        ("true", true),
        ("TRUE", true),
        ("False", false),
        ("false", false),
    ] {
        let normalized = normalize_tool_args(
            "edit",
            json!({"action": "create", "path": "a.zig", "overwrite": raw}),
            Some(&tools),
        );
        assert_eq!(
            normalized["overwrite"],
            json!(expected),
            "spelling {raw:?} should coerce"
        );
    }
}

/// Non-boolean spellings and string-typed params are never coerced.
#[test]
fn corpus_bool_coercion_is_conservative() {
    let tools = corpus_tool_registry();
    let normalized = normalize_tool_args(
        "edit",
        json!({"overwrite": "yes", "action": "true"}),
        Some(&tools),
    );
    // "yes" is not an unambiguous bool spelling.
    assert_eq!(normalized["overwrite"], json!("yes"));
    // `action` is string-typed; the literal string "true" must survive.
    assert_eq!(normalized["action"], json!("true"));
}

/// A JSON-encoded STRING where the schema says list — 2 corpus runtime kills
/// (`TypeError: parameter 'ops' expected list, got string ([{...})`).
#[test]
fn corpus_json_array_string_coerces_when_schema_expects_list() {
    let tools = corpus_tool_registry();
    let normalized = normalize_tool_args(
        "edit",
        json!({"ops": "[{\"op\": \"replace_range\", \"range_start\": 3}]"}),
        Some(&tools),
    );
    assert_eq!(
        normalized["ops"],
        json!([{"op": "replace_range", "range_start": 3}])
    );
}

/// A string that does NOT parse as a JSON array rides through unchanged so
/// the precise type error still surfaces downstream (the corpus sample was
/// itself malformed JSON — never guess at repair).
#[test]
fn corpus_malformed_array_string_is_not_coerced() {
    let tools = corpus_tool_registry();
    let malformed = "[{\"call_id\": \"1\",tool\": \"look\"";
    let normalized = normalize_tool_args("edit", json!({"ops": malformed}), Some(&tools));
    assert_eq!(normalized["ops"], json!(malformed));
}
