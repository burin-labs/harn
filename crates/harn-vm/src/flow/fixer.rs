//! Fixer persona helpers for materializing Flow remediation suggestions.
//!
//! Predicate remediation is inert data: Fixer re-signs suggested atom
//! templates as a separate follow-up slice so the repair remains auditable as
//! its own shipping event.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{
    derive_slice, Approval, Atom, AtomId, CoverageMap, IntentId, InvariantResult, Provenance,
    Remediation, Slice, SliceDerivationError, SliceId,
};

pub const FIXER_PERSONA_NAME: &str = "fixer";
pub const FIXER_TRIGGER: &str = "invariant.blocked_with_remediation";

const MAX_REMEDIATION_DESCRIPTION_CHARS: usize = 200;

pub struct FixerSigningContext<'a> {
    pub principal_id: String,
    pub persona_id: String,
    pub agent_run_id: String,
    pub trace_id: String,
    pub transcript_ref: String,
    pub timestamp: OffsetDateTime,
    pub principal_key: &'a SigningKey,
    pub persona_key: &'a SigningKey,
    pub tool_call_id: Option<String>,
    pub original_author_cosignature: bool,
}

impl<'a> FixerSigningContext<'a> {
    pub fn new(
        principal_id: impl Into<String>,
        agent_run_id: impl Into<String>,
        trace_id: impl Into<String>,
        transcript_ref: impl Into<String>,
        principal_key: &'a SigningKey,
        persona_key: &'a SigningKey,
    ) -> Self {
        Self {
            principal_id: principal_id.into(),
            persona_id: FIXER_PERSONA_NAME.to_string(),
            agent_run_id: agent_run_id.into(),
            trace_id: trace_id.into(),
            transcript_ref: transcript_ref.into(),
            timestamp: OffsetDateTime::now_utc(),
            principal_key,
            persona_key,
            tool_call_id: None,
            original_author_cosignature: false,
        }
    }
}

pub struct FixerProposalInput<'a> {
    pub blocked_slice: &'a Slice,
    pub remediation: &'a Remediation,
    pub atom_index: &'a BTreeMap<AtomId, Atom>,
    pub coverage: &'a CoverageMap,
    pub invariants_applied: Vec<(super::PredicateHash, InvariantResult)>,
    pub approval_chain: Vec<Approval>,
    pub base_ref: Option<AtomId>,
    pub signing: FixerSigningContext<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixerFollowUpProposal {
    pub slice: Slice,
    pub intent: IntentId,
    pub remediation_atoms: Vec<Atom>,
    pub receipt: FixerReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixerReceipt {
    pub trigger: String,
    pub blocked_slice_id: SliceId,
    pub follow_up_slice_id: SliceId,
    pub remediation_atom_ids: Vec<AtomId>,
    pub principal: String,
    pub persona: String,
    pub original_author_cosignature: bool,
    pub description: String,
}

#[derive(Debug)]
pub enum FixerError {
    InvalidRemediation(String),
    MissingBlockedAtom(AtomId),
    DuplicateRemediationAtom(AtomId),
    Atom(super::AtomError),
    Slice(SliceDerivationError),
}

impl fmt::Display for FixerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRemediation(message) => write!(f, "invalid remediation: {message}"),
            Self::MissingBlockedAtom(atom) => {
                write!(
                    f,
                    "blocked slice atom {atom} is missing from the atom index"
                )
            }
            Self::DuplicateRemediationAtom(atom) => {
                write!(f, "duplicate remediation atom {atom}")
            }
            Self::Atom(error) => write!(f, "{error}"),
            Self::Slice(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FixerError {}

impl From<super::AtomError> for FixerError {
    fn from(error: super::AtomError) -> Self {
        Self::Atom(error)
    }
}

impl From<SliceDerivationError> for FixerError {
    fn from(error: SliceDerivationError) -> Self {
        Self::Slice(error)
    }
}

pub fn propose_follow_up_slice(
    input: FixerProposalInput<'_>,
) -> Result<FixerFollowUpProposal, FixerError> {
    validate_remediation(input.remediation)?;
    for atom in &input.blocked_slice.atoms {
        if !input.atom_index.contains_key(atom) {
            return Err(FixerError::MissingBlockedAtom(*atom));
        }
    }

    let remediation_atoms =
        materialize_remediation_atoms(input.blocked_slice, input.remediation, &input.signing)?;
    let mut atoms = input.atom_index.clone();
    let mut remediation_atom_ids = Vec::with_capacity(remediation_atoms.len());
    for atom in &remediation_atoms {
        atom.verify()?;
        if atoms.insert(atom.id, atom.clone()).is_some() {
            return Err(FixerError::DuplicateRemediationAtom(atom.id));
        }
        remediation_atom_ids.push(atom.id);
    }

    let intent = follow_up_intent_id(input.blocked_slice.id, &remediation_atom_ids);
    let mut intent_atoms = input.blocked_slice.atoms.clone();
    intent_atoms.extend(remediation_atom_ids.iter().copied());
    let intents = BTreeMap::from([(intent, intent_atoms)]);
    let slice = derive_slice(super::SliceDerivationInput {
        atoms: &atoms,
        intents: &intents,
        candidate_intents: vec![intent],
        coverage: input.coverage,
        invariants_applied: input.invariants_applied,
        approval_chain: input.approval_chain,
        base_ref: input.base_ref.unwrap_or(input.blocked_slice.base_ref),
    })?;

    let receipt_persona = normalized_fixer_persona(&input.signing.persona_id).to_string();
    Ok(FixerFollowUpProposal {
        receipt: FixerReceipt {
            trigger: FIXER_TRIGGER.to_string(),
            blocked_slice_id: input.blocked_slice.id,
            follow_up_slice_id: slice.id,
            remediation_atom_ids,
            principal: input.signing.principal_id,
            persona: receipt_persona,
            original_author_cosignature: input.signing.original_author_cosignature,
            description: input.remediation.description.clone(),
        },
        slice,
        intent,
        remediation_atoms,
    })
}

fn materialize_remediation_atoms(
    blocked_slice: &Slice,
    remediation: &Remediation,
    signing: &FixerSigningContext<'_>,
) -> Result<Vec<Atom>, FixerError> {
    let suggested_atoms = remediation_atoms(remediation)?;
    let mut atoms = Vec::with_capacity(suggested_atoms.len());
    for template in suggested_atoms {
        let parents = blocked_slice.atoms.iter().copied().collect::<BTreeSet<_>>();
        let provenance = Provenance {
            principal: signing.principal_id.clone(),
            persona: normalized_fixer_persona(&signing.persona_id).to_string(),
            agent_run_id: signing.agent_run_id.clone(),
            tool_call_id: signing.tool_call_id.clone(),
            trace_id: signing.trace_id.clone(),
            transcript_ref: signing.transcript_ref.clone(),
            timestamp: signing.timestamp,
        };
        atoms.push(Atom::sign(
            template.ops.clone(),
            parents.into_iter().collect(),
            provenance,
            template.inverse_of,
            signing.principal_key,
            signing.persona_key,
        )?);
    }
    Ok(atoms)
}

fn validate_remediation(remediation: &Remediation) -> Result<(), FixerError> {
    if remediation.description.trim().is_empty() {
        return Err(FixerError::InvalidRemediation(
            "remediation description is required".to_string(),
        ));
    }
    if remediation.description.chars().count() > MAX_REMEDIATION_DESCRIPTION_CHARS {
        return Err(FixerError::InvalidRemediation(format!(
            "remediation description must be <= {MAX_REMEDIATION_DESCRIPTION_CHARS} chars"
        )));
    }
    remediation_atoms(remediation)?;
    Ok(())
}

fn remediation_atoms(remediation: &Remediation) -> Result<&[Atom], FixerError> {
    let atoms = remediation.suggested_atoms.as_deref().ok_or_else(|| {
        FixerError::InvalidRemediation(
            "remediation requires at least one suggested atom".to_string(),
        )
    })?;
    if atoms.is_empty() {
        return Err(FixerError::InvalidRemediation(
            "remediation requires at least one suggested atom".to_string(),
        ));
    }
    Ok(atoms)
}

fn normalized_fixer_persona(persona: &str) -> &str {
    if persona.trim().is_empty() {
        FIXER_PERSONA_NAME
    } else {
        persona
    }
}

fn follow_up_intent_id(blocked_slice: SliceId, remediation_atoms: &[AtomId]) -> IntentId {
    let mut hasher = Sha256::new();
    hasher.update(b"harn.flow.fixer.follow_up.v0");
    hasher.update(blocked_slice.0);
    for atom in remediation_atoms {
        hasher.update(atom.0);
    }
    IntentId(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{AtomSignature, InvariantBlockError, PredicateHash, SliceStatus, TextOp};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn provenance(persona: &str) -> Provenance {
        Provenance {
            principal: "user:alice".to_string(),
            persona: persona.to_string(),
            agent_run_id: "run-1".to_string(),
            tool_call_id: None,
            trace_id: "trace-1".to_string(),
            transcript_ref: "transcript-1".to_string(),
            timestamp: OffsetDateTime::from_unix_timestamp(0).unwrap(),
        }
    }

    fn signed_atom(index: u8, ops: Vec<TextOp>, parents: Vec<AtomId>, persona: &str) -> Atom {
        Atom::sign(
            ops,
            parents,
            provenance(persona),
            None,
            &key(index),
            &key(index + 1),
        )
        .unwrap()
    }

    fn predicate_result(slice: &Slice, atoms: &BTreeMap<AtomId, Atom>) -> InvariantResult {
        let mut document = Vec::<u8>::new();
        for atom_id in &slice.atoms {
            atoms.get(atom_id).unwrap().apply(&mut document).unwrap();
        }
        if String::from_utf8(document).unwrap().contains("fixed") {
            InvariantResult::allow()
        } else {
            InvariantResult::block(InvariantBlockError::new(
                "needs_fix",
                "slice needs the suggested remediation",
            ))
            .with_remediation(
                Remediation::describe("Append the missing fixed marker.").with_suggested_atoms(
                    vec![signed_atom(
                        20,
                        vec![TextOp::Insert {
                            offset: 3,
                            content: " fixed".to_string(),
                        }],
                        vec![],
                        "predicate",
                    )],
                ),
            )
        }
    }

    fn blocked_remediation(result: InvariantResult) -> (InvariantBlockError, Remediation) {
        let error = result
            .block_error()
            .expect("expected blocking result")
            .clone();
        let remediation = result.remediation.expect("expected remediation suggestion");
        (error, remediation)
    }

    #[test]
    fn fixer_materializes_remediation_as_follow_up_slice_that_passes_predicate() {
        let original = signed_atom(
            1,
            vec![TextOp::Insert {
                offset: 0,
                content: "bad".to_string(),
            }],
            vec![],
            "ship-captain",
        );
        let blocked_slice = Slice {
            id: SliceId([9; 32]),
            atoms: vec![original.id],
            intents: Vec::new(),
            invariants_applied: Vec::new(),
            required_tests: Vec::new(),
            approval_chain: Vec::new(),
            base_ref: original.id,
            status: SliceStatus::Ready,
        };
        let atom_index = BTreeMap::from([(original.id, original.clone())]);
        let (error, remediation) =
            blocked_remediation(predicate_result(&blocked_slice, &atom_index));
        let principal_key = key(50);
        let persona_key = key(51);
        let mut signing = FixerSigningContext::new(
            "user:alice",
            "fixer-run-1",
            "trace-fix",
            "transcript-fix",
            &principal_key,
            &persona_key,
        );
        signing.timestamp = OffsetDateTime::from_unix_timestamp(1).unwrap();
        signing.original_author_cosignature = true;

        let proposal = propose_follow_up_slice(FixerProposalInput {
            blocked_slice: &blocked_slice,
            remediation: &remediation,
            atom_index: &atom_index,
            coverage: &CoverageMap::new(),
            invariants_applied: vec![(
                PredicateHash::new("predicate:v1"),
                InvariantResult::block(error).with_remediation(remediation.clone()),
            )],
            approval_chain: Vec::new(),
            base_ref: None,
            signing,
        })
        .unwrap();

        assert_eq!(proposal.slice.atoms.len(), 2);
        assert!(proposal.slice.atoms.contains(&original.id));
        let remediation_atom = &proposal.remediation_atoms[0];
        assert_eq!(remediation_atom.provenance.principal, "user:alice");
        assert_eq!(remediation_atom.provenance.persona, FIXER_PERSONA_NAME);
        assert_eq!(
            remediation_atom.signature.principal_key,
            principal_key.verifying_key().to_bytes()
        );
        assert_eq!(
            remediation_atom.signature.persona_key,
            persona_key.verifying_key().to_bytes()
        );
        assert!(remediation_atom.parents.contains(&original.id));
        assert_eq!(proposal.receipt.trigger, FIXER_TRIGGER);
        assert!(proposal.receipt.original_author_cosignature);

        let mut follow_up_atoms = atom_index;
        follow_up_atoms.insert(remediation_atom.id, remediation_atom.clone());
        assert_eq!(
            predicate_result(&proposal.slice, &follow_up_atoms),
            InvariantResult::allow()
        );
    }

    #[test]
    fn remediation_rejects_empty_suggestions() {
        let remediation = Remediation::describe("Do something").with_suggested_atoms(Vec::new());
        let error = validate_remediation(&remediation).unwrap_err();
        assert!(matches!(error, FixerError::InvalidRemediation(_)));
    }

    #[test]
    fn materialized_atom_signature_verifies() {
        let template = Atom {
            id: AtomId([7; 32]),
            ops: vec![TextOp::Insert {
                offset: 0,
                content: "fixed".to_string(),
            }],
            parents: Vec::new(),
            provenance: provenance("predicate"),
            signature: AtomSignature {
                principal_key: [0; 32],
                principal_sig: [0; 64],
                persona_key: [0; 32],
                persona_sig: [0; 64],
            },
            inverse_of: None,
        };
        let original = signed_atom(1, Vec::new(), Vec::new(), "ship-captain");
        let remediation =
            Remediation::describe("Apply the fix").with_suggested_atoms(vec![template]);
        let principal_key = key(60);
        let persona_key = key(61);
        let signing = FixerSigningContext::new(
            "fixer-service",
            "fixer-run-2",
            "trace-fix",
            "transcript-fix",
            &principal_key,
            &persona_key,
        );

        let atoms = materialize_remediation_atoms(
            &Slice {
                id: SliceId([1; 32]),
                atoms: vec![original.id],
                intents: Vec::new(),
                invariants_applied: Vec::new(),
                required_tests: Vec::new(),
                approval_chain: Vec::new(),
                base_ref: original.id,
                status: SliceStatus::Ready,
            },
            &remediation,
            &signing,
        )
        .unwrap();

        atoms[0].verify().unwrap();
        assert_eq!(atoms[0].provenance.persona, FIXER_PERSONA_NAME);
    }
}
