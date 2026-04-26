//! Graded `InvariantResult` and surrounding evidence/remediation machinery.
//!
//! See issue #581 for the type contract. Predicates produce an
//! [`InvariantResult`] whose `verdict` may `Allow`, `Warn`, `Block`, or
//! `RequireApproval` (routing to a specific principal or role). Structured
//! evidence pointers (`AtomPointer`, `MetadataPath`, `TranscriptExcerpt`,
//! `ExternalCitation`) explain the verdict; an optional [`Remediation`] is
//! consumed by the Fixer persona (#587), never auto-applied by the executor.
//!
//! The Rust types are kept faithful to the spec. The Harn-side stdlib
//! constructors live in `crates/harn-vm/src/stdlib/flow.rs` and produce values
//! that round-trip through [`InvariantResult::to_vm_value`] /
//! [`InvariantResult::from_vm_value`].

use serde::{Deserialize, Serialize};

use crate::flow::{Atom, AtomId};
use crate::value::VmValue;

/// Predicate result returned by every invariant evaluation attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvariantResult {
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<Remediation>,
    pub confidence: f64,
}

impl Eq for InvariantResult {}

impl InvariantResult {
    /// Allow verdict with full confidence, no evidence, no remediation.
    pub fn allow() -> Self {
        Self {
            verdict: Verdict::Allow,
            evidence: Vec::new(),
            remediation: None,
            confidence: 1.0,
        }
    }

    /// Warn verdict — predicate has concerns but does not block shipment.
    pub fn warn(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Warn {
                reason: reason.into(),
            },
            evidence: Vec::new(),
            remediation: None,
            confidence: 1.0,
        }
    }

    /// Block verdict — predicate refuses to ship the slice as-is.
    pub fn block(error: InvariantBlockError) -> Self {
        Self {
            verdict: Verdict::Block { error },
            evidence: Vec::new(),
            remediation: None,
            confidence: 1.0,
        }
    }

    /// `RequireApproval` verdict — predicate wants a specific principal or
    /// role to co-sign before shipment.
    pub fn require_approval(approver: Approver) -> Self {
        Self {
            verdict: Verdict::RequireApproval { approver },
            evidence: Vec::new(),
            remediation: None,
            confidence: 1.0,
        }
    }

    /// Attach evidence items to this result (replaces any existing evidence).
    pub fn with_evidence(mut self, evidence: Vec<EvidenceItem>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Attach a remediation suggestion to this result.
    pub fn with_remediation(mut self, remediation: Remediation) -> Self {
        self.remediation = Some(remediation);
        self
    }

    /// Override the confidence scalar (clamped to `[0.0, 1.0]`).
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// Returns true when the verdict halts shipment.
    pub fn is_blocking(&self) -> bool {
        matches!(self.verdict, Verdict::Block { .. })
    }

    /// Returns true when the verdict requires explicit cosigner routing.
    pub fn requires_approval(&self) -> bool {
        matches!(self.verdict, Verdict::RequireApproval { .. })
    }

    /// Return the underlying [`InvariantBlockError`] when the verdict is
    /// `Block`. Convenience for migration sites that previously matched on the
    /// old `InvariantResult::Blocked { error }` variant.
    pub fn block_error(&self) -> Option<&InvariantBlockError> {
        match &self.verdict {
            Verdict::Block { error } => Some(error),
            _ => None,
        }
    }

    /// Encode this result as a Harn-visible [`VmValue`]. Used by the stdlib
    /// flow builtins so predicate authors receive idiomatic record values.
    pub fn to_vm_value(&self) -> VmValue {
        let json = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        crate::stdlib::json_to_vm_value(&json)
    }

    /// Decode an `InvariantResult` from a [`VmValue`] produced by the stdlib
    /// flow builders (or by any other path that emits a structurally valid
    /// dict). Returns `Err` with a human-readable message on shape mismatch.
    pub fn from_vm_value(value: &VmValue) -> Result<Self, String> {
        let json = vm_value_to_json(value);
        serde_json::from_value(json).map_err(|error| format!("invalid InvariantResult: {error}"))
    }
}

fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::Value::from(*n),
        VmValue::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(map) => {
            let mut object = serde_json::Map::new();
            for (key, item) in map.iter() {
                object.insert(key.clone(), vm_value_to_json(item));
            }
            serde_json::Value::Object(object)
        }
        other => serde_json::Value::String(other.display()),
    }
}

/// Graded predicate verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// Predicate passed.
    Allow,
    /// Predicate raised concerns but does not block shipment.
    Warn { reason: String },
    /// Predicate refuses to ship the slice as-is.
    Block { error: InvariantBlockError },
    /// Predicate wants a specific principal or role to co-sign before
    /// shipment. The critical mechanism for "predicate says fine but wants a
    /// co-signer".
    RequireApproval { approver: Approver },
}

/// Routing target for a `RequireApproval` verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Approver {
    /// A specific human or system principal (e.g. `user:alice`).
    Principal { id: String },
    /// Any holder of the named role (e.g. `role:security-reviewer`).
    Role { name: String },
}

impl Approver {
    pub fn principal(id: impl Into<String>) -> Self {
        Self::Principal { id: id.into() }
    }

    pub fn role(name: impl Into<String>) -> Self {
        Self::Role { name: name.into() }
    }
}

/// Structured hard-block reason carried by `Verdict::Block`. Code is a stable
/// machine identifier; message is operator-facing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantBlockError {
    pub code: String,
    pub message: String,
}

impl InvariantBlockError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn budget_exceeded(message: impl Into<String>) -> Self {
        Self::new("budget_exceeded", message)
    }

    pub fn nondeterministic_drift(message: impl Into<String>) -> Self {
        Self::new("nondeterministic_drift", message)
    }
}

/// Pointer to a piece of evidence justifying the verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceItem {
    /// A specific atom and a byte span within its diff.
    AtomPointer { atom: AtomId, diff_span: ByteSpan },
    /// A path through the hierarchical `DirectoryMetadata` tree.
    MetadataPath {
        directory: String,
        namespace: String,
        key: String,
    },
    /// A range within a transcript captured during the run that produced the
    /// slice.
    TranscriptExcerpt {
        transcript_id: String,
        span: ByteSpan,
    },
    /// An external citation fetched by an Archivist-authored predicate.
    ExternalCitation {
        url: String,
        quote: String,
        /// RFC3339 timestamp captured at fetch time.
        fetched_at: String,
    },
}

/// Inclusive-start, exclusive-end byte span. Used by both atom diff pointers
/// and transcript excerpts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: u64,
    pub end: u64,
}

impl ByteSpan {
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }
}

/// Inert remediation suggestion. Consumed by the Fixer persona (#587), never
/// auto-applied by the executor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_atoms: Option<Vec<Atom>>,
}

impl Remediation {
    pub fn describe(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            suggested_atoms: None,
        }
    }

    pub fn with_suggested_atoms(mut self, atoms: Vec<Atom>) -> Self {
        self.suggested_atoms = Some(atoms);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_round_trips_through_json_and_vm_value() {
        let original = InvariantResult::allow();
        let json = serde_json::to_value(&original).unwrap();
        let decoded: InvariantResult = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, original);

        let vm_value = original.to_vm_value();
        let from_vm = InvariantResult::from_vm_value(&vm_value).unwrap();
        assert_eq!(from_vm, original);
    }

    #[test]
    fn warn_carries_reason() {
        let result = InvariantResult::warn("unused import in stdlib");
        assert!(
            matches!(result.verdict, Verdict::Warn { ref reason } if reason == "unused import in stdlib")
        );
        assert!(!result.is_blocking());
    }

    #[test]
    fn block_marks_blocking() {
        let result = InvariantResult::block(InvariantBlockError::new(
            "missing_test",
            "no test covers this atom",
        ));
        assert!(result.is_blocking());
        assert_eq!(result.block_error().unwrap().code, "missing_test");
    }

    #[test]
    fn require_approval_routes_to_principal_or_role() {
        let principal = InvariantResult::require_approval(Approver::principal("user:alice"));
        let role = InvariantResult::require_approval(Approver::role("security-reviewer"));
        assert!(principal.requires_approval());
        assert!(role.requires_approval());
        match principal.verdict {
            Verdict::RequireApproval {
                approver: Approver::Principal { id },
            } => {
                assert_eq!(id, "user:alice");
            }
            other => panic!("expected principal approver, got {other:?}"),
        }
        match role.verdict {
            Verdict::RequireApproval {
                approver: Approver::Role { name },
            } => {
                assert_eq!(name, "security-reviewer");
            }
            other => panic!("expected role approver, got {other:?}"),
        }
    }

    #[test]
    fn confidence_clamps_to_unit_interval() {
        let low = InvariantResult::warn("low signal").with_confidence(-0.5);
        let high = InvariantResult::warn("over-confident").with_confidence(2.0);
        let mid = InvariantResult::warn("calibrated").with_confidence(0.42);
        assert_eq!(low.confidence, 0.0);
        assert_eq!(high.confidence, 1.0);
        assert!((mid.confidence - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn evidence_items_serialize_with_kind_tag() {
        let evidence = vec![
            EvidenceItem::AtomPointer {
                atom: AtomId([1; 32]),
                diff_span: ByteSpan::new(0, 64),
            },
            EvidenceItem::MetadataPath {
                directory: "src/auth".to_string(),
                namespace: "policy".to_string(),
                key: "min_review_count".to_string(),
            },
            EvidenceItem::TranscriptExcerpt {
                transcript_id: "transcript-0001".to_string(),
                span: ByteSpan::new(128, 256),
            },
            EvidenceItem::ExternalCitation {
                url: "https://harnlang.com/spec".to_string(),
                quote: "verdicts may grade as Allow, Warn, Block, RequireApproval".to_string(),
                fetched_at: "2026-04-26T00:00:00Z".to_string(),
            },
        ];
        let result = InvariantResult::warn("see evidence").with_evidence(evidence.clone());
        let json = serde_json::to_value(&result).unwrap();
        let decoded: InvariantResult = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.evidence, evidence);
    }

    #[test]
    fn remediation_attaches_without_suggested_atoms() {
        let result =
            InvariantResult::block(InvariantBlockError::new("style", "trailing whitespace"))
                .with_remediation(Remediation::describe("strip trailing whitespace"));
        assert_eq!(
            result.remediation.as_ref().unwrap().description,
            "strip trailing whitespace"
        );
        assert!(result
            .remediation
            .as_ref()
            .unwrap()
            .suggested_atoms
            .is_none());
    }
}
