//! Advisory replay audit for predicate hash drift.
//!
//! A shipped slice pins the predicate hashes that evaluated it. Current
//! `@retroactive` predicates are advisory-only: if a historical slice does not
//! carry the current hash, the audit reports drift but does not rewrite or
//! block the slice.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::predicates::ResolvedPredicate;
#[cfg(test)]
use super::predicates::{DiscoveredPredicate, PredicateSource};
use super::slice::{PredicateHash, Slice, SliceId};

/// Current predicate metadata included in replay-audit reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayAuditPredicate {
    pub name: String,
    pub hash: PredicateHash,
}

/// Per-slice replay-audit outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceReplayAudit {
    pub slice_id: SliceId,
    pub recorded_predicates: usize,
    pub current_retroactive_predicates: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisory_drift: Vec<ReplayAuditPredicate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub historical_only_predicates: Vec<PredicateHash>,
}

impl SliceReplayAudit {
    pub fn has_drift(&self) -> bool {
        !self.advisory_drift.is_empty()
    }
}

/// Aggregate replay-audit report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayAuditReport {
    pub audited_slices: usize,
    pub drifted_slices: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<SliceReplayAudit>,
}

impl ReplayAuditReport {
    pub fn has_drift(&self) -> bool {
        self.drifted_slices > 0
    }
}

/// Audit one shipped slice against the current predicate set.
pub fn audit_slice_against_current_predicates(
    slice: &Slice,
    current_predicates: &[ResolvedPredicate],
) -> SliceReplayAudit {
    let recorded = slice
        .invariants_applied
        .iter()
        .map(|(hash, _)| hash.clone())
        .collect::<BTreeSet<_>>();
    let current = current_predicates
        .iter()
        .map(|resolved| resolved.predicate.source_hash.clone())
        .collect::<BTreeSet<_>>();
    let advisory_drift = current_predicates
        .iter()
        .filter(|resolved| resolved.predicate.retroactive)
        .filter(|resolved| !recorded.contains(&resolved.predicate.source_hash))
        .map(|resolved| ReplayAuditPredicate {
            name: resolved.qualified_name.clone(),
            hash: resolved.predicate.source_hash.clone(),
        })
        .collect::<Vec<_>>();
    let historical_only_predicates = recorded
        .iter()
        .filter(|hash| !current.contains(*hash))
        .cloned()
        .collect::<Vec<_>>();

    SliceReplayAudit {
        slice_id: slice.id,
        recorded_predicates: recorded.len(),
        current_retroactive_predicates: current_predicates
            .iter()
            .filter(|resolved| resolved.predicate.retroactive)
            .count(),
        advisory_drift,
        historical_only_predicates,
    }
}

/// Audit many shipped slices and retain only slices with reportable content.
pub fn replay_audit_report(
    slices: impl IntoIterator<Item = Slice>,
    current_predicates: &[ResolvedPredicate],
) -> ReplayAuditReport {
    let mut report = ReplayAuditReport::default();
    for slice in slices {
        report.audited_slices += 1;
        let audit = audit_slice_against_current_predicates(&slice, current_predicates);
        if audit.has_drift() {
            report.drifted_slices += 1;
        }
        if audit.has_drift() || !audit.historical_only_predicates.is_empty() {
            report.slices.push(audit);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Approval, AtomId, InvariantResult, PredicateKind, SliceStatus, TestId};
    use harn_lexer::Span;

    fn slice(applied: Vec<PredicateHash>) -> Slice {
        Slice {
            id: SliceId([1; 32]),
            atoms: vec![AtomId([2; 32])],
            intents: Vec::new(),
            invariants_applied: applied
                .into_iter()
                .map(|hash| (hash, InvariantResult::allow()))
                .collect(),
            required_tests: vec![TestId::new("unit")],
            approval_chain: Vec::<Approval>::new(),
            base_ref: AtomId([0; 32]),
            status: SliceStatus::Ready,
        }
    }

    fn predicate(name: &str, hash: &str, retroactive: bool) -> ResolvedPredicate {
        ResolvedPredicate {
            qualified_name: name.to_string(),
            logical_name: name.to_string(),
            source: PredicateSource::new("."),
            source_order: 0,
            predicate: DiscoveredPredicate {
                name: name.to_string(),
                kind: PredicateKind::Deterministic,
                archivist: None,
                retroactive,
                source_hash: PredicateHash::new(hash),
                span: Span::dummy(),
            },
        }
    }

    #[test]
    fn current_retroactive_predicate_missing_from_slice_reports_advisory_drift() {
        let report = replay_audit_report(
            vec![slice(vec![PredicateHash::new("sha256:old")])],
            &[predicate("no_secrets", "sha256:new", true)],
        );

        assert!(report.has_drift());
        assert_eq!(report.audited_slices, 1);
        assert_eq!(report.drifted_slices, 1);
        assert_eq!(report.slices[0].advisory_drift[0].name, "no_secrets");
        assert_eq!(
            report.slices[0].historical_only_predicates,
            vec![PredicateHash::new("sha256:old")]
        );
    }

    #[test]
    fn non_retroactive_predicate_changes_do_not_surface_advisory_drift() {
        let report = replay_audit_report(
            vec![slice(vec![PredicateHash::new("sha256:old")])],
            &[predicate("style", "sha256:new", false)],
        );

        assert!(!report.has_drift());
        assert_eq!(report.drifted_slices, 0);
        assert!(report.slices[0].advisory_drift.is_empty());
    }
}
