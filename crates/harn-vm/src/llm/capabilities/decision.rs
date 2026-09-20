//! Typed decision request contract carried by a capability rule.
//!
//! A route that declares the `decision` operation in the provider catalog
//! still has to say *how* the operation is dialled. Decision endpoints are not
//! interchangeable chat endpoints: the protocol picks the URL and the body
//! shape before transport, so it is typed here rather than inferred from the
//! provider id or from a chat capability.
//!
//! These values are deliberately strict. An unknown protocol fails the
//! capability load instead of defaulting to the structured-output chat
//! backend, because defaulting would silently send a decision request to a
//! model that cannot answer one.

use serde::{Deserialize, Serialize};

/// Wire protocol a route serves the `decision` operation over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProtocol {
    /// TypeSafe System One, `POST /v1/systemone`. The direct API.
    TypesafeSystemOne,
    /// Vercel AI Gateway evaluation modality, `POST /v1/evaluate`.
    VercelEvaluate,
    /// OpenRouter decisions, `POST /api/alpha/decisions`.
    OpenrouterDecisions,
    /// A chat route answering a decision through strict JSON-schema
    /// structured output at temperature 0. The only protocol that also needs
    /// `text_generation`, because it dials the ordinary chat endpoint.
    StructuredLlm,
}

impl DecisionProtocol {
    /// Every protocol, for enumerating the closed set in generated bindings
    /// and in tests.
    pub const ALL: [Self; 4] = [
        Self::TypesafeSystemOne,
        Self::VercelEvaluate,
        Self::OpenrouterDecisions,
        Self::StructuredLlm,
    ];

    /// The canonical capability-source string for display and round-trip.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TypesafeSystemOne => "typesafe_system_one",
            Self::VercelEvaluate => "vercel_evaluate",
            Self::OpenrouterDecisions => "openrouter_decisions",
            Self::StructuredLlm => "structured_llm",
        }
    }

    /// Whether this protocol dials a purpose-built decision endpoint. A
    /// native protocol needs the `decision` operation alone; `structured_llm`
    /// additionally needs `text_generation`, since it is a chat call.
    pub const fn is_native(self) -> bool {
        !matches!(self, Self::StructuredLlm)
    }
}

/// A question kind a decision route accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionQuestionKind {
    /// A yes/no question answered by one probability.
    Boolean,
    /// One label out of a described option set.
    Choice,
    /// A fractional rating against ordered, described levels.
    Score,
}

impl DecisionQuestionKind {
    /// Every question kind, for enumerating the closed set.
    pub const ALL: [Self; 3] = [Self::Boolean, Self::Choice, Self::Score];

    /// The canonical capability-source string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }
}

/// Declared request ceilings for a decision route.
///
/// Every field but `max_questions` is required: a route that publishes no
/// question ceiling is a real, documented state, whereas an invented choice or
/// score bound would be read by callers as researched. Unknown keys are
/// refused so a typo cannot read as an unset ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionLimits {
    /// Maximum questions in one request, when the provider publishes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_questions: Option<u32>,
    /// Maximum labelled options on one `choice` question.
    pub max_choice_options: u32,
    /// Fewest ordered levels a `score` question may declare.
    pub score_levels_min: u32,
    /// Most ordered levels a `score` question may declare.
    pub score_levels_max: u32,
    /// Token ceiling on the shared state a request may carry.
    pub state_window_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct ProtocolRow {
        decision_protocol: DecisionProtocol,
    }

    #[derive(Deserialize)]
    struct KindRow {
        kind: DecisionQuestionKind,
    }

    #[test]
    fn unknown_protocol_fails_the_load_instead_of_defaulting() {
        let parsed = toml::from_str::<ProtocolRow>("decision_protocol = \"typesafe_system_two\"\n");
        assert!(parsed.is_err(), "unknown protocol must not decode");
        let structured = toml::from_str::<ProtocolRow>("decision_protocol = \"structured_llm\"\n")
            .unwrap()
            .decision_protocol;
        assert_eq!(structured, DecisionProtocol::StructuredLlm);
        assert!(!structured.is_native());
        assert!(DecisionProtocol::VercelEvaluate.is_native());
    }

    #[test]
    fn limits_refuse_a_misspelled_ceiling_and_keep_max_questions_optional() {
        let misspelled = toml::from_str::<DecisionLimits>(
            "max_choice_option = 255\nscore_levels_min = 2\nscore_levels_max = 10\nstate_window_tokens = 32000\n",
        );
        assert!(misspelled.is_err(), "unknown key must not read as unset");

        let limits: DecisionLimits = toml::from_str(
            "max_choice_options = 255\nscore_levels_min = 2\nscore_levels_max = 10\nstate_window_tokens = 32000\n",
        )
        .unwrap();
        assert_eq!(limits.max_questions, None);
        assert_eq!(limits.max_choice_options, 255);
        assert_eq!(limits.state_window_tokens, 32_000);

        let missing_bound = toml::from_str::<DecisionLimits>(
            "max_choice_options = 255\nscore_levels_min = 2\nstate_window_tokens = 32000\n",
        );
        assert!(missing_bound.is_err(), "a ceiling must not be invented");
    }

    #[test]
    fn question_kinds_round_trip_their_source_spelling() {
        for kind in DecisionQuestionKind::ALL {
            let decoded = toml::from_str::<KindRow>(&format!("kind = \"{}\"\n", kind.as_str()))
                .unwrap()
                .kind;
            assert_eq!(decoded, kind);
        }
        assert!(toml::from_str::<KindRow>("kind = \"ranking\"\n").is_err());
    }
}
