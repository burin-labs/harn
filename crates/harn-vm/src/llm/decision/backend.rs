//! The seam a decision transport plugs into.
//!
//! One method, one physical request, no retries. A backend converts a request
//! into raw per-question distributions and reports what it cost. It never
//! decides an outcome, never repairs a bad answer, and never makes a second
//! call to fill in something the first did not return: the evaluator owns
//! every one of those decisions so both backends cannot drift apart.

use std::collections::BTreeMap;

use super::contract::DecisionContract;
use super::question::QuestionSet;

/// What the evaluator asks a backend for. `state` is the caller's frozen input
/// already canonicalized to JSON; `questions` is the admitted set in declared
/// order.
#[derive(Clone, Debug)]
pub struct DecisionRequest<'a> {
    pub model: &'a str,
    pub provider: &'a str,
    pub state: &'a serde_json::Value,
    pub questions: &'a QuestionSet,
    /// The route's declared decision facts. The structured projection does not
    /// read them; the native adapter (#8538) dispatches on the protocol.
    #[allow(dead_code)]
    pub contract: &'a DecisionContract,
    /// Explicit, and exactly what the policy admitted. A backend that cannot
    /// honor both returns `UnsupportedOptions` rather than dropping one.
    pub effort: &'a str,
    pub temperature: f64,
    pub evaluation_cost_limit: Option<f64>,
    pub run_cost_limit: Option<f64>,
}

/// Where a backend's numbers come from. This is a property of the transport,
/// not of one answer, and it decides every answer's `confidence_kind`. A
/// decision model measures a distribution; a chat model reports a number about
/// itself. Presenting those as one comparable score is the mistake this field
/// exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfidenceProvenance {
    /// The backend measured a distribution. A boolean's confidence is then
    /// derived as `max(p, 1 - p)`; a choice or score carries the vendor's
    /// summary of the distribution's shape. Constructed by the native adapter
    /// (#8538) and by the tests that pin the conversion.
    #[allow(dead_code)]
    VendorDistribution,
    /// The model reported a number about its own answer, alongside prose it
    /// generated. Neither is a measurement.
    ModelReported,
}

/// One question's raw answer, before any conversion to a typed answer.
///
/// A boolean carries one probability. A choice or score carries a
/// distribution over exactly the question's declared labels.
#[derive(Clone, Debug, PartialEq)]
pub enum RawAnswer {
    /// A structured model names an answer and separately estimates its
    /// confidence. The name must not be reconstructed from that estimate.
    ModelReported {
        selection: ReportedSelection,
        confidence: f64,
        evidence: Option<String>,
    },
    Boolean {
        probability: f64,
        /// Present only when the backend reports its own number.
        reported_confidence: Option<f64>,
        evidence: Option<String>,
    },
    Choice {
        /// If the vendor also names a label, it must agree with projection.
        selected: Option<String>,
        probabilities: BTreeMap<String, f64>,
        reported_confidence: Option<f64>,
        evidence: Option<String>,
    },
    Score {
        probabilities: BTreeMap<String, f64>,
        score: Option<f64>,
        reported_confidence: Option<f64>,
        evidence: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReportedSelection {
    Boolean(bool),
    Choice(String),
    Score(String),
}

/// What a backend returns. `answers` is keyed by question id and is not
/// required to be complete: the evaluator refuses a partial set rather than
/// projecting a smaller `answered`.
#[derive(Clone, Debug, PartialEq)]
pub struct RawDecisionResponse {
    /// The structured transport's authoritative settlement, including cache
    /// visibility. Native billing has its separate declared flat-price owner.
    pub usage: Option<Box<crate::llm::usage::LlmUsage>>,
    pub native_transport: Option<super::receipt::NativeTransportReceipt>,
    pub answers: BTreeMap<String, RawAnswer>,
    pub provenance: ConfidenceProvenance,
    /// The identity the provider served, as returned. Not the requested id.
    pub served_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Physical requests this call actually made. A backend that reports
    /// anything but 1 for a completed response has broken the profile.
    pub physical_attempts: u32,
}

/// Why a dispatch produced no response. Each maps onto exactly one outcome
/// arm, so a transport can never collapse a rate limit into a generic failure.
#[derive(Clone, Debug, PartialEq)]
pub enum DecisionTransportError {
    /// A paid response can fail validation while retaining its settlement.
    Accounted {
        error: Box<DecisionTransportError>,
        usage: Box<crate::llm::usage::LlmUsage>,
        served_model: Option<String>,
    },
    /// Credential admission refused locally, before a physical request.
    AuthorityDenied,
    LocalAdmissionDenied {
        diagnostic: String,
    },
    /// The provider refused the request or returned an unusable body.
    Refused {
        reason: RefusalReason,
        diagnostic: String,
    },
    /// The provider says the state did not fit, after the local estimate said
    /// it would. Both numbers reach the receipt so the estimator can be tuned.
    StateTooLarge {
        provider_reason: String,
        limit_tokens: Option<usize>,
    },
    RateLimited {
        retry_after_ms: Option<u64>,
    },
    Overloaded,
    /// The route cannot honor the admitted options; nothing was dropped
    /// silently to make the call work.
    UnsupportedOptions {
        diagnostic: String,
    },
    TransportFailed {
        diagnostic: String,
    },
}

impl DecisionTransportError {
    pub fn with_usage(
        self,
        usage: crate::llm::usage::LlmUsage,
        served_model: Option<String>,
    ) -> Self {
        Self::Accounted {
            error: Box::new(self),
            usage: Box::new(usage),
            served_model,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    ProviderRefusal,
    SchemaInvalid,
    OutputTruncated,
}

impl RefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRefusal => "provider_refusal",
            Self::SchemaInvalid => "schema_invalid",
            Self::OutputTruncated => "output_truncated",
        }
    }
}

/// A decision transport. One evaluation calls this at most once.
#[async_trait::async_trait]
pub trait DecisionBackend: Send + Sync {
    async fn evaluate(
        &self,
        request: DecisionRequest<'_>,
    ) -> Result<RawDecisionResponse, DecisionTransportError>;
}
