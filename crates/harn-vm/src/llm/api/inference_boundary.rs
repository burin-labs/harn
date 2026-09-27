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

/// Catalog facts used to admit a route under an inference boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceCatalogEvidence {
    pub local_runtime: bool,
    pub open_weight: Option<bool>,
}

/// Host-owned run ceiling. Embedders grant this through the session
/// environment; an unconfigured standalone Harn run keeps its existing policy.
pub const HOST_BOUNDARY_ENV: &str = "HARN_INFERENCE_BOUNDARY_JSON";

fn host_boundary() -> Result<Option<InferenceBoundary>, String> {
    let raw = crate::stdlib::process::session_env_var(HOST_BOUNDARY_ENV)
        .map_err(|_| "inference_boundary.host_environment_unavailable".to_string())?;
    raw.as_deref().map(parse_host_boundary).transpose()
}

fn parse_host_boundary(raw: &str) -> Result<InferenceBoundary, String> {
    serde_json::from_str(raw).map_err(|_| "inference_boundary.host_boundary_malformed".to_string())
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
    let host = host_boundary().unwrap_or(Some(InferenceBoundary {
        reach: InferenceReach::LocalOnly,
        allow_training_discounts: false,
    }));
    meet(meet(host, current_ambient_boundary()), requested)
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
    let evidence = catalog_evidence(provider, model)?;
    let facts = RouteFacts {
        local: evidence.local_runtime,
        open_weight: evidence.open_weight,
        training_default: controls.training_default,
        training_control_applied: controls
            .applied
            .iter()
            .any(|control| control.effect == "training"),
    };
    decide(boundary, provider, model, facts)
}

pub(crate) fn catalog_evidence(
    provider: &str,
    model: &str,
) -> Result<InferenceCatalogEvidence, String> {
    let provider_def = crate::llm_config::provider_config(provider)
        .ok_or_else(|| format!("inference_boundary.catalog_provider_unknown: {provider}"))?;
    if provider_def.local_runtime.is_some()
        && !is_loopback_endpoint(&crate::llm_config::resolve_base_url(&provider_def))
    {
        return Err(format!(
            "inference_boundary.local_endpoint_untrusted: {provider}/{model} does not resolve to a loopback endpoint"
        ));
    }
    Ok(InferenceCatalogEvidence {
        local_runtime: provider_def.local_runtime.is_some(),
        open_weight: crate::llm_config::model_catalog_entry_for_route(provider, model)
            .and_then(|row| row.open_weight),
    })
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
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
    controls: &DataControlsReceipt,
) -> Result<Option<&'static str>, String> {
    // An explicitly supplied but malformed host ceiling is a refusal even
    // when the resolved model is local; no fallback may mask bad authority.
    host_boundary()?;
    let Some(boundary) = effective(boundary) else {
        return Ok(None);
    };
    governing_rule(boundary, provider, model, controls).map(Some)
}

pub(crate) fn preflight_chat(
    request: &super::options::LlmRequestPayload,
) -> Result<Option<&'static str>, String> {
    // The shared HTTP transports apply the resolved data-control plan again
    // at send time. Native/ACP adapters do not, so never credit a planned
    // no-training control to one of those routes.
    let shared_transport =
        !matches!(
            request.provider.as_str(),
            "bedrock" | "azure_openai" | "vertex" | "gemini"
        ) && !crate::llm::providers::AcpProvider::is_configured_acp(&request.provider);
    let posture = if shared_transport {
        request.data_controls
    } else {
        crate::llm_config::DataPosture::Default
    };
    let controls = super::data_controls::resolve(
        &request.provider,
        &request.model,
        super::data_controls::dialect_of(
            super::DialectContract::for_request(request).stream_protocol(),
        ),
        posture,
    );
    preflight(
        request.inference_boundary,
        &request.provider,
        &request.model,
        &controls.receipt,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_provider_tag_does_not_make_a_remote_endpoint_local() {
        assert!(is_loopback_endpoint("http://127.0.0.2:11434"));
        assert!(is_loopback_endpoint("http://[::1]:11434"));
        assert!(!is_loopback_endpoint("https://localhost.example.com:11434"));
        assert!(!is_loopback_endpoint("https://api.example.com:11434"));
    }

    #[test]
    fn malformed_host_ceiling_fails_closed() {
        assert!(
            parse_host_boundary(r#"{"reach":"unexpected","allow_training_discounts":true}"#)
                .unwrap_err()
                .contains("host_boundary_malformed")
        );
    }

    #[test]
    fn host_ceiling_reads_a_literal_session_grant() {
        use crate::security::{
            EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment,
        };

        let environment = SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![GrantSpec {
                name: HOST_BOUNDARY_ENV.into(),
                source: GrantSourceSpec::Literal {
                    value: r#"{"reach":"local_only","allow_training_discounts":false}"#.into(),
                },
                expose_as_env: Some(HOST_BOUNDARY_ENV.into()),
                for_command: None,
            }],
            &|_| None,
        )
        .unwrap();
        let _guard = crate::stdlib::process::declare_session_environment_if_absent(environment);
        assert_eq!(
            effective(None),
            Some(InferenceBoundary {
                reach: InferenceReach::LocalOnly,
                allow_training_discounts: false,
            })
        );
    }

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
