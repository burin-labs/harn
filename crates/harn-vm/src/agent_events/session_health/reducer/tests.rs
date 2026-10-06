use super::*;
use crate::agent_events::session_health::VerificationTelemetry;

#[test]
fn missing_measurements_do_not_become_zero() {
    let mut health = SessionHealth::default();
    health.start_turn(0);
    let fact = health.fact("empty");
    assert_eq!(fact.turn, HealthMeasurements::default());
    assert_eq!(fact.rolling, HealthMeasurements::default());

    health.observe_tool("unknown-command", false, false, None);
    let fact = health.fact("empty");
    assert_eq!(fact.turn.tool_call_success_rate.unwrap().value, 0.0);
    assert!(fact.turn.nonzero_command_exit_rate.is_none());
    assert!(fact.turn.prose_to_tool_balance.is_none());
    assert!(fact.turn.edits_since_verification.is_none());
}

#[test]
fn duplicate_completions_do_not_change_populations_or_edits() {
    let mut health = SessionHealth::default();
    health.start_turn(1);
    let command = ToolOutcomeTelemetry {
        command_id: None,
        command_exit_code: Some(0),
        verification: None,
    };
    health.observe_tool("write", true, true, Some(&command));
    health.observe_tool("write", true, true, Some(&command));
    health.finish_turn(Some(20), Some(0));
    health.finish_turn(Some(20), Some(0));
    let measured = health.fact("s").rolling;
    assert_eq!(measured.tool_call_success_rate.unwrap().denominator, 1);
    let commands = measured.nonzero_command_exit_rate.unwrap();
    assert_eq!((commands.numerator, commands.denominator), (0, 1));
    assert_eq!(measured.edits_since_verification, Some(1));
    assert_eq!(measured.turn_wall_time.unwrap().sample_count, 1);
    assert_eq!(measured.prose_to_tool_balance.unwrap().prose_characters, 0);
}

fn verify(health: &mut SessionHealth, id: &str, fingerprint: &str, count: u64) {
    health.observe_tool(
        id,
        count == 0,
        false,
        Some(&ToolOutcomeTelemetry {
            command_id: None,
            command_exit_code: Some(i32::from(count > 0)),
            verification: Some(VerificationTelemetry {
                diagnostics: Some(DiagnosticMeasurement {
                    set_fingerprint: fingerprint.to_owned(),
                    count,
                }),
            }),
        }),
    );
}

#[test]
fn repeated_background_command_receipts_do_not_repeat_verification() {
    let mut health = SessionHealth::default();
    let receipt = ToolOutcomeTelemetry {
        command_id: Some("background-check".to_owned()),
        command_exit_code: Some(0),
        verification: Some(VerificationTelemetry { diagnostics: None }),
    };
    health.start_turn(1);
    health.observe_tool("first-wait", true, false, Some(&receipt));
    health.start_turn(2);
    health.observe_tool("edit", true, true, None);
    health.observe_tool("second-wait", true, false, Some(&receipt));
    let fact = health.fact("s");
    assert!(fact.turn.nonzero_command_exit_rate.is_none());
    assert_eq!(
        fact.rolling.nonzero_command_exit_rate.unwrap().denominator,
        1
    );
    assert_eq!(fact.turn.edits_since_verification, Some(1));
}

#[test]
fn observations_distinguish_progress_stall_and_oscillation() {
    let mut health = SessionHealth::default();
    health.start_turn(1);
    verify(&mut health, "v1", "a", 3);
    assert_eq!(health.fact("s").turn.diagnostic_trend, None);
    verify(&mut health, "v2", "b", 1);
    assert_eq!(
        health.fact("s").turn.diagnostic_trend,
        Some(DiagnosticTrend::Improving)
    );
    verify(&mut health, "v3", "b", 1);
    assert_eq!(
        health.fact("s").turn.diagnostic_trend,
        Some(DiagnosticTrend::Flat)
    );
    verify(&mut health, "v4", "a", 3);
    verify(&mut health, "v5", "b", 1);
    assert_eq!(
        health.fact("s").turn.diagnostic_trend,
        Some(DiagnosticTrend::Oscillating)
    );
    health.observe_tool("edit-after-verify", true, true, None);
    assert_eq!(health.fact("s").turn.edits_since_verification, Some(1));
    health.finish_turn(Some(40), Some(8));

    health.start_turn(2);
    health.finish_turn(Some(80), Some(12));
    health.stop(AgentTerminalKind::Natural);
    let fact = health.fact("s");
    assert!(fact.turn.tool_call_success_rate.is_none());
    assert_eq!(fact.rolling.tool_call_success_rate.unwrap().denominator, 6);
    let time = fact.turn.turn_wall_time.unwrap();
    assert_eq!(time.actual_to_expected, Some(2.0));
    assert_eq!(time.expected.unwrap().sample_count, 1);
    assert_eq!(fact.rolling.turn_wall_time.unwrap().milliseconds, 60);
    assert_eq!(fact.turn.last_stop_class, Some(AgentTerminalKind::Natural));
}
