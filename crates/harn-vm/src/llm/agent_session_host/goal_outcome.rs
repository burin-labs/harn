//! Project the completion owner's assessment after terminal admission is final.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::value::VmError;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Accept,
    Continue,
    StopUnverified,
    StopVerified,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RowStatus {
    Met,
    Unmet,
    Missing,
    Unsupported,
}

#[derive(Deserialize)]
struct Row {
    status: RowStatus,
}

#[derive(Deserialize)]
struct Requirements {
    complete: bool,
    required_count: usize,
    pending_count: usize,
    pending_names: Vec<String>,
    rows: Vec<Row>,
}

/// Only the existing receipt's projection fields cross this boundary.
#[derive(Deserialize)]
pub(super) struct Assessment {
    action: Action,
    evidence_id: String,
    requirements: Option<Requirements>,
    pending_tool_batch_effect_count: usize,
    gap_count: usize,
}

impl Assessment {
    pub(super) fn read(value: Option<Value>) -> Result<Option<Self>, VmError> {
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| VmError::Runtime(format!("invalid completion assessment: {error}")))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Achieved,
    PartiallyAchieved,
    NotAchieved,
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    Unassessed,
    NoDeclaredRequirements,
    RequirementsMet,
    RequirementsUnmet,
    RequirementsUnassessed,
    PendingEffects,
    CompletionNotAccepted,
}

#[derive(Serialize)]
struct Summary {
    required_count: usize,
    met_count: usize,
    unmet_count: usize,
    missing_count: usize,
    unsupported_count: usize,
    pending_count: usize,
    pending_names: Vec<String>,
}

#[derive(Serialize)]
struct Outcome {
    kind: Kind,
    reason: Reason,
    evidence_id: Option<String>,
    requirements: Option<Summary>,
}

pub(super) fn project(assessment: Option<Assessment>, publication_admitted: bool) -> Value {
    let mut outcome = Outcome {
        kind: Kind::Unknown,
        reason: Reason::Unassessed,
        evidence_id: None,
        requirements: None,
    };
    if let Some(assessment) = assessment {
        outcome.evidence_id = Some(assessment.evidence_id);
        if let Some(report) = assessment.requirements {
            let mut summary = Summary {
                required_count: report.required_count,
                met_count: 0,
                unmet_count: 0,
                missing_count: 0,
                unsupported_count: 0,
                pending_count: report.pending_count,
                pending_names: report.pending_names,
            };
            for row in report.rows {
                match row.status {
                    RowStatus::Met => summary.met_count += 1,
                    RowStatus::Unmet => summary.unmet_count += 1,
                    RowStatus::Missing => summary.missing_count += 1,
                    RowStatus::Unsupported => summary.unsupported_count += 1,
                }
            }
            outcome.reason = Reason::RequirementsUnassessed;
            if summary.required_count == 0 {
                outcome.reason = Reason::NoDeclaredRequirements;
            } else if assessment.pending_tool_batch_effect_count > 0 {
                outcome.reason = Reason::PendingEffects;
            } else if summary.missing_count == 0 && summary.unsupported_count == 0 {
                if summary.unmet_count > 0 {
                    outcome.kind = if summary.met_count > 0 {
                        Kind::PartiallyAchieved
                    } else {
                        Kind::NotAchieved
                    };
                    outcome.reason = Reason::RequirementsUnmet;
                } else if report.complete
                    && summary.pending_count == 0
                    && summary.met_count == summary.required_count
                {
                    if publication_admitted
                        && matches!(assessment.action, Action::Accept | Action::StopVerified)
                        && assessment.gap_count == 0
                    {
                        outcome.kind = Kind::Achieved;
                        outcome.reason = Reason::RequirementsMet;
                    } else {
                        outcome.reason = Reason::CompletionNotAccepted;
                    }
                }
            }
            outcome.requirements = Some(summary);
        }
    }
    serde_json::to_value(outcome).expect("goal outcome contains only JSON-safe fields")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Direct native finalization can receive a truncated report, unlike the
    // canonical loop's normalized requirement owner. Absence cannot pass here.
    #[test]
    fn truncated_report_cannot_establish_goal_success() {
        let mut receipt = serde_json::json!({
            "action": "accept", "evidence_id": "assessed-state",
            "pending_tool_batch_effect_count": 0, "gap_count": 0,
            "requirements": {
                "complete": true, "required_count": 1, "pending_count": 0,
                "pending_names": [], "rows": [{"status": "met"}]
            }
        });
        let measured = Assessment::read(Some(receipt.clone())).unwrap();
        assert_eq!(project(measured, true)["kind"], "achieved");
        receipt["requirements"]["rows"] = serde_json::json!([]);
        let truncated = Assessment::read(Some(receipt)).unwrap();
        let outcome = project(truncated, true);
        assert_eq!(outcome["kind"], "unknown");
        assert_eq!(outcome["requirements"]["required_count"], 1);
        assert_eq!(outcome["requirements"]["met_count"], 0);
    }
}
