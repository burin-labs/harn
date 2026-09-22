//! The classified compaction caller of the one Harn evaluator owner.

use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::{ClassificationConfig, ClassificationDecision};

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ClassificationItem {
    pub index: usize,
    pub role: String,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClassificationRoundKind {
    Answered,
    Unavailable,
    WindowRefused,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClassificationRound {
    pub kind: ClassificationRoundKind,
    pub decisions: Vec<ClassificationDecision>,
    pub reason: String,
}

pub(crate) async fn evaluate_round(
    ctx: &AsyncBuiltinCtx,
    config: &ClassificationConfig,
    items: &[ClassificationItem],
    anchor: &str,
    round: usize,
) -> Result<ClassificationRound, VmError> {
    let harness = ctx.child_vm().root_harness_value().ok_or_else(|| {
        VmError::Runtime("classified compaction requires root Harness authority".into())
    })?;
    let fields = harness.as_dict().ok_or_else(|| {
        VmError::Runtime("classified compaction received invalid Harness authority".into())
    })?;
    let mut authority = crate::value::DictMap::new();
    for name in ["fs", "llm"] {
        let value = fields.get(name).ok_or_else(|| {
            VmError::Runtime(format!("classified compaction requires Harness.{name}"))
        })?;
        authority.insert(crate::value::intern_key(name), value.clone());
    }
    let payload = serde_json::json!({
        "items": items,
        "anchor": anchor,
        "round": round,
        "budget_tokens": config.window_tokens,
        // With no route count ceiling, the actual item count is a bound, not
        // an invented model limit. An empty round is never dispatched.
        "max_questions": config.max_questions.unwrap_or(items.len()).max(1),
    });
    let payload = crate::stdlib::json_to_vm_value(&payload);
    let mut request = payload.as_dict().cloned().ok_or_else(|| {
        VmError::Runtime("classified compaction could not encode its request".into())
    })?;
    request.insert(crate::value::intern_key("policy"), config.policy.clone());
    if let Some(fixture) = &config.fixture {
        request.insert(crate::value::intern_key("fixture"), fixture.clone());
    }
    let result = crate::stdlib::harn_entry::call_harn_export_by_name(
        ctx,
        "std/agent/compaction_classify",
        "classify_compaction_round",
        "classified compaction",
        &[VmValue::dict(authority), VmValue::dict(request)],
    )
    .await?;
    serde_json::from_value(crate::llm::vm_value_to_json(&result)).map_err(|error| {
        VmError::Runtime(format!(
            "classified compaction returned an invalid round: {error}"
        ))
    })
}
