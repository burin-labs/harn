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
pub(crate) const STANDARD_MODE_ID: &str = "standard";

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
    enforce_gates: bool,
) -> Result<Option<String>, crate::value::VmError> {
    let mode_id = match requested {
        None | Some(STANDARD_MODE_ID) => return Ok(None),
        Some(other) => other,
    };
    if enforce_gates {
        let thrown = |message: String| {
            crate::value::VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(
                message,
            )))
        };
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
    }
    Ok(Some(mode_id.to_string()))
}

/// Inject the reasoning-mode knob into an already-built provider body.
///
/// No-op when no mode is requested, when the request names the default mode,
/// or when the model has no usable declaration — so it is safe to call
/// unconditionally from every provider body builder.
///
/// Intermediate objects along `param_path` are created when absent. A value
/// already present at an intermediate segment is merged into, never replaced,
/// which is what keeps a caller's `reasoning.effort` intact.
pub(crate) fn apply_request_knob(body: &mut serde_json::Value, model: &str, mode_id: Option<&str>) {
    let Some(mode_id) = mode_id.filter(|id| *id != STANDARD_MODE_ID) else {
        return;
    };
    let Some(mode) = lookup(model, mode_id).filter(is_usable) else {
        return;
    };
    let Some(request) = mode.request else {
        return;
    };
    let Some((leaf, parents)) = request.param_path.split_last() else {
        return;
    };
    let mut cursor = body;
    for segment in parents {
        if !cursor.is_object() {
            return;
        }
        cursor = cursor
            .as_object_mut()
            .expect("checked object above")
            .entry(segment.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    let Some(object) = cursor.as_object_mut() else {
        return;
    };
    object.insert(leaf.clone(), serde_json::Value::String(request.value));
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

    /// The whole reason this knob is path-addressed rather than flat: pro
    /// mode shares the `reasoning` object with the effort the caller asked
    /// for, and must not clobber it.
    #[test]
    fn applying_pro_mode_preserves_an_existing_effort() {
        let mut body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "reasoning": {"effort": "high"},
        });
        apply_request_knob(&mut body, "gpt-5.6-sol", Some("pro"));
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["mode"], "pro");
    }

    #[test]
    fn applying_pro_mode_creates_the_reasoning_object_when_absent() {
        let mut body = serde_json::json!({"model": "gpt-5.6-sol"});
        apply_request_knob(&mut body, "gpt-5.6-sol", Some("pro"));
        assert_eq!(body["reasoning"]["mode"], "pro");
    }

    /// Negative controls. Without these the assertions above would pass just
    /// as happily if the function inserted `mode` unconditionally.
    #[test]
    fn standard_and_absent_modes_write_nothing() {
        let baseline = serde_json::json!({"model": "gpt-5.6-sol"});

        let mut standard = baseline.clone();
        apply_request_knob(&mut standard, "gpt-5.6-sol", Some(STANDARD_MODE_ID));
        assert_eq!(standard, baseline, "`standard` must be a no-op");

        let mut none = baseline.clone();
        apply_request_knob(&mut none, "gpt-5.6-sol", None);
        assert_eq!(none, baseline, "no requested mode must be a no-op");

        // A model with no `reasoning_modes` declaration at all.
        let mut other = serde_json::json!({"model": "claude-opus-5"});
        let other_baseline = other.clone();
        apply_request_knob(&mut other, "claude-opus-5", Some("pro"));
        assert_eq!(
            other, other_baseline,
            "a model that declares no pro mode must be untouched"
        );
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
