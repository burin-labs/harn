//! Trusted deterministic native adapter for hypothesis workflow liveness.

use std::sync::Arc;

use crate::value::{DictMap, VmError, VmValue};
use crate::{host_call_ready, HostCallBridge, HostCallDispatchFuture};

const CAPABILITY: &str = "hypothesis";
const OPERATION: &str = "operation";
const ATTEST_EVENT: &str = "attest_event";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HypothesisScenario {
    Aa,
    KnownBad,
    Denied,
    BudgetExhausted,
    MissingTelemetry,
    FailDecisionAttestation,
}

impl HypothesisScenario {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "aa" => Ok(Self::Aa),
            "known-bad" => Ok(Self::KnownBad),
            "denied" => Ok(Self::Denied),
            "budget-exhausted" => Ok(Self::BudgetExhausted),
            "missing-telemetry" => Ok(Self::MissingTelemetry),
            "fail-decision-attestation" => Ok(Self::FailDecisionAttestation),
            _ => Err(format!(
                "hypothesis scenario must be aa, known-bad, denied, budget-exhausted, missing-telemetry, or fail-decision-attestation; got '{value}'"
            )),
        }
    }
}

pub fn bridge(scenario: HypothesisScenario) -> Arc<dyn HostCallBridge> {
    Arc::new(HypothesisScenarioBridge { scenario })
}

struct HypothesisScenarioBridge {
    scenario: HypothesisScenario,
}

impl HostCallBridge for HypothesisScenarioBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a DictMap,
    ) -> HostCallDispatchFuture<'a> {
        if capability != CAPABILITY {
            return host_call_ready(Ok(None));
        }
        match operation {
            ATTEST_EVENT => host_call_ready(self.attest(params).map(Some)),
            OPERATION => host_call_ready(self.operate(params).map(Some)),
            _ => host_call_ready(Ok(None)),
        }
    }
}

impl HypothesisScenarioBridge {
    fn attest(&self, params: &DictMap) -> Result<VmValue, VmError> {
        if self.scenario == HypothesisScenario::FailDecisionAttestation
            && event_payload_kind(params) == Some("decision_recorded")
        {
            return Err(VmError::Runtime(
                "injected decision attestation boundary failure".to_string(),
            ));
        }
        Ok(dict([(
            "_meta",
            dict([(
                "harn",
                dict([(
                    "hostResult",
                    dict([
                        ("schema", string("harn.host-result.v1")),
                        ("kind", string("hypothesis_native_attestation")),
                    ]),
                )]),
            )]),
        )]))
    }

    fn operate(&self, params: &DictMap) -> Result<VmValue, VmError> {
        let action = required(params, "action")?.clone();
        let receipt = required(params, "operation_receipt_id")?.clone();
        if self.scenario == HypothesisScenario::Denied {
            return Ok(dict([
                ("schema", string("harn.hypothesis.operation_result.v1")),
                ("kind", string("denied")),
                ("action", action),
                ("operation_receipt_id", receipt),
                ("code", string("scenario.capability_denied")),
                (
                    "message",
                    string("the deterministic scenario denied native execution"),
                ),
            ]));
        }

        let mut entries = accepted_header(action.clone(), receipt);
        if !matches!(&action, VmValue::String(value) if value.as_str() == "advance") {
            return Ok(dict(entries));
        }
        if self.scenario == HypothesisScenario::BudgetExhausted {
            entries[1] = ("kind", string("exhausted"));
            entries.extend([
                ("resource", string("api_calls")),
                ("elapsed_ms", VmValue::Int(500)),
            ]);
            return Ok(dict(entries));
        }

        let assignment_plan = required_dict(params, "assignment_plan")?;
        let registration = required_dict(required_dict(params, "plan")?, "registration")?;
        let metrics = metric_specs(registration)?;
        let assignments = required_list(assignment_plan, "assignments")?;
        let observations = assignments
            .iter()
            .map(|assignment| self.arm_observation(assignment, registration, &metrics))
            .collect::<Result<Vec<_>, _>>()?;
        entries.extend([
            (
                "assignment_plan_id",
                required(assignment_plan, "plan_id")?.clone(),
            ),
            (
                "observed_blocking_values",
                required(assignment_plan, "blocking_values")?.clone(),
            ),
            ("arm_observations", VmValue::List(Arc::new(observations))),
            ("elapsed_ms", VmValue::Int(500)),
        ]);
        Ok(dict(entries))
    }

    fn arm_observation(
        &self,
        assignment: &VmValue,
        registration: &DictMap,
        metrics: &[(String, String, f64, f64)],
    ) -> Result<VmValue, VmError> {
        let assignment = assignment
            .as_dict()
            .ok_or_else(|| invalid("assignment must be a record"))?;
        let arm_id = required(assignment, "arm_id")?.clone();
        let baseline_id = required_dict(registration, "baseline")?
            .get("id")
            .and_then(string_value)
            .ok_or_else(|| invalid("registration baseline id must be a string"))?;
        let arm_name = string_value(&arm_id).ok_or_else(|| invalid("arm id must be a string"))?;
        let is_baseline = arm_name == baseline_id;
        let metric_values = metrics.iter().map(|(id, direction, lo, hi)| {
            let midpoint = (lo + hi) / 2.0;
            let value = match self.scenario {
                HypothesisScenario::KnownBad if direction == "up" => {
                    if is_baseline {
                        *hi
                    } else {
                        *lo
                    }
                }
                HypothesisScenario::KnownBad => {
                    if is_baseline {
                        *lo
                    } else {
                        *hi
                    }
                }
                _ => midpoint,
            };
            (crate::value::intern_key(id), VmValue::Float(value))
        });
        Ok(dict([
            ("arm_id", arm_id),
            ("metrics", VmValue::dict(metric_values.collect::<DictMap>())),
            ("spend_delta_usd", VmValue::Float(0.0)),
            ("compute_delta_ms", VmValue::Int(1)),
            ("token_delta", VmValue::Int(0)),
            ("api_call_delta", VmValue::Int(0)),
            (
                "telemetry_status",
                string(if self.scenario == HypothesisScenario::MissingTelemetry {
                    "unavailable"
                } else {
                    "observed"
                }),
            ),
            ("capability_degradations", VmValue::List(Arc::new(vec![]))),
        ]))
    }
}

fn accepted_header(action: VmValue, receipt: VmValue) -> Vec<(&'static str, VmValue)> {
    vec![
        ("schema", string("harn.hypothesis.operation_result.v1")),
        ("kind", string("accepted")),
        ("action", action),
        ("operation_receipt_id", receipt),
        ("occurred_at", string("2026-01-01T00:00:00Z")),
        ("actor", string("harn-testbench")),
        ("source", string("hypothesis-scenario")),
    ]
}

fn event_payload_kind(params: &DictMap) -> Option<&str> {
    params
        .get("event")
        .and_then(VmValue::as_dict)
        .and_then(|event| event.get("content"))
        .and_then(VmValue::as_dict)
        .and_then(|content| content.get("payload"))
        .and_then(VmValue::as_dict)
        .and_then(|payload| payload.get("kind"))
        .and_then(string_value)
}

fn metric_specs(registration: &DictMap) -> Result<Vec<(String, String, f64, f64)>, VmError> {
    let metrics = required_dict(registration, "metrics")?;
    let mut values = vec![metric_spec(required_dict(metrics, "primary")?)?];
    for guardrail in required_list(metrics, "guardrails")? {
        values.push(metric_spec(
            guardrail
                .as_dict()
                .ok_or_else(|| invalid("guardrail metric must be a record"))?,
        )?);
    }
    Ok(values)
}

fn metric_spec(metric: &DictMap) -> Result<(String, String, f64, f64), VmError> {
    let id = string_value(required(metric, "id")?)
        .ok_or_else(|| invalid("metric id must be a string"))?
        .to_string();
    let direction = string_value(required(metric, "direction")?)
        .ok_or_else(|| invalid("metric direction must be a string"))?
        .to_string();
    let bounds = required_dict(metric, "bounds")?;
    Ok((
        id,
        direction,
        number(required(bounds, "lo")?)?,
        number(required(bounds, "hi")?)?,
    ))
}

fn number(value: &VmValue) -> Result<f64, VmError> {
    match value {
        VmValue::Float(value) => Ok(*value),
        VmValue::Int(value) => Ok(*value as f64),
        _ => Err(invalid("metric bound must be numeric")),
    }
}

fn required<'a>(values: &'a DictMap, key: &str) -> Result<&'a VmValue, VmError> {
    values
        .get(key)
        .ok_or_else(|| invalid(&format!("missing '{key}'")))
}

fn required_dict<'a>(values: &'a DictMap, key: &str) -> Result<&'a DictMap, VmError> {
    required(values, key)?
        .as_dict()
        .ok_or_else(|| invalid(&format!("'{key}' must be a record")))
}

fn required_list<'a>(values: &'a DictMap, key: &str) -> Result<&'a [VmValue], VmError> {
    match required(values, key)? {
        VmValue::List(values) => Ok(values.as_slice()),
        _ => Err(invalid(&format!("'{key}' must be a list"))),
    }
}

fn invalid(message: &str) -> VmError {
    VmError::Runtime(format!("hypothesis scenario adapter: {message}"))
}

fn string(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn string_value(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (crate::value::intern_key(key), value))
            .collect::<DictMap>(),
    )
}
