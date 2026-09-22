//! What an evaluation leaves behind.
//!
//! A receipt proves what was evaluated, what came back, and what it consumed.
//! It does not certify that the model's evidence is true. Its cache identity
//! fields are the complete key the later cache and tape work keys off, so that
//! work reads the receipt rather than re-deriving the digest from source.

use serde::{Deserialize, Serialize};

use super::answer::Answer;
use super::question::QuestionSet;

pub const EVALUATION_RECEIPT_SCHEMA: &str = "harn.evaluation.receipt.v1";

/// Bounded execution-owned journal shared by a VM and its children.
#[derive(Default)]
pub(crate) struct EvaluationJournal {
    receipts: Vec<EvaluationReceipt>,
    dropped: usize,
}

impl EvaluationJournal {
    pub(crate) fn record(&mut self, receipt: EvaluationReceipt) {
        if self.receipts.len() < 1024 {
            self.receipts.push(receipt);
        } else {
            self.dropped += 1;
        }
    }

    pub(crate) fn snapshot(&self) -> (Vec<EvaluationReceipt>, usize) {
        (self.receipts.clone(), self.dropped)
    }
}

/// Requested routing restrictions are distinct from what the gateway reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTransportReceipt {
    pub data_controls: crate::llm::api::DataControlsReceipt,
    /// Gateway-reported downstream attempts, absent when unreported. This is
    /// independent of the observed outer HTTP dispatch count.
    pub provider_attempts_reported: Option<u64>,
    pub final_provider_reported: Option<String>,
}

/// Where the answers came from. `live` is the only source that makes a
/// provider request; the rest report zero attempts and keep the original
/// usage as provenance only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSource {
    Live,
    Cache,
    Tape,
    Fixture,
}

/// Whether the usage on this receipt is known. Unknown usage keeps its
/// reservation and is marked here; it is never recorded as free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingStatus {
    Settled,
    UsageUnknown,
    NotDispatched,
}

/// Monetary admission is distinct from the provider's eventual settled bill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostAdmission {
    AdaptiveProjection,
    ConservativeUpperBound,
}

/// The complete cache key, named field by field rather than pre-hashed, so a
/// consumer can see which facts a reuse decision rests on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationIdentity {
    /// Version of canonical request normalization and stable identity hashing.
    /// An absent stamp remains readable historical evidence, but is not eligible
    /// for verification under a later identity contract.
    #[serde(default)]
    pub contract_version: String,
    #[serde(default)]
    pub structured_output_strategy: Option<String>,
    pub input_digest: String,
    pub canonical_input_type: String,
    pub question_set_digest: String,
    pub policy_digest: String,
    pub evaluator_instruction_version: String,
    pub output_schema_version: String,
    pub backend_kind: String,
    pub protocol: String,
}

pub const EVALUATION_IDENTITY_CONTRACT: &str = "harn.evaluation_identity.v1";

/// One question's raw answer as the backend reported it, before conversion.
/// A calibration study reads these, not the derived confidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvaluationQuestionReceipt {
    pub id: String,
    pub kind: String,
    pub confidence_kind: Option<String>,
    pub raw_probabilities: std::collections::BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReceipt {
    /// Present for completed calls. Adaptive estimates can be exceeded by the
    /// actual bill; conservative admission reserves a supported upper bound.
    #[serde(default)]
    pub cost_admission: Option<CostAdmission>,
    /// Canonical observed call usage. Native unknown usage remains unpriced
    /// even when budget_charge_usd retains an admission reservation. Absent cache
    /// declarations never imply a measured native cache hit or miss.
    #[serde(default)]
    pub usage: Option<Box<crate::llm::usage::LlmUsage>>,
    pub native_transport: Option<NativeTransportReceipt>,
    pub schema: String,
    pub evaluation_id: String,
    pub site_id: String,
    pub identity: EvaluationIdentity,
    pub requested_provider: String,
    pub requested_model: String,
    /// The identity the provider served, exactly as it returned it. Absent
    /// when nothing was dispatched or the provider named nothing.
    pub served_model: Option<String>,
    pub questions: Vec<EvaluationQuestionReceipt>,
    pub outcome_kind: String,
    /// Observed client transport requests. Hidden gateway attempts are reported
    /// separately in native_transport. A local refusal records zero, and that zero
    /// is the claim the #8540 falsifiers check.
    pub physical_attempts: u32,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Settled measured cost. Absent when provider usage is unknown; retained
    /// admission reservations are reported separately, never as measured spend.
    pub cost_usd: Option<f64>,
    /// Amount charged to the native admission budget, including a retained
    /// upper bound when usage is unknown. Absent for historical receipts and
    /// paths that do not report this accounting fact.
    #[serde(default)]
    pub budget_charge_usd: Option<f64>,
    pub accounting_status: AccountingStatus,
    pub source: EvaluationSource,
    pub elapsed_ms: u64,
    /// Present when the provider explained a refusal the evaluator mapped onto
    /// an arm, such as the reported reason behind `state_too_large`.
    pub provider_reason: Option<String>,
    pub estimated_state_tokens: Option<usize>,
    pub estimated_longest_question_tokens: Option<usize>,
    pub estimated_request_tokens: Option<usize>,
    pub limit_tokens: Option<usize>,
}

impl EvaluationReceipt {
    /// A receipt for an evaluation that never dispatched. Every local refusal
    /// uses this, so a zero attempt count is written by one owner.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn not_dispatched(
        evaluation_id: String,
        site_id: String,
        identity: EvaluationIdentity,
        requested_provider: String,
        requested_model: String,
        questions: &QuestionSet,
        outcome_kind: &str,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            cost_admission: None,
            usage: None,
            native_transport: None,
            schema: EVALUATION_RECEIPT_SCHEMA.into(),
            evaluation_id,
            site_id,
            identity,
            requested_provider,
            requested_model,
            served_model: None,
            questions: questions
                .questions
                .iter()
                .map(|question| EvaluationQuestionReceipt {
                    id: question.id.clone(),
                    kind: question.body.kind().as_str().into(),
                    confidence_kind: None,
                    raw_probabilities: Default::default(),
                })
                .collect(),
            outcome_kind: outcome_kind.into(),
            physical_attempts: 0,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            budget_charge_usd: None,
            accounting_status: AccountingStatus::NotDispatched,
            source: EvaluationSource::Live,
            elapsed_ms,
            provider_reason: None,
            estimated_state_tokens: None,
            estimated_longest_question_tokens: None,
            estimated_request_tokens: None,
            limit_tokens: None,
        }
    }

    /// Replace the question census with the answers actually returned, keeping
    /// every declared question in place so an unanswered one stays visible.
    pub(crate) fn record_answers(&mut self, answers: &[Answer]) {
        for question in self.questions.iter_mut() {
            let Some(answer) = answers
                .iter()
                .find(|answer| answer.question_id == question.id)
            else {
                continue;
            };
            question.confidence_kind = Some(
                match answer.confidence_kind {
                    super::answer::ConfidenceKind::BinaryProbability => "binary_probability",
                    super::answer::ConfidenceKind::DistributionShape => "distribution_shape",
                    super::answer::ConfidenceKind::ModelRationale => "model_rationale",
                }
                .into(),
            );
            question.raw_probabilities = answer.raw_probabilities.clone();
        }
    }

    /// The opaque handle a caller receives. The journal owns the contents; a
    /// caller can correlate but cannot read the evaluation out of the string.
    pub fn reference(&self) -> String {
        self.evaluation_id.clone()
    }
}
