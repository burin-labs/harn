//! Contract tests for the structural scanner.
//!
//! These pin the *primitive* — does the scan delimit correctly, and does the
//! stream account for every byte it consumed. Dialect behavior (what a unit
//! means, which ladder recovers it) is tested as conformance against the Harn
//! composition, not here.

#![expect(
    clippy::string_slice,
    reason = "unit offsets come from the scanner's char-boundary cursor and inputs are ASCII"
)]

use super::spec::ScanSpec;
use super::{scan_units, Unit, UnitPayload};

/// The vocabulary from `std/llm/dialects`, spelled out so a test failure names
/// a scanner bug rather than a stdlib edit.
fn spec() -> ScanSpec {
    spec_from(serde_json::json!({
        "call_tags": ["tool_call", "toolcall"],
        "block_tags": [
            "assistant_prose", "assistantprose",
            "user_response", "userresponse",
            "done",
        ],
        "wrapper_tags": ["function_calls"],
        "markup_openers": [
            {"opener": "<function=", "close": "</function>"},
            {"opener": "<invoke name=", "close": "</invoke>"},
        ],
        "reserved_openers": ["[[CALL]"],
        "harmony": {
            "header_markers": ["start", "channel", "constrain"],
            "standalone_markers": ["message", "end", "call"],
            "frame_prefix": "<|",
            "frame_suffix": "|>",
            "message_marker": "<|message|>",
            "tool_call_header_prefix": "tool_call to=",
            "corrupted_openers": ["<tool_call<|", "</tool_call<|", "<assistant<|"],
        },
        "known_tools": ["look", "edit", "run", "ledger", "load_skill"],
        "strip_thinking": true,
    }))
}

fn spec_from(value: serde_json::Value) -> ScanSpec {
    ScanSpec::from_json(&value).expect("spec")
}

/// Scan under the standard spec, checking the drop invariant on every input a
/// test feeds through. Every test therefore exercises the tiling property, not
/// just the one test that names it.
fn scan(text: &str) -> (String, Vec<Unit>) {
    let output = scan_units(text, &spec());
    assert_tiles(&output.source, &output.units);
    (output.source, output.units)
}

/// The structural drop invariant: reassembling the units in document order,
/// with the whitespace that separated them, reproduces the scanned source
/// byte for byte. Nothing was consumed without a unit to account for it.
fn assert_tiles(source: &str, units: &[Unit]) {
    let mut rebuilt = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for unit in units {
        assert!(
            unit.start >= cursor,
            "{} unit at {}..{} overlaps the previous unit ending at {cursor}",
            unit.kind(),
            unit.start,
            unit.end
        );
        assert!(
            unit.end > unit.start,
            "{} unit at {}..{} consumed nothing",
            unit.kind(),
            unit.start,
            unit.end
        );
        let gap = &source[cursor..unit.start];
        assert!(
            gap.chars().all(char::is_whitespace),
            "{:?} was skipped before the {} unit at {} without an emitted unit",
            gap,
            unit.kind(),
            unit.start
        );
        rebuilt.push_str(gap);
        rebuilt.push_str(&source[unit.start..unit.end]);
        cursor = unit.end;
    }
    rebuilt.push_str(&source[cursor..]);
    assert!(
        source[cursor..].chars().all(char::is_whitespace),
        "{:?} trails the last unit without an emitted unit",
        &source[cursor..]
    );
    assert_eq!(rebuilt, source, "units do not reconstruct the source");
}

fn kinds(units: &[Unit]) -> Vec<&'static str> {
    units.iter().map(Unit::kind).collect()
}

fn only(units: Vec<Unit>) -> Unit {
    assert_eq!(units.len(), 1, "expected one unit, got {:?}", kinds(&units));
    units.into_iter().next().expect("one unit")
}

// ── One test per unit kind ──────────────────────────────────────────────────

#[test]
fn text_run_is_one_unit_and_keeps_heredoc_bodies_inside() {
    let source = "look({ content: <<EOF\n<tool_call>not a tag</tool_call>\nEOF })\n";
    let (scanned, units) = scan(source);
    match only(units).payload {
        UnitPayload::Text { text } => assert_eq!(text, scanned),
        other => panic!("expected a text unit, got {other:?}"),
    }
}

#[test]
fn fenced_content_is_consumed_a_line_at_a_time() {
    let source = "```\n<tool_call>look({})</tool_call>\n```\n";
    let (_, units) = scan(source);
    let fenced: Vec<&Unit> = units
        .iter()
        .filter(|unit| unit.kind() == "fenced_line")
        .collect();
    assert_eq!(fenced.len(), 1, "kinds: {:?}", kinds(&units));
    match &fenced[0].payload {
        UnitPayload::FencedLine { text } => {
            assert_eq!(text, "<tool_call>look({})</tool_call>");
        }
        other => panic!("expected a fenced_line unit, got {other:?}"),
    }
}

#[test]
fn closed_call_block_carries_body_and_pre_extracted_head() {
    let source = "<tool_call>look({ path: \"a.rs\" })</tool_call>";
    let (_, units) = scan(source);
    match only(units).payload {
        UnitPayload::Block {
            tag,
            body,
            head,
            reserved,
        } => {
            assert_eq!(tag, "tool_call");
            assert_eq!(body, "look({ path: \"a.rs\" })");
            let head = head.expect("call head");
            assert_eq!(head.name, "look");
            assert_eq!(head.sep, '(');
            assert!(!reserved);
        }
        other => panic!("expected a block unit, got {other:?}"),
    }
}

#[test]
fn a_close_tag_inside_a_heredoc_does_not_end_the_block() {
    let source = "<tool_call>edit({ content: <<EOF\n</tool_call>\nEOF })</tool_call>trailing\n";
    let (_, units) = scan(source);
    assert_eq!(kinds(&units), vec!["block", "text"]);
    match &units[0].payload {
        UnitPayload::Block { body, .. } => {
            assert!(body.contains("EOF })"), "body was cut short: {body:?}");
        }
        other => panic!("expected a block unit, got {other:?}"),
    }
}

#[test]
fn unclosed_call_block_runs_to_eof() {
    let source = "<tool_call>edit({ content: \"half";
    let (_, units) = scan(source);
    match only(units).payload {
        UnitPayload::UnclosedBlock {
            tag, body, head, ..
        } => {
            assert_eq!(tag, "tool_call");
            assert_eq!(body, "edit({ content: \"half");
            assert_eq!(head.expect("call head").name, "edit");
        }
        other => panic!("expected an unclosed_block unit, got {other:?}"),
    }
}

#[test]
fn orphan_close_is_its_own_unit() {
    let (_, units) = scan("</tool_call>");
    match only(units).payload {
        UnitPayload::StrayClose { tag } => assert_eq!(tag, "tool_call"),
        other => panic!("expected a stray_close unit, got {other:?}"),
    }
}

#[test]
fn contentless_wrapper_tags_are_units_in_both_directions() {
    let (_, units) = scan("<function_calls>\n</function_calls>");
    assert_eq!(kinds(&units), vec!["wrapper", "wrapper"]);
    for unit in &units {
        match &unit.payload {
            UnitPayload::Wrapper { tag } => assert_eq!(tag, "function_calls"),
            other => panic!("expected a wrapper unit, got {other:?}"),
        }
    }
}

#[test]
fn top_level_function_markup_runs_through_its_close_tag() {
    let source = "<function=edit>\n<parameter=action>\ncreate\n</parameter>\n</function>";
    let (_, units) = scan(source);
    match only(units).payload {
        UnitPayload::Markup { opener, text } => {
            assert_eq!(opener, "<function=");
            assert_eq!(text, source);
        }
        other => panic!("expected a markup unit, got {other:?}"),
    }
}

#[test]
fn markup_without_a_close_tag_runs_to_eof() {
    let source = "<invoke name=\"look\">\n<parameter name=\"path\">a.rs</parameter>";
    let (_, units) = scan(source);
    match only(units).payload {
        UnitPayload::Markup { opener, text } => {
            assert_eq!(opener, "<invoke name=");
            assert_eq!(text, source);
        }
        other => panic!("expected a markup unit, got {other:?}"),
    }
}

#[test]
fn angle_wrapped_call_is_a_unit_only_for_a_known_tool() {
    let (_, units) = scan("<look({ path: \"a.rs\" })>");
    match only(units).payload {
        UnitPayload::AngleCall { name, arguments } => {
            assert_eq!(name, "look");
            assert_eq!(arguments, serde_json::json!({"path": "a.rs"}));
        }
        other => panic!("expected an angle_call unit, got {other:?}"),
    }

    // An unknown name is an unknown tag fragment, running to the first `>`.
    let (_, units) = scan("<frobnicate({ path: \"a.rs\" })>");
    match only(units).payload {
        UnitPayload::Tag { raw, name } => {
            assert_eq!(raw, "<frobnicate({ path: \"a.rs\" })>");
            assert_eq!(name, "frobnicate");
        }
        other => panic!("expected a tag unit, got {other:?}"),
    }
}

#[test]
fn harmony_tool_call_header_is_one_line_unit() {
    let source = "<|message|>tool_call to=functions.look\nlook({})\n";
    let (_, units) = scan(source);
    assert_eq!(kinds(&units), vec!["harmony_line", "text"]);
    match &units[0].payload {
        UnitPayload::HarmonyLine { text } => {
            // The marker is part of the unit: it was consumed here, so it is
            // accounted for here.
            assert_eq!(text, "<|message|>tool_call to=functions.look");
        }
        other => panic!("expected a harmony_line unit, got {other:?}"),
    }
}

#[test]
fn harmony_frame_markers_are_consumed_structurally() {
    let (_, units) = scan("<|start|>assistant<|message|>Hi there\n");
    assert_eq!(kinds(&units), vec!["harmony_skip", "text"]);
    assert_eq!(units[0].payload, UnitPayload::HarmonySkip);
}

#[test]
fn a_corrupted_wrapper_open_is_a_harmony_skip() {
    let (_, units) = scan("<tool_call<|message|>look({})\n");
    assert_eq!(kinds(&units), vec!["harmony_skip", "text"]);
}

#[test]
fn unknown_tag_reports_its_raw_fragment_and_name() {
    let (_, units) = scan("<notes>");
    match only(units).payload {
        UnitPayload::Tag { raw, name } => {
            assert_eq!(raw, "<notes>");
            assert_eq!(name, "notes");
        }
        other => panic!("expected a tag unit, got {other:?}"),
    }
}

#[test]
fn an_unclosed_tag_fragment_stops_at_end_of_line() {
    let (_, units) = scan("<notes attr\nrest\n");
    assert_eq!(kinds(&units), vec!["tag", "text"]);
    match &units[0].payload {
        UnitPayload::Tag { raw, name } => {
            assert_eq!(raw, "<notes attr");
            assert_eq!(name, "notes");
        }
        other => panic!("expected a tag unit, got {other:?}"),
    }
}

#[test]
fn plain_block_tags_use_the_cheap_close_and_carry_no_head() {
    let (_, units) = scan("<assistant_prose>Reading the file.</assistant_prose>");
    match only(units).payload {
        UnitPayload::Block {
            tag, body, head, ..
        } => {
            assert_eq!(tag, "assistant_prose");
            assert_eq!(body, "Reading the file.");
            assert!(head.is_none(), "prose is not a call head");
        }
        other => panic!("expected a block unit, got {other:?}"),
    }
}

// ── Reserved openers ────────────────────────────────────────────────────────

#[test]
fn a_truncated_reserved_opener_collapses_into_a_flagged_block() {
    let source = "[[CALL]look({ path: \"a.rs\" })</tool_call>";
    let (_, units) = scan(source);
    match only(units).payload {
        UnitPayload::Block {
            tag,
            body,
            head,
            reserved,
        } => {
            assert_eq!(tag, "tool_call");
            assert_eq!(body, "look({ path: \"a.rs\" })");
            assert_eq!(head.expect("call head").name, "look");
            assert!(reserved, "the reserved flag is what diagnostics read");
        }
        other => panic!("expected a block unit, got {other:?}"),
    }
}

#[test]
fn a_truncated_reserved_opener_with_no_close_runs_to_eof() {
    let (_, units) = scan("[[CALL]edit({ content: \"half");
    match only(units).payload {
        UnitPayload::UnclosedBlock { reserved, body, .. } => {
            assert!(reserved);
            assert_eq!(body, "edit({ content: \"half");
        }
        other => panic!("expected an unclosed_block unit, got {other:?}"),
    }
}

#[test]
fn the_well_formed_wire_opener_is_left_alone() {
    // `[[CALL]]` is rewritten to the canonical tag upstream; recovering it a
    // second time here would double-handle a call that already parsed.
    let (_, units) = scan("[[CALL]]look({})[[/CALL]]");
    assert_eq!(kinds(&units), vec!["text"]);
}

#[test]
fn a_reserved_opener_longer_than_its_canonical_tag_still_maps_offsets() {
    // Recovery normalizes the stub to the canonical opener and maps offsets
    // back by the length difference. Both lengths are caller data, so the
    // marker may be longer OR shorter than the tag it stands in for.
    let spec = spec_from(serde_json::json!({
        "call_tags": ["tc"],
        "block_tags": [],
        "wrapper_tags": [],
        "markup_openers": [],
        "reserved_openers": ["[[LONG_WIRE_CALL_MARKER]"],
        "harmony": {
            "header_markers": [],
            "standalone_markers": [],
            "frame_prefix": "<|",
            "frame_suffix": "|>",
            "message_marker": "<|message|>",
            "tool_call_header_prefix": "tool_call to=",
            "corrupted_openers": [],
        },
        "known_tools": [],
    }));
    let source = "[[LONG_WIRE_CALL_MARKER]look({})</tc>trailing";
    let output = scan_units(source, &spec);
    assert_tiles(&output.source, &output.units);
    assert_eq!(kinds(&output.units), vec!["block", "text"]);
    let block = &output.units[0];
    assert_eq!(
        &output.source[block.start..block.end],
        "[[LONG_WIRE_CALL_MARKER]look({})</tc>"
    );
}

#[test]
fn a_reserved_opener_inside_a_fence_is_an_example_not_an_action() {
    let (_, units) = scan("```\n[[CALL]look({})\n```\n");
    assert!(
        !units.iter().any(|unit| matches!(
            unit.payload,
            UnitPayload::Block { reserved: true, .. }
                | UnitPayload::UnclosedBlock { reserved: true, .. }
        )),
        "kinds: {:?}",
        kinds(&units)
    );
}

// ── Cross-cutting contracts ─────────────────────────────────────────────────

#[test]
fn offsets_index_the_post_thinking_strip_source() {
    let source = "<think>deliberating</think>\n<tool_call>look({})</tool_call>";
    let (scanned, units) = scan(source);
    assert!(
        !scanned.contains("deliberating"),
        "thinking should be stripped before scanning: {scanned:?}"
    );
    let unit = only(units);
    assert_eq!(
        &scanned[unit.start..unit.end],
        "<tool_call>look({})</tool_call>"
    );
}

#[test]
fn thinking_survives_when_the_spec_turns_stripping_off() {
    let mut raw = serde_json::json!({
        "call_tags": ["tool_call"],
        "block_tags": [],
        "wrapper_tags": [],
        "markup_openers": [],
        "reserved_openers": [],
        "harmony": {
            "header_markers": [],
            "standalone_markers": [],
            "frame_prefix": "<|",
            "frame_suffix": "|>",
            "message_marker": "<|message|>",
            "tool_call_header_prefix": "tool_call to=",
            "corrupted_openers": [],
        },
        "known_tools": [],
        "strip_thinking": false,
    });
    let output = scan_units("<think>deliberating</think>", &spec_from(raw.take()));
    assert!(output.source.contains("deliberating"));
    assert_tiles(&output.source, &output.units);
}

#[test]
fn every_unit_ships_the_bytes_it_covers_pre_sliced() {
    let source = concat!(
        "Intro prose.\n",
        "<tool_call>look({ path: \"a.rs\" })</tool_call>\n",
        "<assistant_prose>Checked it.</assistant_prose>\n",
        "```\n<tool_call>edit({})</tool_call>\n```\n",
        "<notes>\n",
        "<|start|>assistant<|message|>done\n",
    );
    let (scanned, units) = scan(source);
    for unit in &units {
        let span = &scanned[unit.start..unit.end];
        match &unit.payload {
            UnitPayload::Text { text }
            | UnitPayload::FencedLine { text }
            | UnitPayload::Markup { text, .. } => assert_eq!(text, span),
            UnitPayload::Tag { raw, .. } => assert_eq!(raw, span),
            UnitPayload::Block { body, .. } | UnitPayload::UnclosedBlock { body, .. } => {
                assert!(span.contains(body.as_str()), "{body:?} not within {span:?}");
            }
            _ => {}
        }
    }
}

#[test]
fn blocks_chained_on_one_line_stay_top_level() {
    let source = "<tool_call>look({})</tool_call><tool_call>edit({})</tool_call>";
    let (_, units) = scan(source);
    assert_eq!(kinds(&units), vec!["block", "block"]);
}

#[test]
fn unit_count_tracks_structure_not_bytes() {
    let prose = "Some ordinary narration that carries no structural opener. ";
    let block = "<tool_call>look({ path: \"a.rs\" })</tool_call>\n";
    let small = format!("{}{block}", prose.repeat(4));
    let large = format!("{}{block}", prose.repeat(4_000));
    let (small_source, small_units) = scan(&small);
    let (large_source, large_units) = scan(&large);

    assert!(
        large_source.len() > small_source.len() * 100,
        "the large payload should be far bigger: {} vs {}",
        large_source.len(),
        small_source.len()
    );
    assert_eq!(
        kinds(&small_units),
        kinds(&large_units),
        "a thousandfold more prose must not add units"
    );
}

// ── Spec strictness ─────────────────────────────────────────────────────────

#[test]
fn an_absent_vocabulary_field_is_an_error_not_a_fallback() {
    let error = ScanSpec::from_json(&serde_json::json!({
        "block_tags": [],
        "wrapper_tags": [],
        "markup_openers": [],
        "reserved_openers": [],
        "harmony": {},
        "known_tools": [],
    }))
    .expect_err("call_tags is required");
    assert!(error.contains("spec.call_tags"), "{error}");
    assert!(error.contains("required"), "{error}");
}

#[test]
fn an_absent_harmony_field_names_its_parent() {
    let error = ScanSpec::from_json(&serde_json::json!({
        "call_tags": [],
        "block_tags": [],
        "wrapper_tags": [],
        "markup_openers": [],
        "reserved_openers": [],
        "harmony": {"header_markers": []},
        "known_tools": [],
    }))
    .expect_err("standalone_markers is required");
    assert!(error.contains("spec.harmony.standalone_markers"), "{error}");
}

#[test]
fn strip_thinking_is_the_one_field_with_a_default() {
    let built = ScanSpec::from_json(&serde_json::json!({
        "call_tags": [],
        "block_tags": [],
        "wrapper_tags": [],
        "markup_openers": [],
        "reserved_openers": [],
        "harmony": {
            "header_markers": [],
            "standalone_markers": [],
            "frame_prefix": "<|",
            "frame_suffix": "|>",
            "message_marker": "<|message|>",
            "tool_call_header_prefix": "tool_call to=",
            "corrupted_openers": [],
        },
        "known_tools": [],
    }))
    .expect("spec without strip_thinking");
    assert!(built.strip_thinking);
}

// ── Wire shape ──────────────────────────────────────────────────────────────

#[test]
fn the_wire_shape_carries_the_fields_the_composition_reads() {
    let source = "<tool_call>look({ path: \"a.rs\" })</tool_call>";
    let json = scan_units(source, &spec()).to_json();
    let unit = &json["units"][0];
    assert_eq!(unit["kind"], "block");
    assert_eq!(unit["tag"], "tool_call");
    assert_eq!(unit["closed"], true);
    assert_eq!(unit["head_name"], "look");
    assert_eq!(unit["head_sep"], "(");
    assert_eq!(unit["start"], 0);
    assert_eq!(unit["end"], source.len());
    assert!(unit.get("reserved").is_none(), "flag is diagnostics-only");
    assert_eq!(json["source"], source);
}

#[test]
fn a_text_unit_declares_it_is_not_fenced() {
    let json = scan_units("plain narration", &spec()).to_json();
    assert_eq!(json["units"][0]["kind"], "text");
    assert_eq!(json["units"][0]["fenced"], false);
}
