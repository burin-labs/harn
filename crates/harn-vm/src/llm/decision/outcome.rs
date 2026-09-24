//! The closed outcome, built in one place.
//!
//! Every arm is constructed here so the batched outcome and its single-boolean
//! projection cannot drift. A refusal is a value the caller matches on, never
//! an error to catch and never a default verdict.

use crate::value::VmValue;

use super::answer::{Answer, AnswerBody};

/// A built outcome and the receipt it names.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub kind: &'static str,
    value: VmValue,
}

impl Outcome {
    pub fn into_value(self) -> VmValue {
        self.value
    }
}

fn arm(kind: &'static str, receipt: &str, fields: Vec<(&str, VmValue)>) -> Outcome {
    let mut entries = vec![("kind", VmValue::String(kind.into()))];
    entries.extend(fields);
    entries.push(("receipt", VmValue::String(receipt.into())));
    Outcome {
        kind,
        value: VmValue::dict(entries),
    }
}

fn answer_map(answers: &[Answer]) -> VmValue {
    VmValue::dict(
        answers
            .iter()
            .map(|answer| (answer.question_id.as_str(), answer.to_vm_value()))
            .collect::<Vec<_>>(),
    )
}

/// Every declared question answered at or above the threshold.
pub fn answered(receipt: &str, answers: &[Answer]) -> Outcome {
    arm("answered", receipt, vec![("value", answer_map(answers))])
}

/// Answers returned, but at least one below the threshold. The candidates are
/// all here: an uncertain batch is still a measurement, and naming which
/// questions fell short is what lets a caller re-ask only those.
pub fn low_confidence(receipt: &str, answers: &[Answer], threshold: f64) -> Outcome {
    let under: Vec<VmValue> = answers
        .iter()
        .filter(|answer| !answer.meets(threshold))
        .map(|answer| VmValue::String(answer.question_id.as_str().into()))
        .collect();
    arm(
        "low_confidence",
        receipt,
        vec![
            ("candidates", answer_map(answers)),
            ("threshold", VmValue::Float(threshold)),
            ("question_ids", VmValue::List(under.into())),
        ],
    )
}

pub fn refused(receipt: &str, reason: &str, diagnostic: &str) -> Outcome {
    arm(
        "refused",
        receipt,
        vec![
            ("reason", VmValue::String(reason.into())),
            ("diagnostic", VmValue::String(diagnostic.into())),
        ],
    )
}

pub fn unavailable(receipt: &str, reason: &str) -> Outcome {
    arm(
        "unavailable",
        receipt,
        vec![("reason", VmValue::String(reason.into()))],
    )
}

pub fn budget_cut(receipt: &str, limit: &str, requested: f64, remaining: f64) -> Outcome {
    arm(
        "budget_cut",
        receipt,
        vec![
            ("limit", VmValue::String(limit.into())),
            ("requested", VmValue::Float(requested)),
            ("remaining", VmValue::Float(remaining)),
        ],
    )
}

pub fn cancelled(receipt: &str, control_event: &str) -> Outcome {
    arm(
        "cancelled",
        receipt,
        vec![("control_event", VmValue::String(control_event.into()))],
    )
}

/// The state did not fit. Both the local estimate and the declared limit are
/// on the outcome, so a caller can window by construction rather than guess.
pub fn state_too_large(receipt: &str, limit_tokens: usize, estimated_tokens: usize) -> Outcome {
    arm(
        "state_too_large",
        receipt,
        vec![
            ("limit_tokens", VmValue::Int(limit_tokens as i64)),
            ("estimated_tokens", VmValue::Int(estimated_tokens as i64)),
        ],
    )
}

pub fn question_invalid(receipt: &str, question: &str, reason: &str) -> Outcome {
    arm(
        "question_invalid",
        receipt,
        vec![
            ("question", VmValue::String(question.into())),
            ("reason", VmValue::String(reason.into())),
        ],
    )
}

/// Provider 429. `retry_after_ms` is absent when the provider named no delay;
/// absence is not zero, and nothing retries on the caller's behalf.
pub fn rate_limited(receipt: &str, retry_after_ms: Option<u64>) -> Outcome {
    let mut fields = Vec::new();
    if let Some(retry_after_ms) = retry_after_ms {
        fields.push(("retry_after_ms", VmValue::Int(retry_after_ms as i64)));
    }
    arm("rate_limited", receipt, fields)
}

pub fn overloaded(receipt: &str) -> Outcome {
    arm("overloaded", receipt, Vec::new())
}

/// Project a batched outcome onto the single-boolean contract.
///
/// Only the two accepting arms differ between the unions, so every refusal
/// passes through untouched and the two entry points cannot refuse
/// differently. A boolean site declares exactly one boolean question, so an
/// accepted batch has exactly one boolean answer.
pub fn project_to_predicate(outcome: Outcome, answers: &[Answer], threshold: f64) -> Outcome {
    let verdict = |answer: &Answer| match &answer.body {
        AnswerBody::Boolean { verdict, .. } => Some(VmValue::dict(vec![
            ("verdict", VmValue::Bool(*verdict)),
            ("confidence", VmValue::Float(answer.confidence)),
            ("evidence", VmValue::String(answer.evidence.as_str().into())),
        ])),
        _ => None,
    };
    let receipt = receipt_of(&outcome);
    match outcome.kind {
        "answered" => match answers.first().and_then(verdict) {
            Some(value) => arm("verdict", &receipt, vec![("value", value)]),
            // A boolean site whose one answer is not boolean means the
            // evaluator and the site disagree about the question. That is a
            // schema failure, not a verdict of false.
            None => refused(
                &receipt,
                "schema_invalid",
                "predicate site did not receive a boolean answer",
            ),
        },
        "low_confidence" => match answers.first().and_then(verdict) {
            Some(value) => arm(
                "low_confidence",
                &receipt,
                vec![
                    ("candidate", value),
                    ("threshold", VmValue::Float(threshold)),
                ],
            ),
            None => refused(
                &receipt,
                "schema_invalid",
                "predicate site did not receive a boolean answer",
            ),
        },
        _ => outcome,
    }
}

fn receipt_of(outcome: &Outcome) -> String {
    outcome
        .value
        .as_dict()
        .and_then(|fields| fields.get("receipt"))
        .map(|receipt| receipt.as_str_cow().into_owned())
        .unwrap_or_default()
}
