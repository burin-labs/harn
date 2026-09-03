//! Projecting a caller's requested data posture onto the resolved provider's
//! declared controls, and the per-request receipt saying what happened.
//!
//! The receipt is the point. A host that asks for the strictest available
//! posture and gets back nothing must be able to tell "we asked and this
//! provider exposes no per-request control" from "we never asked" from "we
//! have not researched this provider" — three outcomes that all look like an
//! unchanged request body on the wire. Collapsing them to a boolean would let
//! absence read as success, which is the failure this feature exists to
//! prevent.
//!
//! One plan drives both application legs (body fields and headers) and the
//! receipt, so the receipt cannot describe a control the wire did not carry.

use serde::{Deserialize, Serialize};

use crate::llm_config::{
    DataControlDef, DataControlDialect, DataControlEffect, DataControlLocation, DataControlScope,
    DataControlsDef, DataPosture,
};

use super::dialect::StreamProtocol;

/// Exhaustive map from the live wire dialect to its registry name.
///
/// Written as a total match on purpose: adding a `StreamProtocol` variant
/// fails to compile here until someone decides what data controls mean on it.
pub(crate) fn dialect_of(protocol: StreamProtocol) -> DataControlDialect {
    match protocol {
        StreamProtocol::AnthropicSse => DataControlDialect::AnthropicSse,
        StreamProtocol::OpenAiSse => DataControlDialect::OpenAiSse,
        StreamProtocol::OllamaNdjson => DataControlDialect::OllamaNdjson,
        StreamProtocol::GeminiJson => DataControlDialect::GeminiJson,
        StreamProtocol::GeminiInteractionsSse => DataControlDialect::GeminiInteractionsSse,
    }
}

/// What Harn did about data controls on one request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataControlsOutcome {
    /// The caller left the posture at `default`. Harn set nothing and the
    /// provider's server-side default applies. This is not a claim about what
    /// that default is.
    NotRequested,
    /// The caller asked for the strictest available posture and at least one
    /// declared control was written onto this request.
    Applied,
    /// The caller asked, the provider is researched, and it exposes no
    /// per-request control on this dialect. The posture was NOT achieved.
    NoControlAvailable,
    /// The caller asked and this provider carries no researched declaration.
    /// Nobody has checked; this is not the same claim as
    /// `no_control_available`.
    ProviderUnresearched,
}

impl DataControlsOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Applied => "applied",
            Self::NoControlAvailable => "no_control_available",
            Self::ProviderUnresearched => "provider_unresearched",
        }
    }
}

/// One control as actually written onto this request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedDataControl {
    /// `"body"` or `"header"`.
    pub location: String,
    /// Dotted body path or header name.
    pub name: String,
    /// The value written, rendered as it went onto the wire.
    pub value: serde_json::Value,
    /// `"retention"` or `"training"`.
    pub effect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
}

/// The per-request receipt. Attached to provider telemetry so a host, an eval
/// trial, and a transcript all read the same fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataControlsReceipt {
    /// `"default"` or `"strictest_available"` — what the caller asked for.
    pub requested_posture: String,
    pub outcome: DataControlsOutcome,
    pub provider: String,
    /// The registry's `control_scope` for this provider, absent when the
    /// provider is unresearched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_scope: Option<DataControlScope>,
    /// Controls written onto this request. Empty for every outcome but
    /// `applied`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<AppliedDataControl>,
    /// The registry's one-line note for this provider, e.g. that the strict
    /// posture exists but only by contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DataControlsReceipt {
    pub fn as_vm_dict(&self) -> crate::value::VmValue {
        let json = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        crate::schema::json_to_vm_value(&json)
    }
}

/// A resolved plan: the body and header writes owed to the request, plus the
/// receipt describing them.
///
/// Resolution and writing are separate steps on purpose. The body half is
/// written at the very end of request shaping, after the caller's
/// `provider_overrides` escape hatch, so a top-level override cannot silently
/// clobber a declared privacy control and leave the receipt claiming it was
/// applied. The registry wins; the receipt stays true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataControlsPlan {
    pub body_writes: Vec<(String, serde_json::Value)>,
    pub headers: Vec<(String, String)>,
    pub receipt: DataControlsReceipt,
}

impl DataControlsPlan {
    /// Write the body half. Call this after every other body mutation.
    pub(crate) fn write_body(&self, body: &mut serde_json::Value) {
        for (path, value) in &self.body_writes {
            set_body_path(body, path, value.clone());
        }
    }
}

/// Why a route is refused under the strict posture, as a sentence a host can
/// show a person without rewording a privacy claim.
///
/// Applying "every declared per-request control" is not the same as achieving
/// the strict posture. On a route the provider trains on by default and
/// exposes no control for, there are zero controls to apply, so the request
/// would go out unchanged, the receipt would read `no_control_available`, and
/// the traffic would be trained on anyway. That is the absence-reads-as-
/// success failure this whole module exists to prevent, so the strict posture
/// refuses the call instead of quietly proceeding.
///
/// The refusal is deliberately narrow. It fires only when the effective
/// declaration says `trains` outright. `unspecified` and an unresearched
/// provider do not refuse, because "nobody has checked" must not silently
/// acquire the force of "we checked and it trains"; those keep reporting
/// themselves through the receipt.
pub(crate) fn training_refusal(
    provider: &str,
    model: &str,
    posture: DataPosture,
) -> Option<String> {
    if posture != DataPosture::StrictestAvailable {
        return None;
    }
    if crate::llm_config::effective_training_default(provider, model)
        != Some(crate::llm_config::TrainingDefault::Trains)
    {
        return None;
    }
    let sources = crate::llm_config::model_data_controls(model)
        .map(|controls| controls.sources)
        .or_else(|| {
            crate::llm_config::provider_config(provider)
                .and_then(|definition| definition.data_controls)
                .map(|controls| controls.sources)
        })
        .unwrap_or_default();
    let citation = sources
        .first()
        .map(|url| format!(" See {url}."))
        .unwrap_or_default();
    Some(format!(
        "data_controls: refusing to call {model} on {provider}. The catalog records that this \
         route trains on API traffic, and it publishes no per-request control that would stop \
         it, so the strictest_available posture cannot be honored here. Choose a route that \
         does not train, or ask for the default posture to proceed knowing the traffic is \
         trained on.{citation}"
    ))
}

/// Resolve the caller's posture against the registry into the writes this
/// request owes and the receipt describing them.
///
/// Under the `Default` posture the plan is empty, so the request stays
/// byte-for-byte what the provider adapter built.
pub(crate) fn resolve(
    provider: &str,
    dialect: DataControlDialect,
    posture: DataPosture,
) -> DataControlsPlan {
    let requested_posture = match posture {
        DataPosture::Default => "default",
        DataPosture::StrictestAvailable => "strictest_available",
    }
    .to_string();

    let declaration = crate::llm_config::provider_config(provider)
        .and_then(|definition| definition.data_controls);

    if posture == DataPosture::Default {
        return DataControlsPlan {
            body_writes: Vec::new(),
            headers: Vec::new(),
            receipt: DataControlsReceipt {
                requested_posture,
                outcome: DataControlsOutcome::NotRequested,
                provider: provider.to_string(),
                control_scope: declaration.as_ref().map(|entry| entry.control_scope),
                applied: Vec::new(),
                note: declaration.and_then(|entry| entry.note),
            },
        };
    }

    let Some(declaration) = declaration else {
        return DataControlsPlan {
            body_writes: Vec::new(),
            headers: Vec::new(),
            receipt: DataControlsReceipt {
                requested_posture,
                outcome: DataControlsOutcome::ProviderUnresearched,
                provider: provider.to_string(),
                control_scope: None,
                applied: Vec::new(),
                note: None,
            },
        };
    };

    let mut body_writes = Vec::new();
    let mut headers = Vec::new();
    let mut applied = Vec::new();
    for control in declaration.controls_for_dialect(dialect) {
        match control.location {
            DataControlLocation::Body => {
                body_writes.push((control.name.clone(), control.value.as_json()));
                applied.push(applied_receipt(control, control.value.as_json()));
            }
            DataControlLocation::Header => {
                let rendered = control.value.as_header_value();
                headers.push((control.name.clone(), rendered.clone()));
                applied.push(applied_receipt(
                    control,
                    serde_json::Value::String(rendered),
                ));
            }
        }
    }

    let outcome = if applied.is_empty() {
        DataControlsOutcome::NoControlAvailable
    } else {
        DataControlsOutcome::Applied
    };

    DataControlsPlan {
        body_writes,
        headers,
        receipt: DataControlsReceipt {
            requested_posture,
            outcome,
            provider: provider.to_string(),
            control_scope: Some(declaration.control_scope),
            applied,
            note: declaration.note,
        },
    }
}

fn applied_receipt(control: &DataControlDef, value: serde_json::Value) -> AppliedDataControl {
    AppliedDataControl {
        location: match control.location {
            DataControlLocation::Body => "body",
            DataControlLocation::Header => "header",
        }
        .to_string(),
        name: control.name.clone(),
        value,
        effect: match control.effect {
            DataControlEffect::Retention => "retention",
            DataControlEffect::Training => "training",
        }
        .to_string(),
        caveat: control.caveat.clone(),
    }
}

/// Write `value` at a dotted path, creating intermediate objects.
///
/// A non-object sitting where an intermediate object belongs is replaced: the
/// registry, not a caller's ad-hoc body, owns the shape of a declared control.
fn set_body_path(body: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let mut cursor = body;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            if let Some(map) = cursor.as_object_mut() {
                map.insert(segment.to_string(), value);
            }
            return;
        }
        if !cursor.get(segment).is_some_and(|child| child.is_object()) {
            if let Some(map) = cursor.as_object_mut() {
                map.insert(segment.to_string(), serde_json::json!({}));
            } else {
                return;
            }
        }
        cursor = match cursor.get_mut(segment) {
            Some(child) => child,
            None => return,
        };
    }
}

/// Declared posture for a provider, for consumers that want the fact without
/// making a request.
pub fn declaration_for(provider: &str) -> Option<DataControlsDef> {
    crate::llm_config::provider_config(provider).and_then(|entry| entry.data_controls)
}

#[cfg(test)]
#[path = "data_controls_tests.rs"]
mod data_controls_tests;
