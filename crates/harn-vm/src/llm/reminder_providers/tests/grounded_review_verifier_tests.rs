use super::*;

#[test]
fn grounded_review_gold_set_stays_precision_first() {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Label {
        RealDefect,
        StyleNit,
        FalseAlarm,
    }

    let cases = [
        (
            Label::RealDefect,
            json!({
                "tool_name": "exec_command",
                "tool": {"name": "exec_command", "args": {"cmd": "cargo test -p demo"}},
                "result": {"text": "test result: FAILED. 0 passed; 1 failed\n"},
            }),
        ),
        (
            Label::RealDefect,
            json!({
                "result": {
                    "parse_errors": [
                        {"message": "syntax error: expected expression", "line": 3},
                    ],
                },
            }),
        ),
        (
            Label::StyleNit,
            json!({
                "tool_name": "exec_command",
                "tool": {"name": "exec_command", "args": {"cmd": "cargo check -p demo"}},
                "result": {"text": "warning: unused variable: `scratch`\n"},
            }),
        ),
        (
            Label::FalseAlarm,
            json!({
                "tool_name": "exec_command",
                "tool": {"name": "exec_command", "args": {"cmd": "cargo test -p demo"}},
                "result": {"text": "test result: ok. 12 passed; 0 failed\n"},
            }),
        ),
        (
            Label::FalseAlarm,
            json!({
                "tool_name": "exec_command",
                "tool": {"name": "exec_command", "args": {"cmd": "harn check demo.harn"}},
                "result": {"text": "demo.harn: ok, no parse error\n"},
            }),
        ),
    ];

    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut non_defect_cases = 0;
    for (label, payload) in cases {
        let fired = GroundedReviewProvider
            .evaluate(&ctx(HookEvent::PostToolUse, payload, text_review_options()))
            .is_some();
        match (label, fired) {
            (Label::RealDefect, true) => true_positives += 1,
            (Label::RealDefect, false) => panic!("missed real-defect gold case"),
            (Label::StyleNit | Label::FalseAlarm, true) => false_positives += 1,
            (Label::StyleNit | Label::FalseAlarm, false) => non_defect_cases += 1,
        }
    }

    let precision = true_positives as f64 / (true_positives + false_positives) as f64;
    let regression_rate = false_positives as f64 / (false_positives + non_defect_cases) as f64;
    assert_eq!(precision, 1.0);
    assert_eq!(regression_rate, 0.0);
}

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
        .evaluate(&ctx(HookEvent::PostToolUse, zig, text_review_options()))
        .expect("a genuine verifier parse error must still fire");
    assert!(reminder.body.contains("reported:"));
    assert!(!reminder.body.contains("verified:"));
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
