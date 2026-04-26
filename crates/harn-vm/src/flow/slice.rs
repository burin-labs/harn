//! Harn Flow `Slice` primitive.
//!
//! A slice is a deterministic DAG-closed bundle of atoms plus the smallest test
//! gate implied by collector-provided atom coverage. The derivation code stays
//! storage-agnostic: callers provide an atom index, intent-to-atom references,
//! and a coverage map populated by language-specific collectors.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use super::predicates::InvariantResult;
use super::{Atom, AtomId, IntentId};

const SLICE_ID_BYTES: usize = 32;

/// 32-byte SHA-256 content address of a [`Slice`] body.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SliceId(pub [u8; SLICE_ID_BYTES]);

impl SliceId {
    /// Produce a hex-encoded representation suitable for logs and JSON.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character hex string into a `SliceId`.
    pub fn from_hex(raw: &str) -> Result<Self, SliceDerivationError> {
        let bytes = hex::decode(raw)
            .map_err(|error| SliceDerivationError::InvalidSliceId(error.to_string()))?;
        if bytes.len() != SLICE_ID_BYTES {
            return Err(SliceDerivationError::InvalidSliceId(format!(
                "SliceId must be {SLICE_ID_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; SLICE_ID_BYTES];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

impl fmt::Debug for SliceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SliceId({})", self.to_hex())
    }
}

impl fmt::Display for SliceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl Serialize for SliceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for SliceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        SliceId::from_hex(&raw).map_err(serde::de::Error::custom)
    }
}

/// Opaque language-agnostic test identifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TestId(String);

impl TestId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TestId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for TestId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Opaque invariant predicate hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PredicateHash(String);

impl PredicateHash {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PredicateHash {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for PredicateHash {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Human or system approval included in the slice audit trail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub reviewer: String,
    pub approved_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Collector-facing coverage contract.
///
/// `tests_by_atom` answers "which tests exercise this atom?". `atoms_by_test`
/// answers "which atoms are exercised by this test?". Derivation uses both
/// directions to compute a fixed point: touched atoms select required tests,
/// and required tests pull in any additional atoms those tests exercise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageMap {
    #[serde(default)]
    pub tests_by_atom: BTreeMap<AtomId, BTreeSet<TestId>>,
    #[serde(default)]
    pub atoms_by_test: BTreeMap<TestId, BTreeSet<AtomId>>,
}

impl CoverageMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one bidirectional coverage edge.
    pub fn insert(&mut self, atom: AtomId, test: TestId) {
        self.tests_by_atom
            .entry(atom)
            .or_default()
            .insert(test.clone());
        self.atoms_by_test.entry(test).or_default().insert(atom);
    }

    fn tests_for_atoms(&self, atoms: &BTreeSet<AtomId>) -> BTreeSet<TestId> {
        atoms
            .iter()
            .filter_map(|atom| self.tests_by_atom.get(atom))
            .flat_map(|tests| tests.iter().cloned())
            .collect()
    }

    fn atoms_for_tests(&self, tests: &BTreeSet<TestId>) -> BTreeSet<AtomId> {
        tests
            .iter()
            .filter_map(|test| self.atoms_by_test.get(test))
            .flat_map(|atoms| atoms.iter().copied())
            .collect()
    }
}

/// Missing parent edge that kept an atom out of a derived slice.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnresolvedParent {
    pub atom: AtomId,
    pub missing_parent: AtomId,
}

/// Lifecycle state of a derived slice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SliceStatus {
    Ready,
    Empty,
    Blocked {
        unresolved_parents: Vec<UnresolvedParent>,
    },
}

/// Atomic unit shipped by Harn Flow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub id: SliceId,
    pub atoms: Vec<AtomId>,
    pub intents: Vec<IntentId>,
    pub invariants_applied: Vec<(PredicateHash, InvariantResult)>,
    pub required_tests: Vec<TestId>,
    pub approval_chain: Vec<Approval>,
    pub base_ref: AtomId,
    pub status: SliceStatus,
}

/// Input contract for [`derive_slice`].
pub struct SliceDerivationInput<'a> {
    pub atoms: &'a BTreeMap<AtomId, Atom>,
    pub intents: &'a BTreeMap<IntentId, Vec<AtomId>>,
    pub candidate_intents: Vec<IntentId>,
    pub coverage: &'a CoverageMap,
    pub invariants_applied: Vec<(PredicateHash, InvariantResult)>,
    pub approval_chain: Vec<Approval>,
    pub base_ref: AtomId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SliceDerivationError {
    MissingIntent(IntentId),
    MissingAtom(AtomId),
    CyclicAtomParents,
    InvalidSliceId(String),
    Serialize(String),
}

impl fmt::Display for SliceDerivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SliceDerivationError::MissingIntent(id) => {
                write!(f, "slice derivation missing intent {id}")
            }
            SliceDerivationError::MissingAtom(id) => {
                write!(f, "slice derivation missing atom {id}")
            }
            SliceDerivationError::CyclicAtomParents => {
                write!(f, "slice derivation found cyclic atom parents")
            }
            SliceDerivationError::InvalidSliceId(message) => {
                write!(f, "slice id invalid: {message}")
            }
            SliceDerivationError::Serialize(message) => {
                write!(f, "slice derivation serialization failed: {message}")
            }
        }
    }
}

impl std::error::Error for SliceDerivationError {}

/// Derive a deterministic, dependency-closed slice for a candidate intent set.
pub fn derive_slice(input: SliceDerivationInput<'_>) -> Result<Slice, SliceDerivationError> {
    let intents = sorted_unique(input.candidate_intents);
    let mut seeds = BTreeSet::new();
    for intent in &intents {
        let atom_ids = input
            .intents
            .get(intent)
            .ok_or(SliceDerivationError::MissingIntent(*intent))?;
        for atom_id in atom_ids {
            if !input.atoms.contains_key(atom_id) {
                return Err(SliceDerivationError::MissingAtom(*atom_id));
            }
            seeds.insert(*atom_id);
        }
    }

    let mut included = BTreeSet::new();
    let mut required_tests = BTreeSet::new();
    let mut unresolved = BTreeSet::new();

    loop {
        let closure = resolve_coherent_closure(&seeds, input.atoms, &mut unresolved)?;
        let tests = input.coverage.tests_for_atoms(&closure);
        let coverage_atoms = input.coverage.atoms_for_tests(&tests);

        let mut next_seeds = seeds.clone();
        for atom_id in coverage_atoms {
            if !input.atoms.contains_key(&atom_id) {
                return Err(SliceDerivationError::MissingAtom(atom_id));
            }
            next_seeds.insert(atom_id);
        }

        if next_seeds == seeds && tests == required_tests && closure == included {
            included = closure;
            required_tests = tests;
            break;
        }

        seeds = next_seeds;
        included = closure;
        required_tests = tests;
    }

    let atom_order = stable_topological_order(&included, input.atoms)?;
    let required_tests = required_tests.into_iter().collect::<Vec<_>>();
    let unresolved_parents = unresolved.into_iter().collect::<Vec<_>>();
    let status = if !unresolved_parents.is_empty() {
        SliceStatus::Blocked { unresolved_parents }
    } else if atom_order.is_empty() {
        SliceStatus::Empty
    } else {
        SliceStatus::Ready
    };

    let mut slice = Slice {
        id: SliceId([0; SLICE_ID_BYTES]),
        atoms: atom_order,
        intents,
        invariants_applied: input.invariants_applied,
        required_tests,
        approval_chain: input.approval_chain,
        base_ref: input.base_ref,
        status,
    };
    slice.id = compute_slice_id(&slice)?;
    Ok(slice)
}

fn sorted_unique(mut intents: Vec<IntentId>) -> Vec<IntentId> {
    intents.sort();
    intents.dedup();
    intents
}

fn resolve_coherent_closure(
    seeds: &BTreeSet<AtomId>,
    atoms: &BTreeMap<AtomId, Atom>,
    unresolved: &mut BTreeSet<UnresolvedParent>,
) -> Result<BTreeSet<AtomId>, SliceDerivationError> {
    let mut included = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut rejected = BTreeSet::new();
    let mut cycle_detected = false;
    for seed in seeds {
        resolve_atom(
            *seed,
            atoms,
            &mut included,
            &mut visiting,
            &mut rejected,
            unresolved,
            &mut cycle_detected,
        );
    }
    if cycle_detected {
        Err(SliceDerivationError::CyclicAtomParents)
    } else {
        Ok(included)
    }
}

fn resolve_atom(
    atom_id: AtomId,
    atoms: &BTreeMap<AtomId, Atom>,
    included: &mut BTreeSet<AtomId>,
    visiting: &mut BTreeSet<AtomId>,
    rejected: &mut BTreeSet<AtomId>,
    unresolved: &mut BTreeSet<UnresolvedParent>,
    cycle_detected: &mut bool,
) -> bool {
    if included.contains(&atom_id) {
        return true;
    }
    if rejected.contains(&atom_id) {
        return false;
    }
    if !visiting.insert(atom_id) {
        *cycle_detected = true;
        return false;
    }

    let Some(atom) = atoms.get(&atom_id) else {
        rejected.insert(atom_id);
        visiting.remove(&atom_id);
        return false;
    };

    let mut parents_resolved = true;
    for parent in &atom.parents {
        if !atoms.contains_key(parent) {
            unresolved.insert(UnresolvedParent {
                atom: atom_id,
                missing_parent: *parent,
            });
            parents_resolved = false;
            continue;
        }
        if !resolve_atom(
            *parent,
            atoms,
            included,
            visiting,
            rejected,
            unresolved,
            cycle_detected,
        ) {
            parents_resolved = false;
        }
    }

    visiting.remove(&atom_id);
    if parents_resolved {
        included.insert(atom_id);
        true
    } else {
        rejected.insert(atom_id);
        false
    }
}

fn stable_topological_order(
    included: &BTreeSet<AtomId>,
    atoms: &BTreeMap<AtomId, Atom>,
) -> Result<Vec<AtomId>, SliceDerivationError> {
    let mut indegree = BTreeMap::<AtomId, usize>::new();
    let mut children = BTreeMap::<AtomId, BTreeSet<AtomId>>::new();
    for atom_id in included {
        indegree.insert(*atom_id, 0);
    }

    for atom_id in included {
        let atom = atoms
            .get(atom_id)
            .ok_or(SliceDerivationError::MissingAtom(*atom_id))?;
        for parent in atom
            .parents
            .iter()
            .filter(|parent| included.contains(parent))
        {
            *indegree.entry(*atom_id).or_default() += 1;
            children.entry(*parent).or_default().insert(*atom_id);
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(atom_id, degree)| (*degree == 0).then_some(*atom_id))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(included.len());

    while let Some(atom_id) = ready.pop_first() {
        ordered.push(atom_id);
        if let Some(children) = children.get(&atom_id) {
            for child in children {
                let degree = indegree.get_mut(child).expect("child has indegree");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(*child);
                }
            }
        }
    }

    if ordered.len() != included.len() {
        return Err(SliceDerivationError::CyclicAtomParents);
    }
    Ok(ordered)
}

fn compute_slice_id(slice: &Slice) -> Result<SliceId, SliceDerivationError> {
    #[derive(Serialize)]
    struct SliceBody<'a> {
        atoms: &'a [AtomId],
        intents: &'a [IntentId],
        invariants_applied: &'a [(PredicateHash, InvariantResult)],
        required_tests: &'a [TestId],
        approval_chain: &'a [Approval],
        base_ref: AtomId,
        status: &'a SliceStatus,
    }

    let body = SliceBody {
        atoms: &slice.atoms,
        intents: &slice.intents,
        invariants_applied: &slice.invariants_applied,
        required_tests: &slice.required_tests,
        approval_chain: &slice.approval_chain,
        base_ref: slice.base_ref,
        status: &slice.status,
    };
    let bytes = serde_json::to_vec(&body)
        .map_err(|error| SliceDerivationError::Serialize(error.to_string()))?;
    Ok(SliceId(Sha256::digest(bytes).into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{AtomSignature, Provenance, TextOp};
    use proptest::prelude::*;
    use time::OffsetDateTime;

    fn id(byte: u8) -> AtomId {
        AtomId([byte; 32])
    }

    fn intent(name: impl Into<String>) -> IntentId {
        IntentId(Sha256::digest(name.into().as_bytes()).into())
    }

    fn test(name: impl Into<String>) -> TestId {
        TestId::new(name)
    }

    fn atom(atom_id: AtomId, parents: Vec<AtomId>) -> Atom {
        Atom {
            id: atom_id,
            ops: Vec::<TextOp>::new(),
            parents,
            provenance: Provenance {
                principal: "user:alice".to_string(),
                persona: "ship-captain".to_string(),
                agent_run_id: "run-0001".to_string(),
                tool_call_id: None,
                trace_id: "trace-0001".to_string(),
                transcript_ref: "transcript:0001".to_string(),
                timestamp: OffsetDateTime::from_unix_timestamp(0).unwrap(),
            },
            signature: AtomSignature {
                principal_key: [0; 32],
                principal_sig: [0; 64],
                persona_key: [0; 32],
                persona_sig: [0; 64],
            },
            inverse_of: None,
        }
    }

    fn derive(
        atoms: &BTreeMap<AtomId, Atom>,
        intents: &BTreeMap<IntentId, Vec<AtomId>>,
        candidate_intents: Vec<IntentId>,
        coverage: &CoverageMap,
    ) -> Slice {
        derive_slice(SliceDerivationInput {
            atoms,
            intents,
            candidate_intents,
            coverage,
            invariants_applied: Vec::new(),
            approval_chain: Vec::new(),
            base_ref: id(0),
        })
        .unwrap()
    }

    proptest! {
        #[test]
        fn closure_contains_candidate_ancestors_and_parents_precede_children(
            parent_bits in proptest::collection::vec(any::<u16>(), 1..12),
            selected_bits in any::<u16>(),
        ) {
            let mut atoms = BTreeMap::new();
            let mut expected_closure = BTreeSet::new();
            let mut parents_by_atom = Vec::new();
            for (index, bits) in parent_bits.iter().copied().enumerate() {
                let atom_id = id((index + 1) as u8);
                let parents = (0..index)
                    .filter(|parent| bits & (1 << (parent % 16)) != 0)
                    .map(|parent| id((parent + 1) as u8))
                    .collect::<Vec<_>>();
                atoms.insert(atom_id, atom(atom_id, parents.clone()));
                parents_by_atom.push((atom_id, parents));
            }

            let selected = parents_by_atom
                .iter()
                .enumerate()
                .filter_map(|(index, (atom_id, _))| {
                    (selected_bits & (1 << (index % 16)) != 0).then_some(*atom_id)
                })
                .collect::<Vec<_>>();
            let selected = if selected.is_empty() {
                vec![parents_by_atom[0].0]
            } else {
                selected
            };

            fn add_ancestors(
                atom_id: AtomId,
                atoms: &BTreeMap<AtomId, Atom>,
                expected: &mut BTreeSet<AtomId>,
            ) {
                if !expected.insert(atom_id) {
                    return;
                }
                for parent in &atoms.get(&atom_id).unwrap().parents {
                    add_ancestors(*parent, atoms, expected);
                }
            }

            for atom_id in &selected {
                add_ancestors(*atom_id, &atoms, &mut expected_closure);
            }

            let intents = BTreeMap::from([(intent("intent:selected"), selected)]);
            let slice = derive(
                &atoms,
                &intents,
                vec![intent("intent:selected")],
                &CoverageMap::new(),
            );

            prop_assert_eq!(
                slice.atoms.iter().copied().collect::<BTreeSet<_>>(),
                expected_closure
            );
            prop_assert_eq!(slice.status, SliceStatus::Ready);

            let positions = slice
                .atoms
                .iter()
                .enumerate()
                .map(|(index, atom_id)| (*atom_id, index))
                .collect::<BTreeMap<_, _>>();
            for atom_id in &slice.atoms {
                for parent in &atoms.get(atom_id).unwrap().parents {
                    prop_assert!(positions[parent] < positions[atom_id]);
                }
            }
        }

        #[test]
        fn stability_across_rederivations(candidate_flip in any::<bool>()) {
            let atom_a = id(1);
            let atom_b = id(2);
            let atom_c = id(3);
            let atoms = BTreeMap::from([
                (atom_c, atom(atom_c, vec![atom_a, atom_b])),
                (atom_a, atom(atom_a, Vec::new())),
                (atom_b, atom(atom_b, vec![atom_a])),
            ]);
            let intents = BTreeMap::from([
                (intent("intent:beta"), vec![atom_c]),
                (intent("intent:alpha"), vec![atom_b]),
            ]);
            let candidates = if candidate_flip {
                vec![intent("intent:beta"), intent("intent:alpha")]
            } else {
                vec![intent("intent:alpha"), intent("intent:beta")]
            };
            let mut coverage = CoverageMap::new();
            coverage.insert(atom_b, test("test:flow"));
            coverage.insert(atom_c, test("test:flow"));

            let first = derive(&atoms, &intents, candidates.clone(), &coverage);
            let second = derive(&atoms, &intents, candidates.into_iter().rev().collect(), &coverage);

            prop_assert_eq!(&first, &second);
            prop_assert_eq!(first.atoms, vec![atom_a, atom_b, atom_c]);
            prop_assert_eq!(first.required_tests, vec![test("test:flow")]);
        }
    }

    #[test]
    fn coverage_map_selects_tests_and_pulls_test_covered_atoms() {
        let touched = id(1);
        let helper_parent = id(2);
        let helper = id(3);
        let atoms = BTreeMap::from([
            (touched, atom(touched, Vec::new())),
            (helper_parent, atom(helper_parent, Vec::new())),
            (helper, atom(helper, vec![helper_parent])),
        ]);
        let intents = BTreeMap::from([(intent("intent:change"), vec![touched])]);
        let mut coverage = CoverageMap::new();
        coverage.insert(touched, test("test:targeted"));
        coverage.insert(helper, test("test:targeted"));

        let slice = derive(&atoms, &intents, vec![intent("intent:change")], &coverage);

        assert_eq!(slice.atoms, vec![touched, helper_parent, helper]);
        assert_eq!(slice.required_tests, vec![test("test:targeted")]);
        assert_eq!(slice.status, SliceStatus::Ready);
    }

    #[test]
    fn atoms_with_unresolved_parents_are_excluded_and_mark_slice_blocked() {
        let parent = id(1);
        let child = id(2);
        let atoms = BTreeMap::from([(child, atom(child, vec![parent]))]);
        let intents = BTreeMap::from([(intent("intent:change"), vec![child])]);

        let slice = derive(
            &atoms,
            &intents,
            vec![intent("intent:change")],
            &CoverageMap::new(),
        );

        assert!(slice.atoms.is_empty());
        assert_eq!(slice.required_tests, Vec::<TestId>::new());
        assert_eq!(
            slice.status,
            SliceStatus::Blocked {
                unresolved_parents: vec![UnresolvedParent {
                    atom: child,
                    missing_parent: parent,
                }],
            }
        );
    }

    #[test]
    fn cyclic_parent_graph_is_rejected() {
        let atom_a = id(1);
        let atom_b = id(2);
        let atoms = BTreeMap::from([
            (atom_a, atom(atom_a, vec![atom_b])),
            (atom_b, atom(atom_b, vec![atom_a])),
        ]);
        let intents = BTreeMap::from([(intent("intent:cycle"), vec![atom_a])]);

        let error = derive_slice(SliceDerivationInput {
            atoms: &atoms,
            intents: &intents,
            candidate_intents: vec![intent("intent:cycle")],
            coverage: &CoverageMap::new(),
            invariants_applied: Vec::new(),
            approval_chain: Vec::new(),
            base_ref: id(0),
        })
        .unwrap_err();

        assert_eq!(error, SliceDerivationError::CyclicAtomParents);
    }

    #[test]
    fn slice_id_round_trips_through_json() {
        let atom_id = id(1);
        let atoms = BTreeMap::from([(atom_id, atom(atom_id, Vec::new()))]);
        let intents = BTreeMap::from([(intent("intent:change"), vec![atom_id])]);
        let slice = derive(
            &atoms,
            &intents,
            vec![intent("intent:change")],
            &CoverageMap::new(),
        );

        let json = serde_json::to_vec(&slice).unwrap();
        let decoded: Slice = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded, slice);
    }
}
