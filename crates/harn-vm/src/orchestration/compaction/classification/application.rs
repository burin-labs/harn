//! Apply admitted decisions without a positional second pass.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    apply_confidence_floor, validate_decisions, ClassificationApplicationReason,
    ClassificationChoice, ClassificationDecision, ClassificationDecisionReceipt,
    ClassificationSource,
};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClassificationRewrite {
    pub index: usize,
    pub text: String,
}

/// All-or-nothing validation before application. A missing rewrite batch is
/// recoverable by keeping originals; a partial or invented batch is not a
/// successful rewrite. Protected indices never enter `eligible`.
pub(crate) fn apply_round(
    surviving: &mut BTreeMap<usize, String>,
    eligible: &BTreeSet<usize>,
    decisions: Vec<ClassificationDecision>,
    rewrites: Option<Vec<ClassificationRewrite>>,
    floor: f64,
    round: usize,
    source: ClassificationSource,
) -> Result<(Vec<ClassificationDecisionReceipt>, BTreeSet<usize>), String> {
    validate_decisions(eligible, &decisions)?;
    if !eligible.iter().all(|index| surviving.contains_key(index)) {
        return Err("classification named an item that no longer survives".into());
    }
    let mut receipts: Vec<_> = decisions
        .into_iter()
        .map(|decision| apply_confidence_floor(decision, floor, round, source))
        .collect();
    let needed: BTreeSet<_> = receipts
        .iter()
        .filter(|receipt| receipt.applied == ClassificationChoice::Reword)
        .map(|receipt| receipt.decision.index)
        .collect();
    let rewritten = rewrites
        .map(|rewrites| validate_rewrites(&needed, rewrites))
        .transpose()?;
    let mut next = BTreeSet::new();
    for receipt in &mut receipts {
        let index = receipt.decision.index;
        // A refusal to rewrite a low-confidence drop does not make the item
        // eligible for another attempt at dropping it in a later round.
        if receipt.decision.choice == ClassificationChoice::Keep {
            next.insert(index);
        }
        match receipt.applied {
            ClassificationChoice::Keep => {}
            ClassificationChoice::Drop => {
                surviving.remove(&index);
            }
            ClassificationChoice::Reword => {
                if let Some(rewritten) = &rewritten {
                    surviving.insert(index, rewritten[&index].clone());
                } else {
                    receipt.applied = ClassificationChoice::Keep;
                    receipt.application_reason =
                        ClassificationApplicationReason::RewriteUnavailable;
                }
            }
        }
    }
    Ok((receipts, next))
}

fn validate_rewrites(
    expected: &BTreeSet<usize>,
    rewrites: Vec<ClassificationRewrite>,
) -> Result<BTreeMap<usize, String>, String> {
    let mut accepted = BTreeMap::new();
    for rewrite in rewrites {
        if rewrite.text.trim().is_empty()
            || rewrite.text.contains(['\n', '\r'])
            || !expected.contains(&rewrite.index)
            || accepted.insert(rewrite.index, rewrite.text).is_some()
        {
            return Err(
                "rewrites require unique expected indices and nonempty single lines".into(),
            );
        }
    }
    if accepted.keys().copied().collect::<BTreeSet<_>>() != *expected {
        return Err("rewrite batch omitted a message index".into());
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decision::answer::ConfidenceKind;

    fn decision(index: usize, choice: ClassificationChoice) -> ClassificationDecision {
        ClassificationDecision {
            index,
            question_id: format!("message_{index}"),
            choice,
            confidence: 0.2,
            confidence_kind: ConfidenceKind::ModelRationale,
            evaluation_receipt: "typed fixture".into(),
        }
    }

    #[test]
    fn floor_preserves_every_item_and_floor_off_really_drops_every_item() {
        let original = BTreeMap::from([(0, "evidence".into()), (1, "constraint".into())]);
        let eligible = BTreeSet::from([0, 1]);
        let decisions = vec![
            decision(0, ClassificationChoice::Drop),
            decision(1, ClassificationChoice::Drop),
        ];
        let mut guarded = original.clone();
        let rewrites = vec![
            ClassificationRewrite {
                index: 0,
                text: "short evidence".into(),
            },
            ClassificationRewrite {
                index: 1,
                text: "short constraint".into(),
            },
        ];
        let (receipts, next) = apply_round(
            &mut guarded,
            &eligible,
            decisions.clone(),
            Some(rewrites),
            0.8,
            1,
            ClassificationSource::Fixture,
        )
        .unwrap();
        assert_eq!(guarded.len(), 2);
        assert!(receipts
            .iter()
            .all(|receipt| receipt.applied == ClassificationChoice::Reword));
        assert!(next.is_empty());
        let mut unguarded = original;
        let (receipts, _) = apply_round(
            &mut unguarded,
            &eligible,
            decisions,
            None,
            0.0,
            1,
            ClassificationSource::Fixture,
        )
        .unwrap();
        assert!(
            unguarded.is_empty(),
            "the negative control must reach actual removal"
        );
        assert!(receipts
            .iter()
            .all(|receipt| receipt.applied == ClassificationChoice::Drop));
    }

    #[test]
    fn failed_rewrite_preserves_original_and_only_raw_keep_can_be_reasked() {
        let original = BTreeMap::from([
            (0, "protected".into()),
            (1, "evidence".into()),
            (2, "detail".into()),
            (3, "current".into()),
        ]);
        let eligible = BTreeSet::from([1, 2, 3]);
        let mut surviving = original.clone();
        let (receipts, next) = apply_round(
            &mut surviving,
            &eligible,
            vec![
                decision(1, ClassificationChoice::Drop),
                decision(2, ClassificationChoice::Reword),
                decision(3, ClassificationChoice::Keep),
            ],
            None,
            0.8,
            1,
            ClassificationSource::Fixture,
        )
        .unwrap();
        assert_eq!(surviving, original);
        assert_eq!(next, BTreeSet::from([3]));
        assert!(receipts
            .iter()
            .all(|receipt| receipt.applied == ClassificationChoice::Keep));
        assert_eq!(
            receipts[0].application_reason,
            ClassificationApplicationReason::RewriteUnavailable
        );
    }

    #[test]
    fn partial_rewrite_cannot_partially_mutate_the_window() {
        let original = BTreeMap::from([(0, "evidence".into()), (1, "constraint".into())]);
        let mut surviving = original.clone();
        assert!(apply_round(
            &mut surviving,
            &BTreeSet::from([0, 1]),
            vec![
                decision(0, ClassificationChoice::Drop),
                decision(1, ClassificationChoice::Drop),
            ],
            Some(vec![ClassificationRewrite {
                index: 0,
                text: "short evidence".into()
            }]),
            0.8,
            1,
            ClassificationSource::Fixture
        )
        .is_err());
        assert_eq!(surviving, original);
    }
}
