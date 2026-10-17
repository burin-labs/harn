//! Runtime wiring for catalog-declared reasoning execution modes.
//!
//! The catalog (`[llm.models.<id>].reasoning_modes`) is the single source of
//! truth for provider knobs that change how MUCH work a model does before it
//! answers — today OpenAI's `reasoning.mode = "pro"` on the Responses API.
//!
//! This is deliberately a sibling of [`crate::llm::serving_tiers`] rather than
//! a new tier id inside it. A serving tier changes how fast the same work is
//! served and is priced by a different per-token RATE; a reasoning mode keeps
//! the rate and changes the token COUNT, because the provider bills the extra
//! work at the model's standard rates. Folding one into the other would make
//! `cost_multiplier` mean two different things and would let `speed = "fast"`
//! silently select an execution mode.
//!
//! `reasoning.mode` is also nested, sharing its object with `reasoning.effort`,
//! so the knob is addressed by a path and merged into any object already
//! present instead of overwriting it.
//!
//! Not here yet: reading the provider's echoed mode back onto the usage
//! record. Unlike a serving tier, a reasoning mode changes no per-token rate,
//! so no billing decision depends on the echo and a reader would be dead
//! weight today. The catalog already carries `response_values` for it, so the
//! data contract is in place when a surface needs to show "you asked for pro
//! and pro is what ran".

use crate::llm_config::{model_catalog_entry, ReasoningModeDef};

/// The catalog id of the default mode. It is never declared in the catalog;
/// requesting it is the same as requesting nothing.
pub(crate) use harn_provider_catalog::config::STANDARD_MODE_ID;

/// Catalog lifecycle status that disqualifies a reasoning mode from use.
const DEPRECATED_STATUS: &str = "deprecated";

/// Resolve a named reasoning mode from the catalog, if any.
pub(crate) fn lookup(model: &str, mode_id: &str) -> Option<ReasoningModeDef> {
    model_catalog_entry(model).and_then(|entry| {
        entry
            .reasoning_modes
            .into_iter()
            .find(|mode| mode.id == mode_id)
    })
}

/// Whether a reasoning mode is currently usable.
pub(crate) fn is_usable(mode: &ReasoningModeDef) -> bool {
    mode.status.as_deref() != Some(DEPRECATED_STATUS)
}

/// Outcome of validating a reasoning-mode request against the catalog.
pub(crate) enum ReasoningModeGate {
    /// The model offers this usable mode; engage it. The catalog metadata is
    /// re-read by the provider body builder, so the variant carries no payload.
    Usable,
    /// The model does not declare this mode at all.
    Unsupported,
    /// The mode is declared but deprecated; carries the catalog note.
    Deprecated { note: Option<String> },
}

/// Classify a non-default `reasoning_mode` request for the resolved model.
pub(crate) fn gate(model: &str, mode_id: &str) -> ReasoningModeGate {
    match lookup(model, mode_id) {
        None => ReasoningModeGate::Unsupported,
        Some(mode) if !is_usable(&mode) => ReasoningModeGate::Deprecated { note: mode.note },
        Some(mode) if mode.request.is_none() => ReasoningModeGate::Unsupported,
        Some(_) => ReasoningModeGate::Usable,
    }
}

/// Resolve a caller's `reasoning_mode` option against the catalog.
///
/// Returns the mode id to carry on the request, or `None` for the provider
/// default. `standard` is folded to `None` here so exactly one representation
/// of "no mode" reaches the rest of the pipeline. When `enforce_gates` is set,
/// a mode the model does not declare is a thrown error rather than a silently
/// ignored option: the caller is asking for materially more expensive work, so
/// failing to deliver it must not look like success.
pub(crate) fn resolve_requested(
    requested: Option<&str>,
    model: &str,
    provider: &str,
) -> Result<Option<String>, crate::value::VmError> {
    let mode_id = match requested {
        None | Some(STANDARD_MODE_ID) => return Ok(None),
        Some(other) => other,
    };
    let thrown =
        |message: String| crate::value::VmError::Thrown(crate::value::VmValue::string(message));
    match gate(model, mode_id) {
        ReasoningModeGate::Usable => {}
        ReasoningModeGate::Unsupported => {
            return Err(thrown(format!(
                "reasoning_mode: model \"{model}\" (provider \"{provider}\") declares no \
                 \"{mode_id}\" reasoning mode in the catalog; remove `reasoning_mode` or \
                 pick a model that advertises it under `reasoning_modes`"
            )));
        }
        ReasoningModeGate::Deprecated { note } => {
            let detail = note.map(|n| format!(" ({n})")).unwrap_or_default();
            return Err(thrown(format!(
                "reasoning_mode: the \"{mode_id}\" reasoning mode for model \"{model}\" is \
                 deprecated{detail}"
            )));
        }
    }
    Ok(Some(mode_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_openai_pro_mode_knob() {
        let pro = lookup("gpt-5.6-sol", "pro").expect("sol advertises pro reasoning mode");
        let request = pro.request.as_ref().expect("pro mode has a request knob");
        assert_eq!(request.param_path, vec!["reasoning", "mode"]);
        assert_eq!(request.value, "pro");
    }

    #[test]
    fn gate_rejects_a_model_that_does_not_declare_the_mode() {
        assert!(matches!(
            gate("claude-opus-5", "pro"),
            ReasoningModeGate::Unsupported
        ));
        assert!(matches!(
            gate("gpt-5.6-sol", "pro"),
            ReasoningModeGate::Usable
        ));
    }
}
