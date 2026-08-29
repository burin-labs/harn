//! Who answers an `ask`.
//!
//! [`PolicyAction`](super::PolicyAction) is a per-rule VERDICT: allow, ask, or
//! deny. It is ranked, and that rank drives `intersect`'s most-restrictive-wins
//! composition across the graph, node, and host policy stack. The resolver is a
//! different question entirely — not "what does the rule say" but "who answers
//! when the rule says ask" — and it deliberately does not live in that enum.
//!
//! Ranking `allow_all` alongside `Allow` would force an answer to "is
//! `allow_all` more or less restrictive than `Allow`?", which has none, and
//! would silently change how every nested policy intersection composes. So the
//! two axes stay orthogonal, the same split Codex draws between
//! `AskForApproval` and `ApprovalsReviewer`.
//!
//! # Why this exists
//!
//! A non-interactive run has nobody to ask. Until now the only honest answer
//! was to refuse, and [`RunApprovalPolicy`](super::RunApprovalPolicy) collapsed
//! every `Ask` to `Deny` before the run started. That is correct when the only
//! possible answerer is a person. It stops being correct the moment a resolver
//! can answer, which is what makes the collapse conditional rather than
//! unconditional — see `RunApprovalPolicy::construct_with_resolver`.
//!
//! # The rung that is not a rung
//!
//! [`ApprovalResolver::AllowAll`] answers every ask yes. It does **not** lift
//! the catastrophic floor. `universal_catastrophic_reason` refuses `rm -rf /`
//! and its siblings regardless of who is asking or what they answered, and no
//! resolver may overrule it. A "yolo" mode that could disable that floor would
//! not be a faster mode, it would be a different product.

use serde::{Deserialize, Serialize};

/// Who answers a permission `ask` for this run.
///
/// Ordered weakest-authority first. The ordering is documentation, not a
/// lattice: unlike [`PolicyAction`](super::PolicyAction) this type is never
/// intersected, because a resolver is host authority and a nested scope may not
/// widen it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolver {
    /// Ask the host, which asks a person. The historical behavior, and still
    /// the default: a surface that cannot reach a person reports the ask as
    /// unsatisfiable rather than answering on their behalf.
    #[default]
    Host,
    /// Route the ask to an adversarial reviewer that sees the request, why it
    /// was refused, and the session goal, and answers approve or deny with a
    /// one-line reason.
    AutoReview,
    /// Answer every ask yes. Still subject to the catastrophic floor.
    AllowAll,
}

impl ApprovalResolver {
    pub const ALL: [Self; 3] = [Self::Host, Self::AutoReview, Self::AllowAll];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::AutoReview => "auto_review",
            Self::AllowAll => "allow_all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "host" | "ask" | "person" => Some(Self::Host),
            "auto_review" | "auto-review" | "review" => Some(Self::AutoReview),
            // The surface spellings Burin's `/approve` already accepts. They
            // arrive here rather than staying a Burin-local empty policy
            // overlay, so "yolo" has ONE owner and one meaning.
            "allow_all" | "allow-all" | "yolo" | "full_auto" | "full-auto" => Some(Self::AllowAll),
            _ => None,
        }
    }

    /// Whether this resolver can answer an `ask` without a person present.
    ///
    /// This is the predicate that makes a non-interactive run's `Ask` rules
    /// survivable. `Host` cannot: with no person and no bridge, an ask it
    /// receives has no answer, and pretending otherwise silently grants exactly
    /// the calls a rule singled out for review.
    pub fn answers_ask_without_a_person(self) -> bool {
        matches!(self, Self::AutoReview | Self::AllowAll)
    }

    /// Whether this resolver requires a reviewer model to be configured.
    pub fn requires_reviewer(self) -> bool {
        matches!(self, Self::AutoReview)
    }
}

/// Schema of the once-per-session receipt naming the resolver a run installed.
pub const APPROVAL_RESOLVER_RECEIPT_SCHEMA: &str = "harn.approval_resolver.v1";

/// The receipt a run emits once, naming the resolver it actually installed
/// beside the one it was asked for.
///
/// Both fields, deliberately. The resolver is typed pipeline input, and a host
/// that silently fell back to [`ApprovalResolver::Host`] while the caller asked
/// for `AutoReview` would otherwise publish a clean zero fallback count that
/// reads as "the reviewer was never needed" rather than "the reviewer was never
/// installed". Consumers assert `resolved == requested`; the receipt is what
/// gives them something to assert against.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResolverReceipt {
    pub schema: String,
    pub resolver: ApprovalResolver,
    pub requested: ApprovalResolver,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_model: Option<String>,
}

impl ApprovalResolverReceipt {
    pub fn new(
        resolved: ApprovalResolver,
        requested: ApprovalResolver,
        reviewer_model: Option<String>,
    ) -> Self {
        Self {
            schema: APPROVAL_RESOLVER_RECEIPT_SCHEMA.to_string(),
            resolver: resolved,
            requested,
            reviewer_model,
        }
    }

    /// True when the run installed the resolver it was asked for.
    pub fn matches_request(&self) -> bool {
        self.resolver == self.requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cannot_answer_without_a_person() {
        // The load-bearing negative. If this ever returns true, every `Ask`
        // rule on a headless run is silently granted -- which is the exact
        // regression bc#6524 fixed on the Burin side.
        assert!(!ApprovalResolver::Host.answers_ask_without_a_person());
    }

    #[test]
    fn review_and_allow_all_answer_without_a_person() {
        assert!(ApprovalResolver::AutoReview.answers_ask_without_a_person());
        assert!(ApprovalResolver::AllowAll.answers_ask_without_a_person());
    }

    #[test]
    fn default_is_host() {
        // A resolver that defaulted to anything else would turn every existing
        // caller of the non-resolver constructor into an auto-approver.
        assert_eq!(ApprovalResolver::default(), ApprovalResolver::Host);
    }

    #[test]
    fn burin_yolo_spellings_resolve_to_allow_all() {
        for spelling in ["allow_all", "allow-all", "yolo", "full_auto", "full-auto"] {
            assert_eq!(
                ApprovalResolver::parse(spelling),
                Some(ApprovalResolver::AllowAll),
                "{spelling} must reach the one owner of allow-all"
            );
        }
        assert_eq!(ApprovalResolver::parse("nonsense"), None);
    }

    #[test]
    fn only_auto_review_needs_a_reviewer_model() {
        assert!(ApprovalResolver::AutoReview.requires_reviewer());
        assert!(!ApprovalResolver::AllowAll.requires_reviewer());
        assert!(!ApprovalResolver::Host.requires_reviewer());
    }

    #[test]
    fn receipt_reports_a_silent_fallback_as_a_mismatch() {
        let honest = ApprovalResolverReceipt::new(
            ApprovalResolver::AutoReview,
            ApprovalResolver::AutoReview,
            Some("claude-haiku-4-5-20251001".to_string()),
        );
        assert!(honest.matches_request());

        let fell_back = ApprovalResolverReceipt::new(
            ApprovalResolver::Host,
            ApprovalResolver::AutoReview,
            None,
        );
        assert!(!fell_back.matches_request());
    }

    #[test]
    fn receipt_serializes_with_the_schema_consumers_match_on() {
        let receipt = ApprovalResolverReceipt::new(
            ApprovalResolver::AllowAll,
            ApprovalResolver::AllowAll,
            None,
        );
        let json = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(json["schema"], APPROVAL_RESOLVER_RECEIPT_SCHEMA);
        assert_eq!(json["resolver"], "allow_all");
        assert_eq!(json["requested"], "allow_all");
        // Absent, not null: the eval reader treats a missing reviewer model as
        // "no reviewer", and a null would serialize into the artifact as a
        // measured absence of one.
        assert!(json.get("reviewer_model").is_none());
    }
}
