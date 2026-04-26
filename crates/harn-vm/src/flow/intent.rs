//! Harn Flow `Intent` primitive.
//!
//! An `Intent` is a semantic bundle of atoms that appear to serve one goal.
//! The default clusterer stays intentionally mechanical: it groups atoms that
//! share an agent run and transcript, then splits them when transcript event
//! indexes drift too far apart. Ambiguous same-run gaps can be delegated to an
//! optional semantic classifier, bounded by an explicit budget.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use super::atom::{Atom, AtomError, AtomId};

const INTENT_ID_BYTES: usize = 32;
const DEFAULT_MAX_EVENT_GAP: u64 = 8;
const DEFAULT_CONFIDENCE: f32 = 0.75;
const SEMANTIC_CONFIDENCE: f32 = 0.9;

/// 32-byte deterministic identifier for an `Intent`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IntentId(pub [u8; INTENT_ID_BYTES]);

impl IntentId {
    /// Produce a hex-encoded representation suitable for logs and JSON.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character hex string into an `IntentId`.
    pub fn from_hex(raw: &str) -> Result<Self, IntentError> {
        let bytes = hex::decode(raw)
            .map_err(|error| IntentError::Invalid(format!("invalid IntentId hex: {error}")))?;
        if bytes.len() != INTENT_ID_BYTES {
            return Err(IntentError::Invalid(format!(
                "IntentId must be {INTENT_ID_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; INTENT_ID_BYTES];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

impl fmt::Debug for IntentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IntentId({})", self.to_hex())
    }
}

impl fmt::Display for IntentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl Serialize for IntentId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for IntentId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        IntentId::from_hex(&raw).map_err(serde::de::Error::custom)
    }
}

/// Errors produced when parsing or validating intent inputs.
#[derive(Debug)]
pub enum IntentError {
    /// Invalid field shape or value.
    Invalid(String),
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::Invalid(message) => write!(f, "intent invalid: {message}"),
        }
    }
}

impl std::error::Error for IntentError {}

/// Inclusive transcript event span for the observations that produced an
/// intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSpan {
    /// Transcript reference shared by every atom in this intent.
    pub transcript_ref: String,
    /// First transcript event index included in the cluster.
    pub start_event_index: u64,
    /// Last transcript event index included in the cluster.
    pub end_event_index: u64,
}

impl TranscriptSpan {
    /// Build an inclusive span.
    pub fn new(
        transcript_ref: impl Into<String>,
        start_event_index: u64,
        end_event_index: u64,
    ) -> Result<Self, IntentError> {
        if end_event_index < start_event_index {
            return Err(IntentError::Invalid(format!(
                "end_event_index {end_event_index} precedes start_event_index {start_event_index}"
            )));
        }
        Ok(Self {
            transcript_ref: transcript_ref.into(),
            start_event_index,
            end_event_index,
        })
    }

    fn extend_to(&mut self, event_index: u64) {
        self.start_event_index = self.start_event_index.min(event_index);
        self.end_event_index = self.end_event_index.max(event_index);
    }
}

/// A semantic bundle of atoms that expresses one goal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub id: IntentId,
    pub atoms: Vec<AtomId>,
    pub goal_description: String,
    pub origin_transcript_span: TranscriptSpan,
    pub confidence: f32,
}

impl Intent {
    /// Build an intent from already-clustered atom ids.
    pub fn new(
        atoms: Vec<AtomId>,
        goal_description: impl Into<String>,
        origin_transcript_span: TranscriptSpan,
        confidence: f32,
    ) -> Result<Self, IntentError> {
        if atoms.is_empty() {
            return Err(IntentError::Invalid(
                "intent must contain at least one atom".to_string(),
            ));
        }
        let goal_description = goal_description.into();
        let confidence = normalize_confidence(confidence)?;
        let id = derive_intent_id(&atoms, &goal_description, &origin_transcript_span);
        Ok(Self {
            id,
            atoms,
            goal_description,
            origin_transcript_span,
            confidence,
        })
    }

    /// Seal the intent for a future slice derivation. The returned value owns a
    /// snapshot of the current atom set, so later intent mutation cannot change
    /// what was sealed.
    pub fn seal(&self) -> SealedIntent {
        SealedIntent {
            id: self.id,
            atoms: self.atoms.clone(),
            goal_description: self.goal_description.clone(),
            origin_transcript_span: self.origin_transcript_span.clone(),
            confidence: self.confidence,
        }
    }
}

/// Immutable snapshot of an intent's atoms at seal time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SealedIntent {
    pub id: IntentId,
    pub atoms: Vec<AtomId>,
    pub goal_description: String,
    pub origin_transcript_span: TranscriptSpan,
    pub confidence: f32,
}

/// Atom plus transcript location, as observed from transcripts and tool-call
/// logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedAtom {
    pub atom_id: AtomId,
    pub agent_run_id: String,
    pub transcript_ref: String,
    pub transcript_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_description: Option<String>,
}

impl ObservedAtom {
    /// Lift a signed atom into a clusterable transcript observation.
    pub fn from_atom(atom: &Atom, transcript_event_index: u64) -> Self {
        Self {
            atom_id: atom.id,
            agent_run_id: atom.provenance.agent_run_id.clone(),
            transcript_ref: atom.provenance.transcript_ref.clone(),
            transcript_event_index,
            tool_call_id: atom.provenance.tool_call_id.clone(),
            goal_description: None,
        }
    }

    /// Attach a goal hint, commonly recovered from a nearby assistant message
    /// or tool-call log.
    pub fn with_goal_description(mut self, goal_description: impl Into<String>) -> Self {
        self.goal_description = Some(goal_description.into());
        self
    }
}

/// Options for deterministic intent clustering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentClusterOptions {
    /// Same-run atoms with event gaps at or below this value are merged.
    pub max_event_gap: u64,
    /// Maximum number of ambiguous same-run gaps that may invoke a semantic
    /// classifier.
    pub semantic_boundary_budget: usize,
}

impl Default for IntentClusterOptions {
    fn default() -> Self {
        Self {
            max_event_gap: DEFAULT_MAX_EVENT_GAP,
            semantic_boundary_budget: 0,
        }
    }
}

/// Pure default clusterer for flow intents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentClusterer {
    options: IntentClusterOptions,
}

impl IntentClusterer {
    pub fn new(options: IntentClusterOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &IntentClusterOptions {
        &self.options
    }

    /// Cluster observations without semantic classification.
    pub fn cluster<I>(&self, observations: I) -> Vec<Intent>
    where
        I: IntoIterator<Item = ObservedAtom>,
    {
        self.cluster_internal(observations, None)
    }

    /// Cluster observations, delegating ambiguous same-run gaps to `classifier`
    /// until `semantic_boundary_budget` is exhausted.
    pub fn cluster_with_classifier<I, C>(&self, observations: I, classifier: &mut C) -> Vec<Intent>
    where
        I: IntoIterator<Item = ObservedAtom>,
        C: IntentBoundaryClassifier,
    {
        self.cluster_internal(
            observations,
            Some(classifier as &mut (dyn IntentBoundaryClassifier + '_)),
        )
    }

    fn cluster_internal<I>(
        &self,
        observations: I,
        mut classifier: Option<&mut (dyn IntentBoundaryClassifier + '_)>,
    ) -> Vec<Intent>
    where
        I: IntoIterator<Item = ObservedAtom>,
    {
        let mut observations: Vec<ObservedAtom> = observations.into_iter().collect();
        observations.sort_by(|left, right| {
            left.transcript_ref
                .cmp(&right.transcript_ref)
                .then_with(|| left.agent_run_id.cmp(&right.agent_run_id))
                .then_with(|| {
                    left.transcript_event_index
                        .cmp(&right.transcript_event_index)
                })
                .then_with(|| left.atom_id.0.cmp(&right.atom_id.0))
        });

        let mut builder: Option<IntentBuilder> = None;
        let mut intents = Vec::new();
        let mut semantic_budget_remaining = self.options.semantic_boundary_budget;

        for observation in observations {
            if builder.is_none() {
                builder = Some(IntentBuilder::new(observation));
                continue;
            }

            let decision = {
                let active = builder.as_ref().expect("active builder has observations");
                let previous = active.last().expect("active builder has observations");
                self.boundary_decision(
                    previous,
                    &observation,
                    classifier.as_deref_mut(),
                    &mut semantic_budget_remaining,
                )
            };

            match decision {
                BoundaryDecision::Merge { confidence } => builder
                    .as_mut()
                    .expect("active builder has observations")
                    .push(observation, confidence),
                BoundaryDecision::Split => {
                    intents.push(
                        builder
                            .take()
                            .expect("active builder has observations")
                            .finish(),
                    );
                    builder = Some(IntentBuilder::new(observation));
                }
            }
        }

        if let Some(active) = builder {
            intents.push(active.finish());
        }

        intents
    }

    fn boundary_decision(
        &self,
        previous: &ObservedAtom,
        next: &ObservedAtom,
        classifier: Option<&mut (dyn IntentBoundaryClassifier + '_)>,
        semantic_budget_remaining: &mut usize,
    ) -> BoundaryDecision {
        if previous.agent_run_id != next.agent_run_id
            || previous.transcript_ref != next.transcript_ref
        {
            return BoundaryDecision::Split;
        }

        let gap = next
            .transcript_event_index
            .saturating_sub(previous.transcript_event_index);
        if gap <= self.options.max_event_gap {
            return BoundaryDecision::Merge {
                confidence: DEFAULT_CONFIDENCE,
            };
        }

        let Some(classifier) = classifier else {
            return BoundaryDecision::Split;
        };
        if *semantic_budget_remaining == 0 {
            return BoundaryDecision::Split;
        }

        *semantic_budget_remaining -= 1;
        let dispute = IntentBoundaryDispute {
            previous,
            next,
            gap,
        };
        match classifier.classify(&dispute) {
            IntentBoundaryDecision::Merge => BoundaryDecision::Merge {
                confidence: SEMANTIC_CONFIDENCE,
            },
            IntentBoundaryDecision::Split => BoundaryDecision::Split,
        }
    }
}

impl Default for IntentClusterer {
    fn default() -> Self {
        Self::new(IntentClusterOptions::default())
    }
}

/// Boundary dispute passed to optional semantic classifiers.
#[derive(Clone, Copy, Debug)]
pub struct IntentBoundaryDispute<'a> {
    pub previous: &'a ObservedAtom,
    pub next: &'a ObservedAtom,
    pub gap: u64,
}

/// Semantic classifier verdict for an ambiguous boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentBoundaryDecision {
    Merge,
    Split,
}

/// Optional hook for semantic boundary classification.
pub trait IntentBoundaryClassifier {
    fn classify(&mut self, dispute: &IntentBoundaryDispute<'_>) -> IntentBoundaryDecision;
}

enum BoundaryDecision {
    Merge { confidence: f32 },
    Split,
}

struct IntentBuilder {
    observations: Vec<ObservedAtom>,
    span: TranscriptSpan,
    confidence: f32,
}

impl IntentBuilder {
    fn new(observation: ObservedAtom) -> Self {
        let span = TranscriptSpan {
            transcript_ref: observation.transcript_ref.clone(),
            start_event_index: observation.transcript_event_index,
            end_event_index: observation.transcript_event_index,
        };
        Self {
            observations: vec![observation],
            span,
            confidence: DEFAULT_CONFIDENCE,
        }
    }

    fn last(&self) -> Option<&ObservedAtom> {
        self.observations.last()
    }

    fn push(&mut self, observation: ObservedAtom, confidence: f32) {
        self.span.extend_to(observation.transcript_event_index);
        self.confidence = self.confidence.min(confidence);
        self.observations.push(observation);
    }

    fn finish(self) -> Intent {
        let atoms: Vec<AtomId> = self
            .observations
            .iter()
            .map(|observation| observation.atom_id)
            .collect();
        Intent::new(
            atoms,
            goal_description(&self.observations, &self.span),
            self.span,
            self.confidence,
        )
        .expect("builder always contains at least one observation with valid confidence")
    }
}

fn goal_description(observations: &[ObservedAtom], span: &TranscriptSpan) -> String {
    let mut goals = BTreeSet::new();
    for observation in observations {
        if let Some(goal) = observation
            .goal_description
            .as_deref()
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
        {
            goals.insert(goal.to_string());
        }
    }

    if !goals.is_empty() {
        return goals.into_iter().collect::<Vec<_>>().join("; ");
    }

    let first = observations
        .first()
        .expect("goal_description requires observations");
    let tool_calls: BTreeSet<&str> = observations
        .iter()
        .filter_map(|observation| observation.tool_call_id.as_deref())
        .collect();
    if tool_calls.len() == 1 {
        return format!(
            "tool call {} in {} events {}..{}",
            tool_calls.iter().next().unwrap(),
            span.transcript_ref,
            span.start_event_index,
            span.end_event_index
        );
    }

    format!(
        "agent run {} in {} events {}..{}",
        first.agent_run_id, span.transcript_ref, span.start_event_index, span.end_event_index
    )
}

fn derive_intent_id(
    atoms: &[AtomId],
    goal_description: &str,
    origin_transcript_span: &TranscriptSpan,
) -> IntentId {
    let mut hasher = Sha256::new();
    hasher.update(b"FINT");
    hasher.update(origin_transcript_span.transcript_ref.as_bytes());
    hasher.update(origin_transcript_span.start_event_index.to_le_bytes());
    hasher.update(origin_transcript_span.end_event_index.to_le_bytes());
    hasher.update(goal_description.as_bytes());
    for atom in atoms {
        hasher.update(atom.0);
    }
    IntentId(hasher.finalize().into())
}

fn normalize_confidence(confidence: f32) -> Result<f32, IntentError> {
    if !confidence.is_finite() {
        return Err(IntentError::Invalid(
            "confidence must be a finite number".to_string(),
        ));
    }
    if !(0.0..=1.0).contains(&confidence) {
        return Err(IntentError::Invalid(format!(
            "confidence must be between 0.0 and 1.0, got {confidence}"
        )));
    }
    Ok(confidence)
}

impl From<AtomError> for IntentError {
    fn from(error: AtomError) -> Self {
        IntentError::Invalid(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    fn deterministic_signing_key(seed: u8) -> SigningKey {
        let mut bytes = [0u8; 32];
        for slot in bytes.iter_mut() {
            *slot = seed;
        }
        SigningKey::from_bytes(&bytes)
    }

    fn atom(suffix: &str, run_id: &str, transcript_ref: &str, tool_call_id: Option<&str>) -> Atom {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let mut provenance = crate::flow::Provenance {
            principal: "user:alice".to_string(),
            persona: "ship-captain".to_string(),
            agent_run_id: run_id.to_string(),
            tool_call_id: tool_call_id.map(ToString::to_string),
            trace_id: format!("trace-{suffix}"),
            transcript_ref: transcript_ref.to_string(),
            timestamp: OffsetDateTime::parse("2026-04-24T12:34:56Z", &Rfc3339).unwrap(),
        };
        provenance.timestamp += time::Duration::seconds(suffix.len() as i64);
        Atom::sign(
            vec![crate::flow::TextOp::Insert {
                offset: suffix.len() as u64,
                content: suffix.to_string(),
            }],
            Vec::new(),
            provenance,
            None,
            &principal,
            &persona,
        )
        .unwrap()
    }

    fn observed(
        suffix: &str,
        run_id: &str,
        transcript_ref: &str,
        event_index: u64,
        tool_call_id: Option<&str>,
        goal: Option<&str>,
    ) -> ObservedAtom {
        let atom = atom(suffix, run_id, transcript_ref, tool_call_id);
        let observed = ObservedAtom::from_atom(&atom, event_index);
        match goal {
            Some(goal) => observed.with_goal_description(goal),
            None => observed,
        }
    }

    #[test]
    fn default_clustering_groups_same_run_atoms_with_close_transcript_events() {
        let observations = vec![
            observed(
                "a",
                "run-1",
                "transcript:1",
                10,
                Some("tc-1"),
                Some("edit README"),
            ),
            observed(
                "b",
                "run-1",
                "transcript:1",
                13,
                Some("tc-2"),
                Some("edit README"),
            ),
            observed(
                "c",
                "run-1",
                "transcript:1",
                40,
                Some("tc-3"),
                Some("add tests"),
            ),
        ];

        let intents = IntentClusterer::default().cluster(observations);

        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].atoms.len(), 2);
        assert_eq!(intents[0].origin_transcript_span.start_event_index, 10);
        assert_eq!(intents[0].origin_transcript_span.end_event_index, 13);
        assert_eq!(intents[0].goal_description, "edit README");
        assert_eq!(intents[1].atoms.len(), 1);
        assert_eq!(intents[1].goal_description, "add tests");
    }

    #[test]
    fn clustering_respects_agent_run_and_transcript_boundaries() {
        let observations = vec![
            observed("a", "run-1", "transcript:1", 10, None, None),
            observed("b", "run-2", "transcript:1", 11, None, None),
            observed("c", "run-1", "transcript:2", 12, None, None),
        ];

        let intents = IntentClusterer::default().cluster(observations);

        assert_eq!(intents.len(), 3);
        assert!(intents
            .iter()
            .all(|intent| intent.atoms.len() == 1 && intent.confidence == DEFAULT_CONFIDENCE));
    }

    #[test]
    fn clustering_is_stable_for_unsorted_transcript_tool_logs() {
        let a = observed("a", "run-1", "transcript:1", 2, Some("tc-1"), None);
        let b = observed("b", "run-1", "transcript:1", 1, Some("tc-1"), None);

        let intents = IntentClusterer::default().cluster(vec![a.clone(), b.clone()]);

        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].atoms, vec![b.atom_id, a.atom_id]);
        assert_eq!(
            intents[0].goal_description,
            "tool call tc-1 in transcript:1 events 1..2"
        );
    }

    #[test]
    fn semantic_classifier_can_merge_budgeted_boundary_disputes() {
        #[derive(Default)]
        struct MergeOnce {
            calls: usize,
        }
        impl IntentBoundaryClassifier for MergeOnce {
            fn classify(&mut self, dispute: &IntentBoundaryDispute<'_>) -> IntentBoundaryDecision {
                self.calls += 1;
                assert_eq!(dispute.previous.agent_run_id, "run-1");
                assert_eq!(dispute.next.agent_run_id, "run-1");
                assert_eq!(dispute.gap, 20);
                IntentBoundaryDecision::Merge
            }
        }

        let clusterer = IntentClusterer::new(IntentClusterOptions {
            max_event_gap: 5,
            semantic_boundary_budget: 1,
        });
        let observations = vec![
            observed("a", "run-1", "transcript:1", 0, None, Some("rename API")),
            observed("b", "run-1", "transcript:1", 20, None, Some("rename API")),
            observed("c", "run-1", "transcript:1", 40, None, Some("rename API")),
        ];
        let mut classifier = MergeOnce::default();

        let intents = clusterer.cluster_with_classifier(observations, &mut classifier);

        assert_eq!(classifier.calls, 1);
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].atoms.len(), 2);
        assert_eq!(intents[0].confidence, DEFAULT_CONFIDENCE);
        assert_eq!(intents[1].atoms.len(), 1);
    }

    #[test]
    fn semantic_classifier_never_crosses_hard_boundaries() {
        #[derive(Default)]
        struct NeverCalled;
        impl IntentBoundaryClassifier for NeverCalled {
            fn classify(&mut self, _: &IntentBoundaryDispute<'_>) -> IntentBoundaryDecision {
                panic!("hard agent/transcript boundaries must not invoke semantic classifier");
            }
        }

        let clusterer = IntentClusterer::new(IntentClusterOptions {
            max_event_gap: 0,
            semantic_boundary_budget: 10,
        });
        let observations = vec![
            observed("a", "run-1", "transcript:1", 0, None, None),
            observed("b", "run-2", "transcript:1", 1, None, None),
            observed("c", "run-1", "transcript:2", 2, None, None),
        ];
        let mut classifier = NeverCalled;

        let intents = clusterer.cluster_with_classifier(observations, &mut classifier);

        assert_eq!(intents.len(), 3);
    }

    #[test]
    fn sealing_captures_current_atom_set() {
        let observations = vec![
            observed("a", "run-1", "transcript:1", 0, None, Some("ship feature")),
            observed("b", "run-1", "transcript:1", 1, None, Some("ship feature")),
        ];
        let mut intent = IntentClusterer::default()
            .cluster(observations)
            .pop()
            .expect("one intent");

        let sealed = intent.seal();
        intent.atoms.pop();

        assert_eq!(sealed.atoms.len(), 2);
        assert_eq!(intent.atoms.len(), 1);
        assert_eq!(sealed.goal_description, "ship feature");
    }

    #[test]
    fn intent_id_round_trips_through_json() {
        let observations = vec![observed(
            "a",
            "run-1",
            "transcript:1",
            0,
            None,
            Some("ship feature"),
        )];
        let intent = IntentClusterer::default()
            .cluster(observations)
            .pop()
            .expect("one intent");

        let raw = serde_json::to_string(&intent).unwrap();
        let decoded: Intent = serde_json::from_str(&raw).unwrap();

        assert_eq!(decoded, intent);
        assert_eq!(IntentId::from_hex(&intent.id.to_hex()).unwrap(), intent.id);
    }
}
