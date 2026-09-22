//! Raw distributions to typed answers.
//!
//! This is where a probability becomes a verdict, and it is deliberately the
//! only place: both backends project through it, so a native decision and a
//! structured LLM cannot disagree about what "confident" means. What they can
//! disagree about is where the number came from, which is why every answer
//! names its own `confidence_kind` instead of presenting one comparable score.

use std::collections::BTreeMap;

use crate::value::VmValue;

use super::backend::{ConfidenceProvenance, RawAnswer, ReportedSelection};
use super::question::{Question, QuestionBody};

/// The provenance of a confidence number. These are different quantities and
/// one threshold does not equalize their error rates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfidenceKind {
    /// Derived by the evaluator from a single yes-probability.
    BinaryProbability,
    /// The vendor's own summary of a distribution's shape.
    DistributionShape,
    /// A number the model reported about itself.
    ModelRationale,
}

impl ConfidenceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::BinaryProbability => "binary_probability",
            Self::DistributionShape => "distribution_shape",
            Self::ModelRationale => "model_rationale",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceKind {
    /// Mechanically produced from the supplied state.
    InputReference,
    /// Prose the model generated.
    ModelRationale,
}

impl EvidenceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::InputReference => "input_reference",
            Self::ModelRationale => "model_rationale",
        }
    }
}

/// Evidence is bounded so a model cannot spend the caller's transcript on
/// rationale. Bytes, not characters: the contract is a byte bound.
pub(super) const MAX_EVIDENCE_BYTES: usize = 2048;

/// One typed answer, ready to project into the VM and onto the receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub question_id: String,
    pub confidence: f64,
    pub confidence_kind: ConfidenceKind,
    pub evidence: String,
    pub evidence_kind: EvidenceKind,
    pub body: AnswerBody,
    /// The distribution as the backend reported it, before conversion. Empty
    /// for structured model reports, which contain no measured distribution.
    pub raw_probabilities: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnswerBody {
    Boolean {
        verdict: bool,
        probability: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        level: String,
        score: f64,
        probabilities: BTreeMap<String, f64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerRejection {
    pub question_id: String,
    pub diagnostic: String,
}

fn finite_probability(value: f64, what: &str, question: &str) -> Result<f64, AnswerRejection> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(AnswerRejection {
            question_id: question.to_string(),
            diagnostic: format!("{what} is not a finite probability in [0, 1]"),
        });
    }
    Ok(value)
}

/// Truncate on a character boundary, so a bounded evidence string is still
/// valid UTF-8 rather than a panic or a broken code point.
fn bounded_evidence(evidence: Option<String>) -> String {
    let Some(mut evidence) = evidence else {
        return String::new();
    };
    if evidence.len() <= MAX_EVIDENCE_BYTES {
        return evidence;
    }
    let mut end = MAX_EVIDENCE_BYTES;
    while end > 0 && !evidence.is_char_boundary(end) {
        end -= 1;
    }
    evidence.truncate(end);
    evidence
}

/// The declared label with the highest probability, ties broken by declared
/// order so the same distribution always yields the same answer.
fn argmax<'a>(
    labels: &'a [String],
    probabilities: &BTreeMap<String, f64>,
) -> Option<(&'a str, f64)> {
    let mut best: Option<(&str, f64)> = None;
    for label in labels {
        let probability = *probabilities.get(label)?;
        if best.is_none_or(|(_, top)| probability > top) {
            best = Some((label.as_str(), probability));
        }
    }
    best
}

/// A distribution must name every declared label and nothing else. An extra
/// label means the backend answered a question that was not asked.
fn admit_distribution(
    question: &Question,
    labels: &[String],
    probabilities: &BTreeMap<String, f64>,
) -> Result<(), AnswerRejection> {
    let reject = |diagnostic: String| AnswerRejection {
        question_id: question.id.clone(),
        diagnostic,
    };
    if probabilities.len() != labels.len() {
        return Err(reject(format!(
            "distribution names {} labels, the question declares {}",
            probabilities.len(),
            labels.len()
        )));
    }
    for label in labels {
        let Some(probability) = probabilities.get(label) else {
            return Err(reject(format!("distribution omits label `{label}`")));
        };
        finite_probability(*probability, "probability", &question.id)?;
    }
    Ok(())
}

impl Answer {
    /// Project one raw answer onto its question. A mismatch between the two is
    /// a rejection, never a coerced answer.
    pub fn project(
        question: &Question,
        raw: &RawAnswer,
        provenance: ConfidenceProvenance,
    ) -> Result<Self, AnswerRejection> {
        let reject = |diagnostic: &str| AnswerRejection {
            question_id: question.id.clone(),
            diagnostic: diagnostic.to_string(),
        };
        let labels = question.body.labels();
        if let RawAnswer::ModelReported {
            selection,
            confidence,
            evidence,
        } = raw
        {
            if provenance != ConfidenceProvenance::ModelReported {
                return Err(reject(
                    "a named model report requires model-reported provenance",
                ));
            }
            let confidence = finite_probability(*confidence, "confidence", &question.id)?;
            let distribution = |selected: &str| {
                let others = labels.len().saturating_sub(1);
                labels
                    .iter()
                    .map(|label| {
                        let probability = if others == 0 {
                            1.0
                        } else if label == selected {
                            confidence
                        } else {
                            (1.0 - confidence) / others as f64
                        };
                        (label.clone(), probability)
                    })
                    .collect()
            };
            let body = match (&question.body, selection) {
                (QuestionBody::Boolean, ReportedSelection::Boolean(verdict)) => {
                    AnswerBody::Boolean {
                        verdict: *verdict,
                        probability: if *verdict {
                            confidence
                        } else {
                            1.0 - confidence
                        },
                    }
                }
                (QuestionBody::Choice(_), ReportedSelection::Choice(choice))
                    if labels.contains(choice) =>
                {
                    AnswerBody::Choice {
                        choice: choice.clone(),
                        probabilities: distribution(choice),
                    }
                }
                (QuestionBody::Score(_), ReportedSelection::Score(level))
                    if labels.contains(level) =>
                {
                    AnswerBody::Score {
                        level: level.clone(),
                        score: labels.iter().position(|label| label == level).unwrap() as f64,
                        probabilities: distribution(level),
                    }
                }
                _ => return Err(reject("reported selection does not match the question")),
            };
            return Ok(Self {
                question_id: question.id.clone(),
                confidence,
                confidence_kind: ConfidenceKind::ModelRationale,
                evidence: bounded_evidence(evidence.clone()),
                evidence_kind: EvidenceKind::ModelRationale,
                body,
                raw_probabilities: BTreeMap::new(),
            });
        }
        match (&question.body, raw) {
            (
                QuestionBody::Boolean,
                RawAnswer::Boolean {
                    probability,
                    reported_confidence,
                    evidence,
                },
            ) => {
                let probability = finite_probability(*probability, "probability", &question.id)?;
                // A binary answer's confidence is in the verdict it selected,
                // including a negative one. Reading the yes-probability as
                // confidence would report a confident "no" as uncertain.
                let (confidence, confidence_kind) = match (provenance, reported_confidence) {
                    (ConfidenceProvenance::ModelReported, Some(reported)) => (
                        finite_probability(*reported, "confidence", &question.id)?,
                        ConfidenceKind::ModelRationale,
                    ),
                    _ => (
                        probability.max(1.0 - probability),
                        ConfidenceKind::BinaryProbability,
                    ),
                };
                Ok(Self {
                    question_id: question.id.clone(),
                    confidence,
                    confidence_kind,
                    evidence: bounded_evidence(evidence.clone()),
                    evidence_kind: evidence_kind_of(provenance),
                    body: AnswerBody::Boolean {
                        verdict: probability >= 0.5,
                        probability,
                    },
                    raw_probabilities: BTreeMap::from([("true".to_string(), probability)]),
                })
            }
            (
                QuestionBody::Choice(_),
                RawAnswer::Choice {
                    probabilities,
                    reported_confidence,
                    evidence,
                },
            ) => {
                admit_distribution(question, &labels, probabilities)?;
                let (choice, _) = argmax(&labels, probabilities)
                    .ok_or_else(|| reject("distribution is empty"))?;
                let (confidence, confidence_kind) = distribution_confidence(
                    provenance,
                    reported_confidence,
                    probabilities,
                    &question.id,
                )?;
                Ok(Self {
                    question_id: question.id.clone(),
                    confidence,
                    confidence_kind,
                    evidence: bounded_evidence(evidence.clone()),
                    evidence_kind: evidence_kind_of(provenance),
                    body: AnswerBody::Choice {
                        choice: choice.to_string(),
                        probabilities: probabilities.clone(),
                    },
                    raw_probabilities: probabilities.clone(),
                })
            }
            (
                QuestionBody::Score(levels),
                RawAnswer::Score {
                    probabilities,
                    score,
                    reported_confidence,
                    evidence,
                },
            ) => {
                admit_distribution(question, &labels, probabilities)?;
                let (level, _) = argmax(&labels, probabilities)
                    .ok_or_else(|| reject("distribution is empty"))?;
                // A reported score is on the declared scale. Without one, the
                // expected index under the distribution is the position, which
                // is the same quantity the vendor reports.
                let score = match score {
                    Some(score) if score.is_finite() => *score,
                    Some(_) => return Err(reject("score is not finite")),
                    None => levels
                        .iter()
                        .enumerate()
                        .map(|(index, level)| {
                            index as f64 * probabilities.get(level).copied().unwrap_or(0.0)
                        })
                        .sum(),
                };
                let (confidence, confidence_kind) = distribution_confidence(
                    provenance,
                    reported_confidence,
                    probabilities,
                    &question.id,
                )?;
                Ok(Self {
                    question_id: question.id.clone(),
                    confidence,
                    confidence_kind,
                    evidence: bounded_evidence(evidence.clone()),
                    evidence_kind: evidence_kind_of(provenance),
                    body: AnswerBody::Score {
                        level: level.to_string(),
                        score,
                        probabilities: probabilities.clone(),
                    },
                    raw_probabilities: probabilities.clone(),
                })
            }
            _ => Err(reject("answer kind does not match the question kind")),
        }
    }

    pub fn meets(&self, threshold: f64) -> bool {
        self.confidence >= threshold
    }

    pub fn to_vm_value(&self) -> VmValue {
        let probabilities = |probabilities: &BTreeMap<String, f64>| {
            VmValue::dict(
                probabilities
                    .iter()
                    .map(|(label, probability)| (label.as_str(), VmValue::Float(*probability)))
                    .collect::<Vec<_>>(),
            )
        };
        let mut fields: Vec<(&str, VmValue)> = match &self.body {
            AnswerBody::Boolean {
                verdict,
                probability,
            } => vec![
                ("kind", VmValue::String("boolean".into())),
                ("verdict", VmValue::Bool(*verdict)),
                ("probability", VmValue::Float(*probability)),
            ],
            AnswerBody::Choice {
                choice,
                probabilities: distribution,
            } => vec![
                ("kind", VmValue::String("choice".into())),
                ("choice", VmValue::String(choice.as_str().into())),
                ("probabilities", probabilities(distribution)),
            ],
            AnswerBody::Score {
                level,
                score,
                probabilities: distribution,
            } => vec![
                ("kind", VmValue::String("score".into())),
                ("level", VmValue::String(level.as_str().into())),
                ("score", VmValue::Float(*score)),
                ("probabilities", probabilities(distribution)),
            ],
        };
        fields.push(("confidence", VmValue::Float(self.confidence)));
        fields.push((
            "confidence_kind",
            VmValue::String(self.confidence_kind.as_str().into()),
        ));
        fields.push(("evidence", VmValue::String(self.evidence.as_str().into())));
        fields.push((
            "evidence_kind",
            VmValue::String(self.evidence_kind.as_str().into()),
        ));
        VmValue::dict(fields)
    }
}

/// Generated prose is a model rationale; a mechanically cited observation is
/// an input reference. The backend that reports its own confidence is the one
/// that also wrote the evidence.
fn evidence_kind_of(provenance: ConfidenceProvenance) -> EvidenceKind {
    match provenance {
        ConfidenceProvenance::ModelReported => EvidenceKind::ModelRationale,
        ConfidenceProvenance::VendorDistribution => EvidenceKind::InputReference,
    }
}

/// A vendor confidence summarizes the distribution it came with. Without one,
/// the top probability is the evaluator's own summary of the same shape. A
/// number the model wrote about itself stays labelled as such.
fn distribution_confidence(
    provenance: ConfidenceProvenance,
    reported: &Option<f64>,
    probabilities: &BTreeMap<String, f64>,
    question: &str,
) -> Result<(f64, ConfidenceKind), AnswerRejection> {
    let kind = match provenance {
        ConfidenceProvenance::ModelReported => ConfidenceKind::ModelRationale,
        ConfidenceProvenance::VendorDistribution => ConfidenceKind::DistributionShape,
    };
    match reported {
        Some(reported) => Ok((finite_probability(*reported, "confidence", question)?, kind)),
        None => Ok((
            probabilities
                .values()
                .copied()
                .fold(0.0_f64, |top, probability| top.max(probability)),
            kind,
        )),
    }
}
