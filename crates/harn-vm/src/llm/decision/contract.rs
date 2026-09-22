//! What a route declares about answering decisions.
//!
//! The catalog owns these facts. This module is the runtime's one accessor for
//! them, so the evaluator never reads a capability row itself and never
//! defaults a limit a native route did not declare.

use crate::llm_config::{self, ModelOperation};

/// How a route is asked for a decision. A gateway can serve chat and decisions
/// through different endpoints, so the protocol is a route fact, not a
/// provider fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProtocol {
    TypesafeSystemOne,
    VercelEvaluate,
    OpenrouterDecisions,
    /// A chat route answering the batch through one generated JSON schema.
    StructuredLlm,
}

impl DecisionProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TypesafeSystemOne => "typesafe_system_one",
            Self::VercelEvaluate => "vercel_evaluate",
            Self::OpenrouterDecisions => "openrouter_decisions",
            Self::StructuredLlm => "structured_llm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionQuestionKind {
    Boolean,
    Choice,
    Score,
}

impl DecisionQuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }
}

/// Declared bounds. Every field is a number the route states; none is a
/// default the evaluator invented. `max_questions` is absent when the route
/// declares no ceiling, which is different from a ceiling of zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecisionLimits {
    pub max_questions: Option<usize>,
    pub max_choice_options: usize,
    pub score_levels_min: usize,
    pub score_levels_max: usize,
    pub state_window_tokens: usize,
}

/// Everything the evaluator needs about a route before it dispatches.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionContract {
    pub protocol: DecisionProtocol,
    pub question_kinds: Vec<DecisionQuestionKind>,
    pub limits: DecisionLimits,
    pub input_price_per_mtok: Option<f64>,
    pub output_price_per_mtok: Option<f64>,
    pub served_model_id: String,
}

impl DecisionContract {
    pub fn supports(&self, kind: DecisionQuestionKind) -> bool {
        self.question_kinds.contains(&kind)
    }
}

/// The limits a chat route answering through one generated JSON schema obeys.
///
/// These are the evaluator's own bounds, not a vendor's: the schema it emits
/// caps the option count and the state window comes from the route's context
/// window, less the schema and instruction overhead the evaluator adds.
const STRUCTURED_LLM_MAX_CHOICE_OPTIONS: usize = 255;
const STRUCTURED_LLM_SCORE_LEVELS_MIN: usize = 2;
const STRUCTURED_LLM_SCORE_LEVELS_MAX: usize = 10;
/// Reserved for the evaluator instruction, the generated schema, and the
/// answers. Subtracted from the route's declared context window.
const STRUCTURED_LLM_OVERHEAD_TOKENS: usize = 2048;

/// The decision contract of a route, or `None` when the route declares no
/// decision operation.
///
/// Native decision rows and their capability-declared protocols, question
/// kinds, and limits are owned by the catalog work on #8537. Until those rows
/// exist, this resolves only the `structured_llm` projection of a chat route
/// that declares both `decision` and `text_generation`; a route that declares
/// `decision` without `text_generation` has no contract here yet and the
/// evaluator reports it as unconfigured rather than guessing a protocol.
pub fn decision_contract_for_route(provider: &str, model: &str) -> Option<DecisionContract> {
    let id = llm_config::model_catalog_id_for_route(provider, model)?;
    let entry = llm_config::model_catalog_entry(&id)?;
    if !entry.supports_operation(ModelOperation::Decision)
        || !entry.supports_operation(ModelOperation::TextGeneration)
    {
        return None;
    }
    let window = entry.context_window as usize;
    Some(DecisionContract {
        protocol: DecisionProtocol::StructuredLlm,
        question_kinds: vec![
            DecisionQuestionKind::Boolean,
            DecisionQuestionKind::Choice,
            DecisionQuestionKind::Score,
        ],
        limits: DecisionLimits {
            max_questions: None,
            max_choice_options: STRUCTURED_LLM_MAX_CHOICE_OPTIONS,
            score_levels_min: STRUCTURED_LLM_SCORE_LEVELS_MIN,
            score_levels_max: STRUCTURED_LLM_SCORE_LEVELS_MAX,
            state_window_tokens: window.saturating_sub(STRUCTURED_LLM_OVERHEAD_TOKENS),
        },
        // An absent rate card is an unknown price, and an unknown price
        // refuses dispatch rather than charging nothing.
        input_price_per_mtok: entry.pricing.as_ref().map(|price| price.input_per_mtok),
        output_price_per_mtok: entry.pricing.as_ref().map(|price| price.output_per_mtok),
        served_model_id: entry.wire_model.unwrap_or_else(|| model.to_string()),
    })
}
