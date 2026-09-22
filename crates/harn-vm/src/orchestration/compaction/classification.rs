//! Typed classification decisions and their deterministic application policy.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::value::{VmError, VmValue};

#[derive(Clone, Debug)]
pub struct ClassificationConfig {
    pub policy: VmValue,
    /// The evaluator policy threshold is the one configured confidence floor.
    pub confidence_floor: f64,
    pub max_rounds: usize,
    pub window_tokens: usize,
    pub max_questions: Option<usize>,
    pub fixture: Option<VmValue>,
    pub rewrite_fixture: Option<VmValue>,
}

impl ClassificationConfig {
    pub(crate) fn from_value(value: &VmValue) -> Result<Self, VmError> {
        let fields = value
            .as_dict()
            .ok_or_else(|| invalid("classify must be a record"))?;
        const KEYS: &[&str] = &[
            "policy",
            "max_rounds",
            "window_tokens",
            "fixture",
            "rewrite_fixture",
        ];
        if let Some(key) = fields.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(invalid(&format!("unknown classify option {key}")));
        }
        let policy = fields
            .get("policy")
            .ok_or_else(|| invalid("classify requires policy"))?;
        let normalized = crate::llm::decision::EvaluationPolicy::from_value(policy)
            .map_err(|message| invalid(&message))?;
        let contract = crate::llm::decision::contract::decision_contract_for_route(
            &normalized.provider,
            &normalized.model,
        )
        .ok_or_else(|| invalid("classify policy route has no decision contract"))?;
        let window_tokens = positive_integer(fields.get("window_tokens"), "window_tokens")?
            .unwrap_or(contract.limits.state_window_tokens)
            .min(contract.limits.state_window_tokens);
        if window_tokens == 0 {
            return Err(invalid("classify route has no usable state window"));
        }
        Ok(Self {
            policy: policy.clone(),
            confidence_floor: normalized.threshold,
            max_rounds: positive_integer(fields.get("max_rounds"), "max_rounds")?.unwrap_or(2),
            window_tokens,
            max_questions: contract.limits.max_questions,
            fixture: fixture(fields.get("fixture"), "fixture")?,
            rewrite_fixture: fixture(fields.get("rewrite_fixture"), "rewrite_fixture")?,
        })
    }
}

fn positive_integer(value: Option<&VmValue>, name: &str) -> Result<Option<usize>, VmError> {
    value
        .map(|value| {
            value
                .as_int()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid(&format!("classify {name} must be a positive integer")))
        })
        .transpose()
}

fn fixture(value: Option<&VmValue>, name: &str) -> Result<Option<VmValue>, VmError> {
    value
        .map(|value| {
            if matches!(value, VmValue::Closure(_)) {
                Ok(value.clone())
            } else {
                Err(invalid(&format!(
                    "classify {name} must be a typed fixture closure"
                )))
            }
        })
        .transpose()
}

fn invalid(message: &str) -> VmError {
    VmError::Runtime(format!("classified compaction: {message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationChoice {
    Keep,
    Reword,
    Drop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationDecision {
    pub index: usize,
    pub question_id: String,
    pub choice: ClassificationChoice,
    pub confidence: f64,
    pub confidence_kind: crate::llm::decision::answer::ConfidenceKind,
    pub evaluation_receipt: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationApplicationReason {
    Classified,
    ConfidenceFloor,
    RewriteUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationSource {
    Evaluation,
    Fixture,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassificationDecisionReceipt {
    #[serde(flatten)]
    pub decision: ClassificationDecision,
    pub round: usize,
    pub source: ClassificationSource,
    pub applied: ClassificationChoice,
    pub application_reason: ClassificationApplicationReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    Applied,
    Fallback,
}

/// Budget failure is observable; it is never permission for a later
/// positional truncation to undo the recorded confidence floor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassificationReceipt {
    pub status: ClassificationStatus,
    pub confidence_floor: f64,
    pub rounds: usize,
    pub budget_bytes: usize,
    pub result_bytes: usize,
    pub budget_met: bool,
    pub decisions: Vec<ClassificationDecisionReceipt>,
    pub fallback_reason: Option<String>,
}

/// A low-confidence decision moves one step toward preservation. In
/// particular, a low-confidence drop requests a rewrite; it never authorizes
/// discarding the message merely because a byte target remains unmet.
pub(super) fn apply_confidence_floor(
    decision: ClassificationDecision,
    floor: f64,
    round: usize,
    source: ClassificationSource,
) -> ClassificationDecisionReceipt {
    let applied = if decision.confidence < floor {
        match decision.choice {
            ClassificationChoice::Drop => ClassificationChoice::Reword,
            ClassificationChoice::Reword | ClassificationChoice::Keep => ClassificationChoice::Keep,
        }
    } else {
        decision.choice
    };
    let application_reason = if applied == decision.choice {
        ClassificationApplicationReason::Classified
    } else {
        ClassificationApplicationReason::ConfidenceFloor
    };
    ClassificationDecisionReceipt {
        decision,
        round,
        source,
        applied,
        application_reason,
    }
}

/// Both explicit fixtures and model answers cross this boundary before any
/// transcript edit. Missing, repeated, or invented indices cannot become a
/// partial successful classification.
pub(super) fn validate_decisions(
    expected_indices: &BTreeSet<usize>,
    decisions: &[ClassificationDecision],
) -> Result<(), String> {
    let mut seen_indices = BTreeSet::new();
    let mut seen_questions = BTreeSet::new();
    for decision in decisions {
        if !decision.confidence.is_finite() || !(0.0..=1.0).contains(&decision.confidence) {
            return Err("classification confidence must be finite and between zero and one".into());
        }
        if !expected_indices.contains(&decision.index) || !seen_indices.insert(decision.index) {
            return Err("classification contains an extra or repeated message index".into());
        }
        if decision.question_id.is_empty() || !seen_questions.insert(&decision.question_id) {
            return Err("classification question identifiers must be nonempty and unique".into());
        }
    }
    if seen_indices != *expected_indices {
        return Err("classification omitted a message index".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decision::answer::ConfidenceKind;

    fn decision(
        index: usize,
        choice: ClassificationChoice,
        confidence: f64,
    ) -> ClassificationDecision {
        ClassificationDecision {
            index,
            question_id: format!("message_{index}"),
            choice,
            confidence,
            confidence_kind: ConfidenceKind::ModelRationale,
            evaluation_receipt: "explicit typed fixture".into(),
        }
    }

    #[test]
    fn low_confidence_drop_never_authorizes_discarding() {
        let raw = decision(1, ClassificationChoice::Drop, 0.2);
        let guarded = apply_confidence_floor(raw.clone(), 0.8, 1, ClassificationSource::Fixture);
        assert_eq!(guarded.applied, ClassificationChoice::Reword);
        assert_eq!(
            guarded.application_reason,
            ClassificationApplicationReason::ConfidenceFloor
        );
        let control = apply_confidence_floor(raw, 0.0, 1, ClassificationSource::Fixture);
        assert_eq!(control.applied, ClassificationChoice::Drop);
        let uncertain_reword = apply_confidence_floor(
            decision(1, ClassificationChoice::Reword, 0.2),
            0.8,
            1,
            ClassificationSource::Fixture,
        );
        assert_eq!(uncertain_reword.applied, ClassificationChoice::Keep);
    }

    #[test]
    fn classification_requires_exact_coverage_and_valid_confidence() {
        let expected = BTreeSet::from([1, 2]);
        let first = decision(1, ClassificationChoice::Keep, 0.9);
        let second = decision(2, ClassificationChoice::Reword, 0.8);
        assert!(validate_decisions(&expected, &[first.clone(), second]).is_ok());
        assert!(validate_decisions(&expected, &[]).is_err());
        assert!(validate_decisions(&expected, &[first.clone(), first.clone()]).is_err());
        assert!(validate_decisions(
            &expected,
            &[first.clone(), decision(3, ClassificationChoice::Drop, 0.9)]
        )
        .is_err());
        for invalid in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(validate_decisions(
                &expected,
                &[
                    first.clone(),
                    decision(2, ClassificationChoice::Drop, invalid)
                ]
            )
            .is_err());
        }
    }
}
