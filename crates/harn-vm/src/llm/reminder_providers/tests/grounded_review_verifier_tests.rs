use super::*;

// Verifier-PRESERVING: a GENUINE compiler/`zig test` parse error on a
// FAILING line must STILL produce the grounded signal — no regression.
#[test]
fn grounded_review_still_fires_on_genuine_verifier_parse_error() {
    // Real `zig build test` output: a compiler `error:` on a failing line.
    let zig = json!({
        "tool_name": "exec_command",
        "tool": {"name": "exec_command", "args": {"cmd": "zig build test"}},
        "result": {
            "text": "src/parser.zig:42:9: error: expected ';', found '}'\n\
                     28/59 parser.test.parse error: unclosed section...FAIL\n"
        },
    });
    let reminder = GroundedReviewProvider
        .evaluate(&ctx(HookEvent::PostToolUse, zig, JsonValue::Null))
        .expect("a genuine verifier parse error must still fire");
    assert!(reminder.body.contains("verified:"));
    assert!(reminder.body.contains("error: expected ';'"));

    // The pass-marker exemption is line-scoped: a real `parse error:` line
    // with NO pass marker still surfaces.
    assert!(review_failure_line("error: parse error: unexpected token at line 3").is_some());
    // And a structured verifier `parse_errors` array is untouched.
    let structured = json!({
        "result": {
            "parse_errors": [{"message": "syntax error: expected expression", "line": 3}],
        },
    });
    assert!(
        GroundedReviewProvider
            .evaluate(&ctx(HookEvent::PostToolUse, structured, JsonValue::Null))
            .is_some(),
        "a structured verifier parse_errors array must still fire"
    );
}
