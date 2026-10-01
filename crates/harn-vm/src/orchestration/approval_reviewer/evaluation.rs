//! Validated decision evidence carried by the existing approval seam.

use harn_parser::builtin_signatures::TyExt;
use serde::{Deserialize, Serialize};

use crate::value::VmValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDisposition {
    Approved,
    Denied,
    NeedsReview,
}

/// Host-authored wording, projected without adding presentation policy in each UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPresentation {
    pub summary: String,
    pub details: Vec<String>,
}

/// The outcome is validated against the evaluator's owning structural type,
/// rather than a second hand-maintained answer or refusal schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionReview {
    pub disposition: ReviewDisposition,
    pub rule: String,
    pub consequence: Option<String>,
    pub minimum_confidence: Option<serde_json::Number>,
    pub presentation: Option<ReviewPresentation>,
    outcome: serde_json::Value,
}

impl DecisionReview {
    pub fn parse(value: &VmValue) -> Result<Self, &'static str> {
        let map = value.as_dict().ok_or("invalid_evaluation_review")?;
        let outcome = map.get("outcome").ok_or("missing_evaluation_outcome")?;
        crate::typecheck::assert_value_matches_type(
            outcome,
            &harn_builtin_meta::predicate::EVALUATION_OUTCOME.to_type_expr(),
            "approval_reviewer",
            "evaluation_review.outcome",
            None,
        )
        .map_err(|_| "invalid_evaluation_outcome")?;
        let disposition = match super::string_field(map, "disposition").as_deref() {
            Some("approved") => ReviewDisposition::Approved,
            Some("denied") => ReviewDisposition::Denied,
            Some("needs_review") => ReviewDisposition::NeedsReview,
            _ => return Err("invalid_review_disposition"),
        };
        let outcome = crate::llm::helpers::vm_value_to_json(outcome);
        if disposition != ReviewDisposition::NeedsReview
            && outcome.get("kind").and_then(serde_json::Value::as_str) != Some("answered")
        {
            return Err("non_answer_cannot_settle_review");
        }
        let rule = super::string_field(map, "rule").ok_or("missing_review_rule")?;
        let minimum_confidence = match map.get("minimum_confidence") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::Float(value)) if value.is_finite() && (0.0..=1.0).contains(value) => {
                serde_json::Number::from_f64(*value)
            }
            _ => return Err("invalid_review_confidence_floor"),
        };
        let presentation = match map.get("presentation") {
            None | Some(VmValue::Nil) => None,
            Some(value) => Some(
                serde_json::from_value(crate::llm::helpers::vm_value_to_json(value))
                    .map_err(|_| "invalid_review_presentation")?,
            ),
        };
        Ok(Self {
            disposition,
            rule,
            consequence: super::string_field(map, "consequence"),
            minimum_confidence,
            presentation,
            outcome,
        })
    }

    pub fn outcome_kind(&self) -> &str {
        self.outcome["kind"]
            .as_str()
            .expect("validated evaluation outcome has a string kind")
    }
}
