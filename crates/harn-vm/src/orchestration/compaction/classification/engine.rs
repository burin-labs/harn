use std::collections::{BTreeMap, BTreeSet};

use crate::value::VmError;
use crate::vm::AsyncBuiltinCtx;

use super::super::{CompactionPolicy, CompactionSummary, RecapMetrics};
use super::{
    apply_confidence_floor, apply_round, evaluate_round, rewrite_items, ClassificationChoice,
    ClassificationConfig, ClassificationItem, ClassificationReceipt, ClassificationRoundKind,
    ClassificationSource, ClassificationStatus,
};

pub(crate) struct ClassificationInputs<'a> {
    pub ctx: &'a AsyncBuiltinCtx,
    pub config: &'a ClassificationConfig,
    pub archived: &'a [serde_json::Value],
    pub retained: &'a [serde_json::Value],
    pub first_index: usize,
    pub budget_bytes: usize,
    pub active: Option<&'a crate::llm::api::LlmCallOptions>,
    pub summarize_prompt: Option<&'a str>,
    pub policy: &'a CompactionPolicy,
}

pub(crate) struct ClassifiedWindow {
    pub summary: CompactionSummary,
    pub receipt: Box<ClassificationReceipt>,
    pub fallback_recap: Option<RecapMetrics>,
}

pub(crate) async fn classify_window(
    input: ClassificationInputs<'_>,
) -> Result<ClassifiedWindow, VmError> {
    let mut protected: BTreeSet<_> =
        super::super::latest_pinned_indices(input.archived.iter(), |message| {
            message.get("content").and_then(serde_json::Value::as_str)
        })
        .into_iter()
        .map(|index| input.first_index + index)
        .collect();
    if let Some(index) = input.archived.iter().rposition(|message| {
        message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .is_some_and(super::super::is_prior_recap)
    }) {
        protected.insert(input.first_index + index);
    }
    let source = if input.config.fixture.is_some() {
        ClassificationSource::Fixture
    } else {
        ClassificationSource::Evaluation
    };
    let mut surviving: BTreeMap<_, _> = input
        .archived
        .iter()
        .enumerate()
        .map(|(index, message)| (input.first_index + index, message_body(message)))
        .collect();
    let roles: BTreeMap<_, _> = input
        .archived
        .iter()
        .enumerate()
        .map(|(index, message)| {
            (
                input.first_index + index,
                message
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            )
        })
        .collect();
    let anchor = input
        .retained
        .iter()
        .rev()
        .chain(input.archived.iter().rev())
        .find(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
        .map(message_body)
        .unwrap_or_default();
    let mut eligible: BTreeSet<_> = surviving
        .keys()
        .copied()
        .filter(|index| !protected.contains(index))
        .collect();
    let mut receipt = ClassificationReceipt {
        status: ClassificationStatus::Applied,
        confidence_floor: input.config.confidence_floor,
        rounds: 0,
        budget_bytes: input.budget_bytes,
        result_bytes: 0,
        budget_met: false,
        decisions: Vec::new(),
        fallback_reason: None,
    };
    for round in 1..=input.config.max_rounds {
        if eligible.is_empty() {
            break;
        }
        let items: Vec<_> = eligible
            .iter()
            .map(|index| ClassificationItem {
                index: *index,
                role: roles[index].clone(),
                content: surviving[index].clone(),
            })
            .collect();
        let result = evaluate_round(input.ctx, input.config, &items, &anchor, round).await?;
        if result.kind == ClassificationRoundKind::WindowRefused && round == 1 {
            // Only the explicit, all-windows predispatch refusal permits the
            // existing positional fallback. Later failures never override an
            // observed confidence decision.
            let (summary, recap) = super::super::observation_mask_compaction_with_callback(
                input.archived,
                input.archived.len(),
                None,
                input.budget_bytes,
            );
            receipt.status = ClassificationStatus::Fallback;
            receipt.fallback_reason = Some(result.reason);
            receipt.result_bytes = summary.text.len();
            receipt.budget_met = receipt.result_bytes <= input.budget_bytes;
            return Ok(ClassifiedWindow {
                summary,
                receipt: Box::new(receipt),
                fallback_recap: Some(recap),
            });
        }
        if result.kind != ClassificationRoundKind::Answered {
            return Err(VmError::Runtime(format!(
                "classified compaction unavailable: {}",
                result.reason
            )));
        }
        // Validate coverage before spending on rewrites or applying anything.
        super::validate_decisions(&eligible, &result.decisions).map_err(VmError::Runtime)?;
        let rewrite_indices: BTreeSet<_> = result
            .decisions
            .iter()
            .filter(|decision| {
                apply_confidence_floor(
                    (*decision).clone(),
                    input.config.confidence_floor,
                    round,
                    source,
                )
                .applied
                    == ClassificationChoice::Reword
            })
            .map(|decision| decision.index)
            .collect();
        let rewrites = rewrite_items(
            input.ctx,
            input.config,
            input.active,
            &items
                .into_iter()
                .filter(|item| rewrite_indices.contains(&item.index))
                .collect::<Vec<_>>(),
        )
        .await?;
        let (decisions, next) = apply_round(
            &mut surviving,
            &eligible,
            result.decisions,
            rewrites,
            input.config.confidence_floor,
            round,
            source,
        )
        .map_err(VmError::Runtime)?;
        receipt.rounds = round;
        receipt.decisions.extend(decisions);
        eligible = next;
        if render(&surviving, &roles).text.len() <= input.budget_bytes {
            break;
        }
    }
    let mut summary = render(&surviving, &roles);
    if let Some(prompt) = input.summarize_prompt {
        let summary_input: Vec<_> = surviving
            .iter()
            .filter(|(index, _)| !protected.contains(index))
            .map(|(index, body)| serde_json::json!({"role": roles[index], "content": body}))
            .collect();
        if !summary_input.is_empty() {
            let generated = super::super::llm_compaction_summary(
                &summary_input,
                input.retained,
                summary_input.len(),
                input.active.ok_or_else(|| {
                    VmError::Runtime(
                        "classified compaction summary requires active chat options".into(),
                    )
                })?,
                Some(prompt),
                input.policy,
            )
            .await?;
            let pinned: BTreeMap<_, _> = surviving
                .into_iter()
                .filter(|(index, _)| protected.contains(index))
                .collect();
            let kept = render(&pinned, &roles);
            let scaffold = generated.scaffold_bytes + kept.scaffold_bytes + 1;
            summary =
                CompactionSummary::new(format!("{}\n{}", kept.text, generated.text), scaffold);
        }
    }
    receipt.result_bytes = summary.text.len();
    receipt.budget_met = receipt.result_bytes <= input.budget_bytes;
    Ok(ClassifiedWindow {
        summary,
        receipt: Box::new(receipt),
        fallback_recap: None,
    })
}

fn message_body(message: &serde_json::Value) -> String {
    let mut body = match message.get("content") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    };
    if let Some(fields) = message.as_object() {
        let metadata: serde_json::Map<_, _> = fields
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "role" | "content"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !metadata.is_empty() {
            body.push('\n');
            body.push_str(&serde_json::Value::Object(metadata).to_string());
        }
    }
    body
}

fn render(
    surviving: &BTreeMap<usize, String>,
    roles: &BTreeMap<usize, String>,
) -> CompactionSummary {
    let mut text = format!(
        "[auto-compacted {}",
        super::super::CLASSIFIED_RECAP_HEADER_SENTINEL
    );
    let mut carried = 0;
    for (index, body) in surviving {
        text.push_str(&format!("\n[message {index} {}]\n", roles[index]));
        text.push_str(body);
        carried += body.len();
    }
    let scaffold = text.len() - carried;
    CompactionSummary::new(text, scaffold)
}
