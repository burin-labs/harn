use std::collections::HashSet;

use super::{
    DiagnosticMeasurement, DiagnosticTrend, HealthHeuristics, HealthMeasurements, HealthRate,
    ProseToolBalance, SessionHealthFact, TimeExpectation, TimeExpectationSource,
    ToolOutcomeTelemetry, TurnTimeMeasurement, SESSION_HEALTH_SCHEMA_VERSION,
};
use crate::agent_events::{
    AgentEvent, AgentTerminalKind, AgentTurnPhase, ToolCallStatus, ToolMutationStatus,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default)]
struct Counts {
    tools: u64,
    successful_tools: u64,
    commands: u64,
    nonzero_commands: u64,
    prose_characters: u64,
    prose_samples: u64,
    elapsed_ms: u64,
    timed_turns: u64,
}

/// Lives with the owning session; resetting a model iteration preserves history.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionHealth {
    iteration: Option<u64>,
    turn_closed: bool,
    turn: Counts,
    rolling: Counts,
    completed_calls: HashSet<String>,
    completed_commands: HashSet<String>,
    edits_since_verification: Option<u64>,
    diagnostics: Vec<DiagnosticMeasurement>,
    diagnostic_trend: Option<DiagnosticTrend>,
    last_stop: Option<AgentTerminalKind>,
    expected_time: Option<TimeExpectation>,
    turn_started_ms: Option<i64>,
}

impl SessionHealth {
    pub(crate) fn start_turn(&mut self, iteration: u64) {
        self.iteration = Some(iteration);
        self.turn_closed = false;
        self.turn = Counts::default();
        self.completed_calls.clear();
        self.turn_started_ms = None;
        self.expected_time = (self.rolling.timed_turns > 0).then(|| TimeExpectation {
            milliseconds: self.rolling.elapsed_ms as f64 / self.rolling.timed_turns as f64,
            source: TimeExpectationSource::PriorMeasuredTurns,
            sample_count: self.rolling.timed_turns,
        });
    }

    pub(crate) fn observe_tool(
        &mut self,
        call_id: &str,
        succeeded: bool,
        applied_edit: bool,
        telemetry: Option<&ToolOutcomeTelemetry>,
    ) -> bool {
        if !self.completed_calls.insert(call_id.to_owned()) {
            return false;
        }
        let mut duplicate_command = false;
        let command_exit = telemetry.and_then(|value| {
            let code = value.command_exit_code?;
            if let Some(id) = &value.command_id {
                if !self.completed_commands.insert(id.clone()) {
                    duplicate_command = true;
                    return None;
                }
            }
            Some(code)
        });
        for counts in [&mut self.turn, &mut self.rolling] {
            counts.tools += 1;
            counts.successful_tools += u64::from(succeeded);
            if let Some(code) = command_exit {
                counts.commands += 1;
                counts.nonzero_commands += u64::from(code != 0);
            }
        }
        if applied_edit {
            *self.edits_since_verification.get_or_insert(0) += 1;
        }
        if let Some(verification) = telemetry
            .filter(|_| !duplicate_command)
            .and_then(|value| value.verification.as_ref())
        {
            self.edits_since_verification = Some(0);
            if let Some(current) = &verification.diagnostics {
                self.observe_diagnostics(current);
            }
        }
        true
    }

    fn observe_diagnostics(&mut self, current: &DiagnosticMeasurement) {
        if let Some(previous) = self.diagnostics.last() {
            let oscillating = self.diagnostics.len() == 2
                && self.diagnostics[0] == *current
                && previous != current;
            self.diagnostic_trend = Some(if oscillating {
                DiagnosticTrend::Oscillating
            } else if current.count < previous.count {
                DiagnosticTrend::Improving
            } else if current.count > previous.count {
                DiagnosticTrend::Regressing
            } else {
                DiagnosticTrend::Flat
            });
        }
        if self.diagnostics.len() == 2 {
            self.diagnostics.remove(0);
        }
        self.diagnostics.push(current.clone());
    }

    pub(crate) fn finish_turn(&mut self, elapsed_ms: Option<u64>, prose_characters: Option<u64>) {
        // A terminal path can revisit the same iteration. Its measurement must
        // not increase the rolling population a second time.
        if self.turn.timed_turns == 0 {
            if let Some(elapsed) = elapsed_ms {
                self.turn.elapsed_ms = elapsed;
                self.turn.timed_turns = 1;
                self.rolling.elapsed_ms += elapsed;
                self.rolling.timed_turns += 1;
            }
        }
        if self.turn.prose_samples == 0 {
            if let Some(characters) = prose_characters {
                self.turn.prose_characters = characters;
                self.turn.prose_samples = 1;
                self.rolling.prose_characters += characters;
                self.rolling.prose_samples += 1;
            }
        }
    }

    pub(crate) fn stop(&mut self, kind: AgentTerminalKind) {
        self.last_stop = Some(kind);
    }

    pub(crate) fn observe(&mut self, event: &AgentEvent, now_ms: i64) -> Option<SessionHealthFact> {
        match event {
            AgentEvent::IterationStart { iteration, .. } => {
                self.start_turn(*iteration as u64);
                self.turn_started_ms = Some(now_ms);
            }
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                status,
                mutation_status,
                health,
                parsing: None,
                ..
            } if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
                && self.observe_tool(
                    tool_call_id,
                    *status == ToolCallStatus::Completed,
                    *mutation_status == ToolMutationStatus::Applied,
                    health.as_deref(),
                )
                && self.turn_closed =>
            {
                // Observe all real completions, but publish an additional fact
                // only when closeout changes an already-closed turn's population.
                return Some(self.fact(event.session_id()));
            }
            AgentEvent::IterationEnd { iteration_info, .. } => {
                self.turn_closed = true;
                self.finish_turn(
                    self.turn_started_ms
                        .and_then(|start| u64::try_from(now_ms - start).ok()),
                    iteration_info
                        .get("prose_characters")
                        .and_then(serde_json::Value::as_u64),
                );
                return Some(self.fact(event.session_id()));
            }
            AgentEvent::TurnPhaseChanged {
                phase: AgentTurnPhase::Terminal { outcome, .. },
                ..
            } => {
                self.turn_closed = true;
                self.stop(outcome.kind);
                return Some(self.fact(event.session_id()));
            }
            _ => {}
        }
        None
    }

    pub(crate) fn fact(&self, session_id: &str) -> SessionHealthFact {
        SessionHealthFact {
            schema_version: SESSION_HEALTH_SCHEMA_VERSION,
            session_id: session_id.to_owned(),
            iteration: self.iteration,
            turn: self.measurements(&self.turn),
            rolling: self.measurements(&self.rolling),
            heuristics: HealthHeuristics::default(),
        }
    }

    fn measurements(&self, counts: &Counts) -> HealthMeasurements {
        let prose_to_tool_balance = (counts.prose_samples > 0).then(|| ProseToolBalance {
            prose_characters: counts.prose_characters,
            tool_calls: counts.tools,
            characters_per_tool_call: (counts.tools > 0)
                .then(|| counts.prose_characters as f64 / counts.tools as f64),
        });
        let turn_wall_time = (counts.timed_turns > 0).then(|| {
            let actual = counts.elapsed_ms / counts.timed_turns;
            TurnTimeMeasurement {
                milliseconds: actual,
                sample_count: counts.timed_turns,
                expected: self.expected_time.clone(),
                actual_to_expected: self
                    .expected_time
                    .as_ref()
                    .filter(|expected| expected.milliseconds > 0.0)
                    .map(|expected| actual as f64 / expected.milliseconds),
            }
        });
        HealthMeasurements {
            tool_call_success_rate: HealthRate::measured(counts.successful_tools, counts.tools),
            nonzero_command_exit_rate: HealthRate::measured(
                counts.nonzero_commands,
                counts.commands,
            ),
            edits_since_verification: self.edits_since_verification,
            diagnostic_trend: self.diagnostic_trend,
            turn_wall_time,
            prose_to_tool_balance,
            last_stop_class: self.last_stop,
        }
    }
}
