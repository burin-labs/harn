//! An embedder's ceiling on where one inference request may be sent.
//! The catalog supplies route facts; callers only choose their allowed reach.

use serde::{Deserialize, Serialize};

use crate::llm_config::TrainingDefault;
use crate::value::{VmError, VmValue};

use super::data_controls::DataControlsReceipt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceReach {
    LocalOnly,
    HostedOpenWeight,
    AnyHosted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceBoundary {
    pub reach: InferenceReach,
    pub allow_training_discounts: bool,
}

thread_local! {
    static AMBIENT_BOUNDARY: std::cell::RefCell<Option<InferenceBoundary>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn swap_ambient_boundary(next: Option<InferenceBoundary>) -> Option<InferenceBoundary> {
    AMBIENT_BOUNDARY.with(|slot| slot.replace(next))
}

pub(crate) fn current_ambient_boundary() -> Option<InferenceBoundary> {
    AMBIENT_BOUNDARY.with(|slot| *slot.borrow())
}

fn reach_rank(reach: InferenceReach) -> u8 {
    match reach {
        InferenceReach::LocalOnly => 0,
        InferenceReach::HostedOpenWeight => 1,
        InferenceReach::AnyHosted => 2,
    }
}

/// Nested scopes and explicit call options may tighten, never widen, the
/// destination ceiling installed by an outer host run.
pub(crate) fn meet(
    inherited: Option<InferenceBoundary>,
    requested: Option<InferenceBoundary>,
) -> Option<InferenceBoundary> {
    match (inherited, requested) {
        (Some(outer), Some(inner)) => Some(InferenceBoundary {
            reach: if reach_rank(outer.reach) <= reach_rank(inner.reach) {
                outer.reach
            } else {
                inner.reach
            },
            allow_training_discounts: outer.allow_training_discounts
                && inner.allow_training_discounts,
        }),
        (one, None) | (None, one) => one,
    }
}

pub(crate) fn effective(requested: Option<InferenceBoundary>) -> Option<InferenceBoundary> {
    meet(current_ambient_boundary(), requested)
}

pub(crate) fn parse_vm_value(value: &VmValue) -> Result<InferenceBoundary, VmError> {
    let VmValue::Dict(fields) = value else {
        return Err(VmError::Runtime(
            "inference_boundary: expected {reach, allow_training_discounts}".into(),
        ));
    };
    serde_json::from_value(crate::llm::helpers::vm_value_dict_to_json(fields))
        .map_err(|error| VmError::Runtime(format!("inference_boundary: {error}")))
}

#[derive(Clone, Copy)]
struct RouteFacts {
    local: bool,
    open_weight: Option<bool>,
    training_default: Option<TrainingDefault>,
    training_control_applied: bool,
}

/// Decide against the concrete route after routing and failover have resolved.
/// A missing catalog fact is a refusal, never a permissive default.
pub(crate) fn governing_rule(
    boundary: InferenceBoundary,
    provider: &str,
    model: &str,
    controls: &DataControlsReceipt,
) -> Result<&'static str, String> {
    let provider_def = crate::llm_config::provider_config(provider)
        .ok_or_else(|| format!("inference_boundary.catalog_provider_unknown: {provider}"))?;
    let facts = RouteFacts {
        local: provider_def.local_runtime.is_some(),
        open_weight: crate::llm_config::model_catalog_entry_for_route(provider, model)
            .and_then(|row| row.open_weight),
        training_default: controls.training_default,
        training_control_applied: controls
            .applied
            .iter()
            .any(|control| control.effect == "training"),
    };
    decide(boundary, provider, model, facts)
}

fn decide(
    boundary: InferenceBoundary,
    provider: &str,
    model: &str,
    facts: RouteFacts,
) -> Result<&'static str, String> {
    let rule = if facts.local {
        "inference_boundary.local_runtime"
    } else {
        match boundary.reach {
            InferenceReach::LocalOnly => {
                return Err(format!(
                    "inference_boundary.local_only: hosted route {provider}/{model} refused"
                ));
            }
            InferenceReach::HostedOpenWeight => {
                if facts.open_weight != Some(true) {
                    return Err(format!(
                        "inference_boundary.hosted_open_weight: {provider}/{model} is not cataloged as open-weight"
                    ));
                }
                "inference_boundary.hosted_open_weight"
            }
            InferenceReach::AnyHosted => "inference_boundary.any_hosted",
        }
    };

    if facts.local {
        return Ok(rule);
    }
    match facts.training_default {
        Some(TrainingDefault::DoesNotTrain) => Ok(rule),
        Some(TrainingDefault::Trains) if boundary.allow_training_discounts => Ok(rule),
        Some(TrainingDefault::Trains) if facts.training_control_applied => {
            Ok(rule)
        }
        Some(TrainingDefault::Trains) => Err(format!(
            "inference_boundary.training_default: {provider}/{model} trains on API traffic without an applied training control"
        )),
        _ => Err(format!(
            "inference_boundary.training_unknown: no verified no-training fact for {provider}/{model}"
        )),
    }
}

/// Conservative guard shared by every live chat transport, including native
/// adapters that do not yet project per-request data controls. The shared
/// HTTP transport checks again against the control plan it actually writes.
pub(crate) fn preflight(
    boundary: Option<InferenceBoundary>,
    provider: &str,
    model: &str,
) -> Result<Option<&'static str>, String> {
    let Some(boundary) = effective(boundary) else {
        return Ok(None);
    };
    let facts = super::data_controls::resolve(
        provider,
        model,
        crate::llm_config::DataControlDialect::OpenAiSse,
        crate::llm_config::DataPosture::Default,
    );
    governing_rule(boundary, provider, model, &facts.receipt).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ceiling_refuses_a_hosted_route_before_training_facts() {
        let result = decide(
            InferenceBoundary {
                reach: InferenceReach::LocalOnly,
                allow_training_discounts: false,
            },
            "hosted",
            "model",
            RouteFacts {
                local: false,
                open_weight: Some(true),
                training_default: Some(TrainingDefault::DoesNotTrain),
                training_control_applied: false,
            },
        );
        assert!(result.unwrap_err().contains("local_only"));
    }

    #[test]
    fn unknown_training_never_reads_as_no_training() {
        let result = decide(
            InferenceBoundary {
                reach: InferenceReach::AnyHosted,
                allow_training_discounts: false,
            },
            "hosted",
            "model",
            RouteFacts {
                local: false,
                open_weight: None,
                training_default: None,
                training_control_applied: false,
            },
        );
        assert!(result.unwrap_err().contains("training_unknown"));
    }

    #[test]
    fn open_weight_tier_needs_positive_catalog_evidence() {
        let boundary = InferenceBoundary {
            reach: InferenceReach::HostedOpenWeight,
            allow_training_discounts: false,
        };
        let facts = RouteFacts {
            local: false,
            open_weight: None,
            training_default: Some(TrainingDefault::DoesNotTrain),
            training_control_applied: false,
        };
        assert!(decide(boundary, "hosted", "model", facts)
            .unwrap_err()
            .contains("hosted_open_weight"));
        assert_eq!(
            decide(
                boundary,
                "hosted",
                "model",
                RouteFacts {
                    open_weight: Some(true),
                    ..facts
                }
            ),
            Ok("inference_boundary.hosted_open_weight")
        );
    }

    #[test]
    fn training_route_needs_applied_control_or_explicit_discount() {
        let boundary = InferenceBoundary {
            reach: InferenceReach::AnyHosted,
            allow_training_discounts: false,
        };
        let facts = RouteFacts {
            local: false,
            open_weight: Some(true),
            training_default: Some(TrainingDefault::Trains),
            training_control_applied: false,
        };
        assert!(decide(boundary, "hosted", "model", facts)
            .unwrap_err()
            .contains("training_default"));
        assert_eq!(
            decide(
                InferenceBoundary {
                    allow_training_discounts: true,
                    ..boundary
                },
                "hosted",
                "model",
                facts,
            ),
            Ok("inference_boundary.any_hosted")
        );
    }

    #[tokio::test]
    async fn scope_follows_a_spawned_worker_and_refuses_widening() {
        let outer = InferenceBoundary {
            reach: InferenceReach::LocalOnly,
            allow_training_discounts: false,
        };
        let wider = InferenceBoundary {
            reach: InferenceReach::AnyHosted,
            allow_training_discounts: true,
        };
        crate::orchestration::scope_inference_boundary(outer, async move {
            assert_eq!(effective(Some(wider)), Some(outer));
            let child = tokio::spawn(crate::orchestration::scope_inline_subtask(async move {
                effective(Some(wider))
            }));
            assert_eq!(child.await.expect("worker joins"), Some(outer));
        })
        .await;
        assert_eq!(current_ambient_boundary(), None);
    }
}
