//! Exact evaluation reuse at the shared runtime boundary, before transport.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::testbench::tape::{EventTape, TapeHeader, TapeRecordKind, TapeRecorder};
use crate::VmError;

use super::answer::{Answer, AnswerBody, ConfidenceKind};
use super::backend::{ConfidenceProvenance, RawAnswer, ReportedSelection};
use super::identity::{verify_receipt, EvaluationRequest};
use super::outcome::Outcome;
use super::receipt::{AccountingStatus, EvaluationReceipt, EvaluationSource};
use super::{Evaluation, EvaluationPolicy};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recording {
    request: EvaluationRequest,
    outcome: serde_json::Value,
    answers: Vec<Answer>,
    receipt: EvaluationReceipt,
}

enum Mode {
    Record(TapeRecorder),
    Replay(VecDeque<Recording>),
    Fixtures(VecDeque<Recording>),
    Cache(BTreeMap<String, Recording>),
}

/// A test-owned answer is explicit about the selected arm and its claimed
/// confidence. It is projected through the same question validator as a model
/// report rather than being trusted as a finished outcome.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixtureAnswer {
    Boolean {
        verdict: bool,
        confidence: f64,
        evidence: String,
    },
    Choice {
        choice: String,
        confidence: f64,
        evidence: String,
    },
    Score {
        level: String,
        confidence: f64,
        evidence: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationFixture {
    pub site_id: String,
    pub state: Value,
    pub questions: Value,
    pub policy: Value,
    pub answers: BTreeMap<String, FixtureAnswer>,
}

/// An explicit scope. Replay never falls back to live transport; cache reuse
/// is opt-in, bounded to 1024 complete answers, and lasts only for this scope.
#[derive(Clone)]
pub struct EvaluationReplayScope(Arc<Mutex<Mode>>, Arc<Mutex<Option<String>>>);

impl EvaluationReplayScope {
    pub fn fixtures(fixtures: Vec<EvaluationFixture>) -> Result<Self, VmError> {
        let records = fixtures
            .into_iter()
            .map(fixture_recording)
            .collect::<Result<VecDeque<_>, _>>()?;
        Ok(Self(
            Arc::new(Mutex::new(Mode::Fixtures(records))),
            Arc::default(),
        ))
    }

    pub fn record() -> Self {
        Self(
            Arc::new(Mutex::new(Mode::Record(TapeRecorder::new()))),
            Arc::default(),
        )
    }

    pub fn cache() -> Self {
        Self(
            Arc::new(Mutex::new(Mode::Cache(BTreeMap::new()))),
            Arc::default(),
        )
    }

    pub fn replay(tape: &EventTape) -> Result<Self, VmError> {
        let mut records = VecDeque::new();
        for record in &tape.records {
            let TapeRecordKind::DecisionEvaluation {
                request_digest,
                response,
            } = &record.kind
            else {
                return Err(error("unexpected non-evaluation record"));
            };
            let bytes = tape.resolve_payload(response).map_err(error)?;
            if crate::testbench::tape::content_hash(&bytes) != response.content_hash() {
                return Err(error("record payload hash mismatch"));
            }
            let recorded: Recording =
                serde_json::from_slice(&bytes).map_err(|e| error(e.to_string()))?;
            if &recorded.receipt.evaluation_id != request_digest {
                return Err(error("record request digest mismatch"));
            }
            if recorded.receipt.source != EvaluationSource::Live {
                return Err(error("tape record must retain an original live evaluation"));
            }
            validate(&recorded)?;
            records.push_back(recorded);
        }
        Ok(Self(
            Arc::new(Mutex::new(Mode::Replay(records))),
            Arc::default(),
        ))
    }

    /// Extra records are an error, including a tape whose script made no calls.
    pub fn finish(&self) -> Result<Option<EventTape>, VmError> {
        if let Some(failure) = &*self.1.lock().expect("evaluation replay failure lock") {
            return Err(error(failure));
        }
        match &*self.0.lock().expect("evaluation replay lock") {
            Mode::Record(recorder) => Ok(Some(recorder.snapshot(TapeHeader::current(
                None,
                None,
                vec![],
            )))),
            Mode::Replay(records) | Mode::Fixtures(records) if !records.is_empty() => {
                Err(error(format!(
                    "{} unconsumed record(s); next site `{}`",
                    records.len(),
                    records[0].request.site_id,
                )))
            }
            _ => Ok(None),
        }
    }
}

fn error(message: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("evaluation replay mismatch: {message}"))
}

fn key(request: &EvaluationRequest) -> Result<String, VmError> {
    let (site, evaluation) = Evaluation::from_arguments(&request.arguments())?;
    let route = super::resolve_route(&evaluation.policy.provider, &evaluation.policy.model);
    let identity = super::identity::request_identity(
        &evaluation.state,
        &evaluation.questions,
        &evaluation.policy,
        route.as_ref(),
    );
    Ok(super::identity::request_id(&site, &identity))
}

fn fixture_recording(fixture: EvaluationFixture) -> Result<Recording, VmError> {
    let request = EvaluationRequest {
        site_id: fixture.site_id,
        state: fixture.state,
        questions: fixture.questions,
        policy: fixture.policy,
    };
    let (site, evaluation) = Evaluation::from_arguments(&request.arguments())?;
    let route = super::resolve_route(&evaluation.policy.provider, &evaluation.policy.model)
        .ok_or_else(|| {
            error(format!(
                "fixture site `{site}` has no decision-capable route"
            ))
        })?;
    evaluation.questions.admit(&route).map_err(|refusal| {
        error(format!(
            "fixture site `{site}` question `{}` is invalid: {}",
            refusal.question,
            refusal.reason.as_str()
        ))
    })?;
    if fixture.answers.len() != evaluation.questions.questions.len() {
        return Err(error(format!(
            "fixture site `{site}` declares {} answers for {} questions",
            fixture.answers.len(),
            evaluation.questions.questions.len()
        )));
    }
    let mut answers = Vec::with_capacity(fixture.answers.len());
    for question in &evaluation.questions.questions {
        let declared = fixture.answers.get(&question.id).ok_or_else(|| {
            error(format!(
                "fixture site `{site}` has no answer for question `{}`",
                question.id
            ))
        })?;
        let (selection, confidence, evidence) = match declared {
            FixtureAnswer::Boolean {
                verdict,
                confidence,
                evidence,
            } => (ReportedSelection::Boolean(*verdict), *confidence, evidence),
            FixtureAnswer::Choice {
                choice,
                confidence,
                evidence,
            } => (
                ReportedSelection::Choice(choice.clone()),
                *confidence,
                evidence,
            ),
            FixtureAnswer::Score {
                level,
                confidence,
                evidence,
            } => (
                ReportedSelection::Score(level.clone()),
                *confidence,
                evidence,
            ),
        };
        let answer = Answer::project(
            question,
            &RawAnswer::ModelReported {
                selection,
                confidence,
                evidence: Some(evidence.clone()),
            },
            ConfidenceProvenance::ModelReported,
        )
        .map_err(|rejection| {
            error(format!(
                "fixture site `{site}` question `{}`: {}",
                rejection.question_id, rejection.diagnostic
            ))
        })?;
        answers.push(answer);
    }
    let identity = super::identity::request_identity(
        &evaluation.state,
        &evaluation.questions,
        &evaluation.policy,
        Some(&route),
    );
    let evaluation_id = super::identity::request_id(&site, &identity);
    let mut receipt = EvaluationReceipt::not_dispatched(
        evaluation_id,
        site,
        identity,
        evaluation.policy.provider.clone(),
        evaluation.policy.model.clone(),
        &evaluation.questions,
        "answered",
        0,
    );
    receipt.source = EvaluationSource::Fixture;
    receipt.input_tokens = Some(0);
    receipt.output_tokens = Some(0);
    receipt.cost_usd = Some(0.0);
    receipt.budget_charge_usd = Some(0.0);
    receipt.record_answers(&answers);
    let reference = receipt.reference();
    let outcome = if answers
        .iter()
        .all(|answer| answer.meets(evaluation.policy.threshold))
    {
        super::outcome::answered(&reference, &answers)
    } else {
        super::outcome::low_confidence(&reference, &answers, evaluation.policy.threshold)
    };
    receipt.outcome_kind = outcome.kind.into();
    let recording = Recording {
        request,
        outcome: crate::llm::helpers::vm_value_to_json(&outcome.into_value()),
        answers,
        receipt,
    };
    validate(&recording)?;
    Ok(recording)
}

type ReusedEvaluation = (Outcome, Vec<Answer>, EvaluationPolicy, EvaluationReceipt);

pub(super) fn lookup(
    ctx: &crate::vm::AsyncBuiltinCtx,
    request: &EvaluationRequest,
) -> Result<Option<ReusedEvaluation>, VmError> {
    let Some(scope) = ctx.evaluation_replay() else {
        return Ok(None);
    };
    let result = lookup_inner(ctx, request, &scope);
    if let Err(error) = &result {
        *scope.1.lock().expect("evaluation replay failure lock") = Some(error.to_string());
    }
    result
}

fn lookup_inner(
    ctx: &crate::vm::AsyncBuiltinCtx,
    request: &EvaluationRequest,
    scope: &EvaluationReplayScope,
) -> Result<Option<ReusedEvaluation>, VmError> {
    let (recorded, source) = {
        let mut mode = scope.0.lock().expect("evaluation replay lock");
        match &mut *mode {
            Mode::Record(_) => return Ok(None),
            Mode::Cache(records) => {
                let Some(recorded) = records.get(&key(request)?).cloned() else {
                    return Ok(None);
                };
                (recorded, EvaluationSource::Cache)
            }
            Mode::Replay(records) => {
                let recorded = records.pop_front().ok_or_else(|| {
                    error(format!("missing record for site `{}`", request.site_id))
                })?;
                (recorded, EvaluationSource::Tape)
            }
            Mode::Fixtures(records) => {
                let recorded = records.pop_front().ok_or_else(|| {
                    error(format!("missing fixture for site `{}`", request.site_id))
                })?;
                (recorded, EvaluationSource::Fixture)
            }
        }
    };
    let verified = verify_receipt(request, &recorded.receipt)?;
    if !verified.verified {
        return Err(error(format!(
            "site `{}`: {:?}",
            request.site_id, verified.refusals
        )));
    }
    let (_, evaluation) = Evaluation::from_arguments(&request.arguments())?;
    let mut receipt = recorded.receipt.clone();
    receipt.invocation_id = Some(ctx.evaluation_invocation_id());
    receipt.reused_from = (source != EvaluationSource::Fixture).then(|| Box::new(recorded.receipt));
    receipt.source = source;
    receipt.physical_attempts = 0;
    receipt.input_tokens = Some(0);
    receipt.output_tokens = Some(0);
    receipt.cost_usd = Some(0.0);
    receipt.budget_charge_usd = Some(0.0);
    receipt.accounting_status = AccountingStatus::NotDispatched;
    receipt.cost_admission = None;
    receipt.native_transport = None;
    receipt.elapsed_ms = 0;
    // A replayed local refusal is still an evaluation occurrence, but never
    // becomes a logical LLM call merely because it came from a tape.
    if receipt.usage.is_some() {
        let mut usage = crate::llm::usage::LlmUsage::from_provider_receipt(
            &receipt.requested_provider,
            &receipt.requested_model,
            &crate::llm::usage::ProviderUsageReceipt::new(Some(0), Some(0), Some(0.0), false),
        );
        usage.provider_call_count = Some(0);
        crate::llm::trace::trace_llm_call(crate::llm::trace::LlmTraceEntry {
            provider: receipt.requested_provider.clone(),
            model: receipt.requested_model.clone(),
            usage: usage.clone(),
            duration_ms: 0,
        });
        receipt.usage = Some(Box::new(usage));
    }
    let mut value = recorded.outcome;
    value["receipt"] = receipt.reference().into();
    let outcome = Outcome::from_recorded(crate::schema::json_to_vm_value(&value))?;
    ctx.record_evaluation_receipt(receipt.clone());
    super::publish(&receipt);
    Ok(Some((
        outcome,
        recorded.answers,
        evaluation.policy,
        receipt,
    )))
}

pub(super) fn record(
    ctx: &crate::vm::AsyncBuiltinCtx,
    request: EvaluationRequest,
    outcome: &Outcome,
    answers: &[Answer],
    receipt: &EvaluationReceipt,
) -> Result<(), VmError> {
    let Some(scope) = ctx.evaluation_replay() else {
        return Ok(());
    };
    let recording = Recording {
        request,
        outcome: crate::llm::helpers::vm_value_to_json(&outcome.clone().into_value()),
        answers: answers.to_vec(),
        receipt: receipt.clone(),
    };
    match &mut *scope.0.lock().expect("evaluation replay lock") {
        Mode::Record(recorder) => {
            let bytes = serde_json::to_vec(&recording).map_err(|e| error(e.to_string()))?;
            let response = recorder.payload_from_bytes(bytes);
            recorder.record(TapeRecordKind::DecisionEvaluation {
                request_digest: receipt.evaluation_id.clone(),
                response,
            });
        }
        Mode::Cache(records) if outcome.kind == "answered" && records.len() < 1024 => {
            records.insert(receipt.evaluation_id.clone(), recording);
        }
        _ => {}
    }
    Ok(())
}

fn validate(record: &Recording) -> Result<(), VmError> {
    if record.receipt.reused_from.is_some() {
        return Err(error("record cannot reuse another receipt"));
    }
    if !verify_receipt(&record.request, &record.receipt)?.verified {
        return Err(error("recorded request does not match receipt"));
    }
    let outcome = Outcome::from_recorded(crate::schema::json_to_vm_value(&record.outcome))?;
    if outcome.kind != record.receipt.outcome_kind
        || record.outcome["receipt"] != record.receipt.reference()
    {
        return Err(error("outcome and receipt differ"));
    }
    let (_, evaluation) = Evaluation::from_arguments(&record.request.arguments())?;
    if matches!(outcome.kind, "answered" | "low_confidence") {
        if record.answers.len() != evaluation.questions.questions.len() {
            return Err(error("answer question count differs"));
        }
        for (question, answer) in evaluation.questions.questions.iter().zip(&record.answers) {
            let (raw, provenance) = raw_answer(answer);
            if Answer::project(question, &raw, provenance).map_err(|e| error(e.diagnostic))?
                != *answer
            {
                return Err(error("recorded answer differs from canonical projection"));
            }
        }
        let reference = record.receipt.reference();
        let expected = if record
            .answers
            .iter()
            .all(|answer| answer.meets(evaluation.policy.threshold))
        {
            super::outcome::answered(&reference, &record.answers)
        } else {
            super::outcome::low_confidence(&reference, &record.answers, evaluation.policy.threshold)
        };
        if crate::llm::helpers::vm_value_to_json(&expected.into_value()) != record.outcome {
            return Err(error("answer outcome differs from canonical projection"));
        }
    }
    Ok(())
}

fn raw_answer(answer: &Answer) -> (RawAnswer, ConfidenceProvenance) {
    let evidence = Some(answer.evidence.clone());
    if answer.confidence_kind == ConfidenceKind::ModelRationale {
        let selection = match &answer.body {
            AnswerBody::Boolean { verdict, .. } => ReportedSelection::Boolean(*verdict),
            AnswerBody::Choice { choice, .. } => ReportedSelection::Choice(choice.clone()),
            AnswerBody::Score { level, .. } => ReportedSelection::Score(level.clone()),
        };
        return (
            RawAnswer::ModelReported {
                selection,
                confidence: answer.confidence,
                evidence,
            },
            ConfidenceProvenance::ModelReported,
        );
    }
    let raw = match &answer.body {
        AnswerBody::Boolean { probability, .. } => RawAnswer::Boolean {
            probability: *probability,
            reported_confidence: None,
            evidence,
        },
        AnswerBody::Choice { choice, .. } => RawAnswer::Choice {
            selected: Some(choice.clone()),
            probabilities: answer.raw_probabilities.clone(),
            reported_confidence: Some(answer.confidence),
            evidence,
        },
        AnswerBody::Score { score, .. } => RawAnswer::Score {
            probabilities: answer.raw_probabilities.clone(),
            score: Some(*score),
            reported_confidence: Some(answer.confidence),
            evidence,
        },
    };
    (raw, ConfidenceProvenance::VendorDistribution)
}
