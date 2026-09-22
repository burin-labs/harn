//! The question set as the evaluator sees it at run time.
//!
//! Every check here is local. A question the route's declared limits refuse is
//! refused before dispatch, so the caller gets `question_invalid` having made
//! zero provider requests and paid nothing. Static and runtime-declared
//! vocabularies share these semantic and route-limit checks.

use crate::value::VmValue;

use super::contract::{DecisionContract, DecisionQuestionKind};

/// Why a question set cannot be dispatched. These are the closed reasons of
/// the `question_invalid` outcome arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRefusal {
    /// The offending question's id, or the empty string when the whole set is.
    pub question: String,
    pub reason: QuestionRefusalReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionRefusalReason {
    EmptyQuestions,
    EmptyOptions,
    EmptyIdentifier,
    DuplicateLabels,
    TooManyOptions,
    TooFewLevels,
    TooManyLevels,
    EmptyInstructions,
    TooManyQuestions,
    UnsupportedQuestionKind,
}

impl QuestionRefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmptyQuestions => "empty_questions",
            Self::EmptyOptions => "empty_options",
            Self::EmptyIdentifier => "empty_identifier",
            Self::DuplicateLabels => "duplicate_labels",
            Self::TooManyOptions => "too_many_options",
            Self::TooFewLevels => "too_few_levels",
            Self::TooManyLevels => "too_many_levels",
            Self::EmptyInstructions => "empty_instructions",
            Self::TooManyQuestions => "too_many_questions",
            Self::UnsupportedQuestionKind => "unsupported_question_kind",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum QuestionBody {
    Boolean,
    /// Label to the description the model judges against, in declared order.
    Choice(Vec<(String, String)>),
    /// Ordered levels, lowest first.
    Score(Vec<String>),
}

impl QuestionBody {
    pub fn kind(&self) -> DecisionQuestionKind {
        match self {
            Self::Boolean => DecisionQuestionKind::Boolean,
            Self::Choice(_) => DecisionQuestionKind::Choice,
            Self::Score(_) => DecisionQuestionKind::Score,
        }
    }

    /// The labels an answer's probability map is keyed by. A boolean answer
    /// carries one probability rather than a map, so it has none.
    pub fn labels(&self) -> Vec<String> {
        match self {
            Self::Boolean => Vec::new(),
            Self::Choice(criteria) => criteria.iter().map(|(label, _)| label.clone()).collect(),
            Self::Score(levels) => levels.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Question {
    pub id: String,
    pub instructions: String,
    pub body: QuestionBody,
}

/// A whole question set, in declared order. Order is significant: it is part
/// of the request and part of the cache identity.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize)]
pub struct QuestionSet {
    pub questions: Vec<Question>,
}

fn text(value: Option<&VmValue>) -> Option<String> {
    match value? {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    }
}

impl QuestionSet {
    /// Read the question set out of the argument the VM passed.
    ///
    /// A shape the contract does not describe is a runtime type error, not a
    /// `question_invalid`: the checker owns the shape and only a caller
    /// bypassing it can get here.
    pub fn from_value(value: &VmValue) -> Result<Self, String> {
        let Some(entries) = value.as_dict() else {
            return Err(format!(
                "evaluation questions must be a dict of question id to question, got {}",
                value.type_name()
            ));
        };
        let mut questions = Vec::with_capacity(entries.len());
        for (id, question) in entries.iter() {
            let id = id.to_string();
            let Some(fields) = question.as_dict() else {
                return Err(format!("question `{id}` is not a question record"));
            };
            let instructions = text(fields.get("instructions"))
                .ok_or_else(|| format!("question `{id}` has no string instructions"))?;
            let kind = text(fields.get("kind"))
                .ok_or_else(|| format!("question `{id}` has no question kind"))?;
            let body = match kind.as_str() {
                "boolean" => QuestionBody::Boolean,
                "choice" => {
                    let criteria = fields
                        .get("criteria")
                        .and_then(VmValue::as_dict)
                        .ok_or_else(|| format!("choice question `{id}` has no criteria"))?;
                    QuestionBody::Choice(
                        criteria
                            .iter()
                            .map(|(label, description)| {
                                Ok((
                                    label.to_string(),
                                    text(Some(description)).ok_or_else(|| {
                                        format!("choice question `{id}` has a non-string criterion")
                                    })?,
                                ))
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    )
                }
                "score" => {
                    let VmValue::List(levels) = fields
                        .get("levels")
                        .ok_or_else(|| format!("score question `{id}` has no levels"))?
                    else {
                        return Err(format!("score question `{id}` has non-list levels"));
                    };
                    QuestionBody::Score(
                        levels
                            .iter()
                            .map(|level| {
                                text(Some(level)).ok_or_else(|| {
                                    format!("score question `{id}` has a non-string level")
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    )
                }
                other => return Err(format!("question `{id}` has unknown kind `{other}`")),
            };
            questions.push(Question {
                id,
                instructions,
                body,
            });
        }
        Ok(Self { questions })
    }

    /// The declared ids, in order. Read by the mock backend's request log.
    #[cfg(test)]
    pub fn ids(&self) -> Vec<String> {
        self.questions
            .iter()
            .map(|question| question.id.clone())
            .collect()
    }

    /// Check the set against what the route declared. The first refusal wins,
    /// and it is returned rather than thrown so the caller gets a typed
    /// outcome instead of an error to catch.
    pub fn admit(&self, contract: &DecisionContract) -> Result<(), QuestionRefusal> {
        let limits = &contract.limits;
        if self.questions.is_empty() {
            return Err(QuestionRefusal {
                question: String::new(),
                reason: QuestionRefusalReason::EmptyQuestions,
            });
        }
        if let Some(max) = limits.max_questions {
            if self.questions.len() > max {
                return Err(QuestionRefusal {
                    question: String::new(),
                    reason: QuestionRefusalReason::TooManyQuestions,
                });
            }
        }
        for question in &self.questions {
            let refuse = |reason| {
                Err(QuestionRefusal {
                    question: question.id.clone(),
                    reason,
                })
            };
            if question.id.trim().is_empty() {
                return refuse(QuestionRefusalReason::EmptyIdentifier);
            }
            if question.instructions.trim().is_empty() {
                return refuse(QuestionRefusalReason::EmptyInstructions);
            }
            if !contract.supports(question.body.kind()) {
                return refuse(QuestionRefusalReason::UnsupportedQuestionKind);
            }
            match &question.body {
                QuestionBody::Boolean => {}
                QuestionBody::Choice(criteria) => {
                    if criteria.is_empty() {
                        return refuse(QuestionRefusalReason::EmptyOptions);
                    }
                    if criteria.iter().any(|(label, _)| label.trim().is_empty()) {
                        return refuse(QuestionRefusalReason::EmptyIdentifier);
                    }
                    if criteria.len() > limits.max_choice_options {
                        return refuse(QuestionRefusalReason::TooManyOptions);
                    }
                    // One label remains a legitimate bounded vocabulary.
                }
                QuestionBody::Score(levels) => {
                    if levels.iter().any(|label| label.trim().is_empty()) {
                        return refuse(QuestionRefusalReason::EmptyIdentifier);
                    }
                    if levels
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != levels.len()
                    {
                        return refuse(QuestionRefusalReason::DuplicateLabels);
                    }
                    if levels.len() < limits.score_levels_min {
                        return refuse(QuestionRefusalReason::TooFewLevels);
                    }
                    if levels.len() > limits.score_levels_max {
                        return refuse(QuestionRefusalReason::TooManyLevels);
                    }
                }
            }
        }
        Ok(())
    }
}
