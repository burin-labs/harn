//! The question set of a batched evaluation site.
//!
//! A question set is read statically for two reasons that share one
//! requirement. The site manifest records which questions a site asks, and the
//! checker types each answer from its own question, so a choice answer's
//! `choice` is the literal union of that question's criteria keys and a `match`
//! on it is exhaustive. Both need the labels at check time, so the question set
//! must be a literal at the call site.
//!
//! Question kind is decided from the inferred type of the entry, not from the
//! spelling of the callee: a question is whatever carries one of the three
//! contract shapes. The labels themselves then come from the literal argument,
//! because a `dict<string, string>` type has already lost its keys.

use super::{scope::TypeScope, TypeChecker};
use crate::ast::*;
use crate::builtin_signatures::TyExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateQuestionKind {
    Boolean,
    Choice,
    Score,
}

impl PredicateQuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }

    fn from_literal(kind: &str) -> Option<Self> {
        match kind {
            "boolean" => Some(Self::Boolean),
            "choice" => Some(Self::Choice),
            "score" => Some(Self::Score),
            _ => None,
        }
    }
}

/// One checked question. `labels` are the criteria keys of a choice or the
/// ordered levels of a score, and are empty for a boolean.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PredicateQuestionSpec {
    pub id: String,
    pub kind: PredicateQuestionKind,
    pub instructions: String,
    pub labels: Vec<String>,
}

impl PredicateQuestionSpec {
    /// The answer type this question produces, with its label field narrowed
    /// to the literal union of its own labels.
    pub fn answer_type(&self) -> TypeExpr {
        let (contract, label_field) = match self.kind {
            PredicateQuestionKind::Boolean => {
                return harn_builtin_meta::predicate::BOOLEAN_ANSWER.to_type_expr()
            }
            PredicateQuestionKind::Choice => {
                (harn_builtin_meta::predicate::CHOICE_ANSWER, "choice")
            }
            PredicateQuestionKind::Score => (harn_builtin_meta::predicate::SCORE_ANSWER, "level"),
        };
        let mut answer = contract.to_type_expr();
        if self.labels.is_empty() {
            return answer;
        }
        let labels: Vec<TypeExpr> = self
            .labels
            .iter()
            .map(|label| TypeExpr::LitString(label.clone()))
            .collect();
        let narrowed = if labels.len() == 1 {
            labels.into_iter().next().expect("one label")
        } else {
            TypeExpr::Union(labels)
        };
        if let TypeExpr::Shape(fields) = &mut answer {
            if let Some(field) = fields.iter_mut().find(|field| field.name == label_field) {
                field.type_expr = narrowed;
            }
        }
        answer
    }
}

/// Why a question set could not be read. Each maps to one diagnostic message;
/// the checker owns reporting so the span points at the offending entry.
pub(super) enum QuestionSetError {
    NotLiteral,
    NoQuestions,
    DuplicateId(String),
    EntryNotAQuestion(String),
    LabelsNotLiteral(String),
    NoLabels(String),
    DuplicateLabel(String, String),
    InstructionsNotLiteral(String),
}

impl QuestionSetError {
    pub(super) fn message(&self) -> String {
        match self {
            Self::NotLiteral => {
                "evaluation questions must be a dict literal of question builders".into()
            }
            Self::NoQuestions => "evaluation declares no questions".into(),
            Self::DuplicateId(id) => format!("question id `{id}` is declared more than once"),
            Self::EntryNotAQuestion(id) => format!(
                "question `{id}` is not a boolean, choice, or score question from std/predicate"
            ),
            Self::LabelsNotLiteral(id) => format!(
                "question `{id}` must declare its criteria or levels as a literal so answers can be typed"
            ),
            Self::NoLabels(id) => format!("question `{id}` declares no criteria or levels"),
            Self::DuplicateLabel(id, label) => {
                format!("question `{id}` declares label `{label}` more than once")
            }
            Self::InstructionsNotLiteral(id) => {
                format!("question `{id}` must declare its instructions as a nonempty string literal")
            }
        }
    }

    pub(super) fn help(&self) -> String {
        match self {
            Self::EntryNotAQuestion(_) | Self::NotLiteral => {
                "build each question with boolean(...), choice(...), or score(...) from std/predicate, inline at the call".into()
            }
            _ => "the question set is part of the site's cache identity and types every answer, so it is read at check time".into(),
        }
    }
}

fn literal_text(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(text) | Node::RawStringLiteral(text) if !text.is_empty() => {
            Some(text.clone())
        }
        _ => None,
    }
}

fn entry_key(entry: &DictEntry) -> Option<String> {
    match &entry.key.node {
        Node::StringLiteral(key) | Node::RawStringLiteral(key) | Node::Identifier(key)
            if !key.is_empty() =>
        {
            Some(key.clone())
        }
        _ => None,
    }
}

impl TypeChecker {
    /// The `kind` discriminant of a question, read from its inferred type.
    fn question_kind(&self, node: &SNode, scope: &TypeScope) -> Option<PredicateQuestionKind> {
        let ty = self.resolve_alias(&self.infer_type(node, scope)?, scope);
        let TypeExpr::Shape(fields) = super::union::without_nil(&ty)? else {
            return None;
        };
        let kind = fields.iter().find(|field| field.name == "kind")?;
        let TypeExpr::LitString(kind) = &kind.type_expr else {
            return None;
        };
        PredicateQuestionKind::from_literal(kind)
    }

    /// Read one question entry. The kind comes from the type; the labels and
    /// instructions come from the literal arguments of the builder call.
    fn question_spec(
        &self,
        id: &str,
        node: &SNode,
        scope: &TypeScope,
    ) -> Result<PredicateQuestionSpec, QuestionSetError> {
        let kind = self
            .question_kind(node, scope)
            .ok_or_else(|| QuestionSetError::EntryNotAQuestion(id.to_string()))?;
        let Node::FunctionCall { args, .. } = &node.node else {
            return Err(QuestionSetError::LabelsNotLiteral(id.to_string()));
        };
        let instructions = args
            .first()
            .and_then(literal_text)
            .ok_or_else(|| QuestionSetError::InstructionsNotLiteral(id.to_string()))?;
        let labels = match kind {
            PredicateQuestionKind::Boolean => Vec::new(),
            PredicateQuestionKind::Choice => {
                let Some(SNode {
                    node: Node::DictLiteral(entries),
                    ..
                }) = args.get(1)
                else {
                    return Err(QuestionSetError::LabelsNotLiteral(id.to_string()));
                };
                entries
                    .iter()
                    .map(|entry| {
                        entry_key(entry)
                            .ok_or_else(|| QuestionSetError::LabelsNotLiteral(id.to_string()))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            PredicateQuestionKind::Score => {
                let Some(SNode {
                    node: Node::ListLiteral(items),
                    ..
                }) = args.get(1)
                else {
                    return Err(QuestionSetError::LabelsNotLiteral(id.to_string()));
                };
                items
                    .iter()
                    .map(|item| {
                        literal_text(item)
                            .ok_or_else(|| QuestionSetError::LabelsNotLiteral(id.to_string()))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        if kind != PredicateQuestionKind::Boolean && labels.is_empty() {
            return Err(QuestionSetError::NoLabels(id.to_string()));
        }
        for (index, label) in labels.iter().enumerate() {
            if labels[..index].contains(label) {
                return Err(QuestionSetError::DuplicateLabel(
                    id.to_string(),
                    label.clone(),
                ));
            }
        }
        Ok(PredicateQuestionSpec {
            id: id.to_string(),
            kind,
            instructions,
            labels,
        })
    }

    /// Read the whole question set of an evaluation site.
    pub(super) fn question_set(
        &self,
        node: &SNode,
        scope: &TypeScope,
    ) -> Result<Vec<PredicateQuestionSpec>, QuestionSetError> {
        let Node::DictLiteral(entries) = &node.node else {
            return Err(QuestionSetError::NotLiteral);
        };
        if entries.is_empty() {
            return Err(QuestionSetError::NoQuestions);
        }
        let mut specs: Vec<PredicateQuestionSpec> = Vec::new();
        for entry in entries {
            let id = entry_key(entry).ok_or(QuestionSetError::NotLiteral)?;
            if specs.iter().any(|spec| spec.id == id) {
                return Err(QuestionSetError::DuplicateId(id));
            }
            specs.push(self.question_spec(&id, &entry.value, scope)?);
        }
        Ok(specs)
    }

    /// The record of answers this question set produces, keyed by question id.
    /// `None` whenever the set is not statically readable; the declared
    /// `dict<string, EvaluationAnswer>` then stands, and the site check reports
    /// the reason.
    pub(in crate::typechecker) fn evaluation_answer_record(
        &self,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Option<TypeExpr> {
        let specs = self.question_set(args.get(2)?, scope).ok()?;
        Some(TypeExpr::Shape(
            specs
                .iter()
                .map(|spec| ShapeField::synthetic(spec.id.clone(), spec.answer_type(), false))
                .collect(),
        ))
    }

    /// Replace the answer maps of the `answered` and `low_confidence` arms with
    /// the site's own record of typed answers.
    pub(in crate::typechecker) fn narrow_evaluation_answers(
        mut outcome: TypeExpr,
        answers: TypeExpr,
    ) -> TypeExpr {
        let TypeExpr::Union(members) = &mut outcome else {
            return outcome;
        };
        for member in members.iter_mut() {
            let TypeExpr::Shape(fields) = member else {
                continue;
            };
            let arm = fields
                .iter()
                .find(|field| field.name == "kind")
                .map(|field| {
                    matches!(&field.type_expr, TypeExpr::LitString(kind)
                    if kind == "answered" || kind == "low_confidence")
                });
            if arm != Some(true) {
                continue;
            }
            if let Some(field) = fields
                .iter_mut()
                .find(|field| matches!(field.name.as_str(), "value" | "candidates"))
            {
                field.type_expr = answers.clone();
            }
        }
        outcome
    }
}
