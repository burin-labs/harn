//! What a route declares about answering decisions.
//!
//! The catalog owns these facts. This module is the runtime's one accessor for
//! them, so the evaluator never reads a capability row itself and never
//! defaults a limit a native route did not declare.

use crate::llm_config;

pub use crate::llm::capabilities::{DecisionProtocol, DecisionQuestionKind};

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
    pub request_window_tokens: Option<usize>,
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
/// The catalog owns native protocols and limits. Chat routes receive only
/// the evaluator's own schema and output bounds here.
pub fn decision_contract_for_route(provider: &str, model: &str) -> Option<DecisionContract> {
    let declared = crate::provider_catalog::decision_contract_for_route(provider, model)?;
    let id = llm_config::model_catalog_id_for_route(provider, model)?;
    let entry = llm_config::model_catalog_entry(&id)?;
    if declared.protocol.is_native() {
        let limits = declared.limits?;
        return Some(DecisionContract {
            protocol: declared.protocol,
            question_kinds: declared.question_kinds,
            limits: DecisionLimits {
                max_questions: limits.max_questions.map(|n| n as usize),
                max_choice_options: limits.max_choice_options as usize,
                score_levels_min: limits.score_levels_min as usize,
                score_levels_max: limits.score_levels_max as usize,
                state_window_tokens: usize::try_from(limits.state_window_tokens).ok()?,
                request_window_tokens: limits
                    .request_window_tokens
                    .map(usize::try_from)
                    .transpose()
                    .ok()?,
            },
            input_price_per_mtok: declared.input_price_per_mtok,
            output_price_per_mtok: declared.output_price_per_mtok,
            served_model_id: declared.served_model_id,
        });
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
            request_window_tokens: None,
        },
        // An absent rate card is an unknown price, and an unknown price
        // refuses dispatch rather than charging nothing.
        input_price_per_mtok: entry.pricing.as_ref().map(|price| price.input_per_mtok),
        output_price_per_mtok: entry.pricing.as_ref().map(|price| price.output_per_mtok),
        served_model_id: entry.wire_model.unwrap_or_else(|| model.to_string()),
    })
}
