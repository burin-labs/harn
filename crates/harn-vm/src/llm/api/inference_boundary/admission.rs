//! A value-free read of the same route decision inference enforces.

use serde::{Deserialize, Serialize};

use super::{catalog_evidence, chat_controls, decide, effective_result};
use super::{DenialRule, InferenceBoundary, RouteFacts};
use crate::llm::api::data_controls::DataControlsOutcome;
use crate::llm_config::DataPosture;

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceAdmissionRequest {
    /// Concrete route identity after model selection, not a model alias.
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub boundary: Option<InferenceBoundary>,
    #[serde(default)]
    pub data_controls: Option<DataPosture>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InferenceAdmissionStatus {
    Admitted,
    Denied,
    Unknown,
}

/// Admission says only what the current policy and route facts permit.
/// It does not establish credentials, availability, model readiness, or future
/// admission. Actual inference resolves and enforces its boundary again.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct InferenceAdmissionSnapshot {
    pub schema: String,
    pub provider: String,
    pub model: String,
    pub status: InferenceAdmissionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_boundary: Option<InferenceBoundary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governing_rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_runtime: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weight: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub training_default: Option<String>,
    /// A control the selected chat transport can apply, not an assertion that
    /// this preview sent a request or applied a control to live traffic.
    pub training_control_planned: bool,
    /// The data posture inference will send on this route: the request's own
    /// `data_controls` when named, otherwise the catalog's
    /// `[data_controls_policy] default_posture`, and `default` on a transport
    /// that cannot carry per-request controls. The same value the per-request
    /// receipt reports as `requested_posture`.
    pub data_posture: DataPosture,
    /// What that posture will do on this route, as the receipt will say it.
    /// Absent when the route never resolved, so an unresolved route does not
    /// read as `not_requested`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_controls_outcome: Option<DataControlsOutcome>,
    /// The route's own data-handling note when its model row declares one,
    /// otherwise its provider's. Absent when neither level carries a note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_controls_note: Option<String>,
    /// Why the requested data posture refuses this route, when it does. A
    /// typed fact so a host switches on it instead of matching
    /// `governing_rule` text. Absent when the posture admits the route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_controls_refusal: Option<DataControlsRefusal>,
}

/// Why a data posture refuses a route outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataControlsRefusal {
    /// `strictest_available` on a route the catalog records as training on
    /// API traffic, with no per-request control that would stop it.
    TrainingWithoutControl,
}

/// Governing rule when the requested posture refuses the route outright: the
/// catalog records that it trains on API traffic and it exposes no control
/// that would stop it, so `strictest_available` cannot be honored.
pub const DATA_CONTROLS_TRAINING_REFUSED_RULE: &str = "data_controls.training_refused";

/// Schema projections of the owning request and snapshot types for embedders
/// and the host-binding generator. No hand-maintained field list is involved.
pub fn inference_admission_schemas() -> [(&'static str, serde_json::Value); 2] {
    [
        (
            "InferenceAdmissionRequest",
            serde_json::to_value(schemars::schema_for!(InferenceAdmissionRequest))
                .expect("request schema encodes"),
        ),
        (
            "InferenceAdmissionSnapshot",
            serde_json::to_value(schemars::schema_for!(InferenceAdmissionSnapshot))
                .expect("snapshot schema encodes"),
        ),
    ]
}

/// Resolve a preview without provider I/O or credential resolution. The
/// endpoint itself and control values never enter the returned snapshot.
pub fn preview_inference_admission(
    request: &InferenceAdmissionRequest,
) -> InferenceAdmissionSnapshot {
    let mut snapshot = InferenceAdmissionSnapshot {
        schema: "harn.llm.inference-admission/v1".into(),
        provider: request.provider.clone(),
        model: request.model.clone(),
        status: InferenceAdmissionStatus::Unknown,
        effective_boundary: None,
        governing_rule: None,
        local_runtime: None,
        open_weight: None,
        training_default: None,
        training_control_planned: false,
        data_posture: request
            .data_controls
            .unwrap_or_else(crate::llm_config::data_controls_default_posture),
        data_controls_outcome: None,
        data_controls_note: None,
        data_controls_refusal: None,
    };
    // Malformed host authority remains distinct from a valid policy refusal.
    // Never return its supplied bytes or guess that a default was admitted.
    snapshot.effective_boundary = match effective_result(request.boundary) {
        Ok(boundary) => boundary,
        Err(rule) => {
            snapshot.governing_rule = Some(rule);
            return snapshot;
        }
    };
    if request.provider.trim().is_empty() || request.model.trim().is_empty() {
        snapshot.governing_rule = Some("inference_boundary.route_unresolved".into());
        return snapshot;
    }
    let evidence = match catalog_evidence(&request.provider, &request.model) {
        Ok(evidence) => evidence,
        Err(denial) => {
            snapshot.governing_rule = Some(denial.rule.as_str().into());
            snapshot.status = if matches!(denial.rule, DenialRule::UnknownProvider) {
                InferenceAdmissionStatus::Unknown
            } else {
                InferenceAdmissionStatus::Denied
            };
            return snapshot;
        }
    };
    snapshot.local_runtime = Some(evidence.local_runtime);
    snapshot.open_weight = evidence.open_weight;
    let controls = chat_controls(&request.provider, &request.model, snapshot.data_posture);
    // The plan's own posture, not the requested one: a transport that cannot
    // carry per-request controls sends `default` whatever was asked.
    snapshot.data_posture = serde_json::from_value(serde_json::Value::String(
        controls.receipt.requested_posture.clone(),
    ))
    .unwrap_or(DataPosture::Default);
    snapshot.data_controls_outcome = Some(controls.receipt.outcome);
    snapshot.data_controls_note = controls.receipt.note.clone();
    snapshot.training_default =
        controls
            .receipt
            .training_default
            .and_then(|value| match serde_json::to_value(value) {
                Ok(serde_json::Value::String(value)) => Some(value),
                _ => None,
            });
    snapshot.training_control_planned = controls
        .receipt
        .applied
        .iter()
        .any(|control| control.effect == "training");
    // Inference refuses this route before building the request; a preview
    // that admitted it would promise a call that cannot be made.
    if crate::llm::api::data_controls::training_refusal(
        &request.provider,
        &request.model,
        snapshot.data_posture,
        &controls.receipt,
    )
    .is_some()
    {
        snapshot.status = InferenceAdmissionStatus::Denied;
        snapshot.governing_rule = Some(DATA_CONTROLS_TRAINING_REFUSED_RULE.into());
        snapshot.data_controls_refusal = Some(DataControlsRefusal::TrainingWithoutControl);
        return snapshot;
    }
    let Some(boundary) = snapshot.effective_boundary else {
        snapshot.status = InferenceAdmissionStatus::Admitted;
        return snapshot;
    };
    let facts = RouteFacts {
        local: evidence.local_runtime,
        open_weight: evidence.open_weight,
        training_default: controls.receipt.training_default,
        training_control_applied: snapshot.training_control_planned,
    };
    match decide(boundary, &request.provider, &request.model, facts) {
        Ok(rule) => {
            snapshot.status = InferenceAdmissionStatus::Admitted;
            snapshot.governing_rule = Some(rule.into());
        }
        Err(denial) => {
            snapshot.governing_rule = Some(denial.rule.as_str().into());
            snapshot.status = match denial.rule {
                DenialRule::TrainingUnknown | DenialRule::UnknownProvider => {
                    InferenceAdmissionStatus::Unknown
                }
                DenialRule::HostedOpenWeight if evidence.open_weight.is_none() => {
                    InferenceAdmissionStatus::Unknown
                }
                _ => InferenceAdmissionStatus::Denied,
            };
        }
    }
    snapshot
}

#[cfg(test)]
mod tests;
