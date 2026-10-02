//! Observational session health. Missing measurements remain explicit nulls.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod reducer;
pub(crate) use reducer::SessionHealth;

pub const SESSION_HEALTH_SCHEMA_VERSION: u32 = 1;
pub const SESSION_HEALTH_SCHEMA_ARTIFACT: &str = "schemas/session-health.schema.json";

/// A rate retains its population so a measured zero cannot hide missing data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthRate {
    pub numerator: u64,
    pub denominator: u64,
    pub value: f64,
}

impl HealthRate {
    pub(crate) fn measured(numerator: u64, denominator: u64) -> Option<Self> {
        (denominator > 0).then(|| Self {
            numerator,
            denominator,
            value: numerator as f64 / denominator as f64,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticTrend {
    Improving,
    Flat,
    Regressing,
    Oscillating,
}

/// Producer-normalized diagnostic identity and cardinality, without messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticMeasurement {
    pub set_fingerprint: String,
    pub count: u64,
}

/// A completed declared verification run may have no diagnostic measurement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerificationTelemetry {
    pub diagnostics: Option<DiagnosticMeasurement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolOutcomeTelemetry {
    pub command_id: Option<String>,
    pub command_exit_code: Option<i32>,
    pub verification: Option<VerificationTelemetry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeExpectationSource {
    PriorMeasuredTurns,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeExpectation {
    pub milliseconds: f64,
    pub source: TimeExpectationSource,
    pub sample_count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TurnTimeMeasurement {
    /// Mean elapsed time of the measured turns in this population.
    pub milliseconds: u64,
    pub sample_count: u64,
    pub expected: Option<TimeExpectation>,
    /// Absent when there is no positive expectation from prior measured turns.
    pub actual_to_expected: Option<f64>,
}

/// Counts come from emission telemetry, never from interpretation of prose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProseToolBalance {
    pub prose_characters: u64,
    pub tool_calls: u64,
    /// Absent when no tool call supplies a denominator.
    pub characters_per_tool_call: Option<f64>,
}

/// Reserved for explicitly identified estimates, separate from measured facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthHeuristics {}

pub fn session_health_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(SessionHealthFact))
        .expect("session health schema is JSON");
    schema["$id"] = serde_json::json!("https://harnlang.com/schemas/session-health.v1.json");
    schema["x-harn-provenance"] = serde_json::json!({
        "owner": "harn-vm::agent_events::session_health",
        "schema_version": SESSION_HEALTH_SCHEMA_VERSION,
    });
    schema
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionHealthFact {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    pub session_id: String,
    pub iteration: Option<u64>,
    pub turn: HealthMeasurements,
    pub rolling: HealthMeasurements,
    pub heuristics: HealthHeuristics,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthMeasurements {
    pub tool_call_success_rate: Option<HealthRate>,
    pub nonzero_command_exit_rate: Option<HealthRate>,
    pub edits_since_verification: Option<u64>,
    pub diagnostic_trend: Option<DiagnosticTrend>,
    pub turn_wall_time: Option<TurnTimeMeasurement>,
    pub prose_to_tool_balance: Option<ProseToolBalance>,
    pub last_stop_class: Option<super::AgentTerminalKind>,
}
