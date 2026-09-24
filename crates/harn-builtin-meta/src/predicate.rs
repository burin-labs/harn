//! The decision evaluation boundary shared by checking and execution.
//!
//! This is a capability method, not a second expression grammar. The checker
//! supplements the ordinary signature with literal site identity, closed data
//! input, a literal question set, and outcome consumption checks.
//!
//! [`EVALUATE`] answers a whole question set over one shared state in one
//! request. [`EVALUATE_PREDICATE`] is its single-boolean projection: the same
//! evaluator, the same receipt, one question named by the site.

use crate::{BuiltinSignature, Param, ShapeFieldDescriptor as Field, Ty};

const STRING: Ty = Ty::Named("string");
const FLOAT: Ty = Ty::Named("float");
const INT: Ty = Ty::Named("int");
const BOOL: Ty = Ty::Named("bool");

const DICT_STRING_STRING_ARGS: &[Ty] = &[STRING, STRING];
const DICT_STRING_STRING: Ty = Ty::Apply("dict", DICT_STRING_STRING_ARGS);
const DICT_STRING_FLOAT_ARGS: &[Ty] = &[STRING, FLOAT];
const DICT_STRING_FLOAT: Ty = Ty::Apply("dict", DICT_STRING_FLOAT_ARGS);
const LIST_STRING_ARGS: &[Ty] = &[STRING];
const LIST_STRING: Ty = Ty::Apply("list", LIST_STRING_ARGS);

/// Where a confidence number came from. These are different quantities and a
/// threshold does not equalize their error rates, so the answer names the
/// provenance rather than presenting one comparable score.
const CONFIDENCE_KIND: Ty = Ty::Union(&[
    Ty::LitString("binary_probability"),
    Ty::LitString("distribution_shape"),
    Ty::LitString("model_rationale"),
]);

/// `input_reference` is a mechanically produced citation of the supplied
/// state. `model_rationale` is generated prose. A native decision backend must
/// never make a second hidden text call to manufacture the latter.
const EVIDENCE_KIND: Ty = Ty::Union(&[
    Ty::LitString("input_reference"),
    Ty::LitString("model_rationale"),
]);

// -- Questions ---------------------------------------------------------------

pub const BOOLEAN_QUESTION: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("boolean")),
    Field::new("instructions", STRING),
]);

pub const CHOICE_QUESTION: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("choice")),
    Field::new("instructions", STRING),
    Field::new("criteria", DICT_STRING_STRING),
]);

pub const SCORE_QUESTION: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("score")),
    Field::new("instructions", STRING),
    Field::new("levels", LIST_STRING),
]);

pub const QUESTION: Ty = Ty::Union(&[BOOLEAN_QUESTION, CHOICE_QUESTION, SCORE_QUESTION]);

const DICT_STRING_QUESTION_ARGS: &[Ty] = &[STRING, QUESTION];
const DICT_STRING_QUESTION: Ty = Ty::Apply("dict", DICT_STRING_QUESTION_ARGS);

// -- Answers -----------------------------------------------------------------

pub const BOOLEAN_ANSWER: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("boolean")),
    Field::new("verdict", BOOL),
    Field::new("probability", FLOAT),
    Field::new("confidence", FLOAT),
    Field::new("confidence_kind", CONFIDENCE_KIND),
    Field::new("evidence", STRING),
    Field::new("evidence_kind", EVIDENCE_KIND),
]);

/// `choice` is the declared label. The checker narrows it to the literal union
/// of the criteria keys at each site, so a `match` on it is exhaustive.
pub const CHOICE_ANSWER: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("choice")),
    Field::new("choice", STRING),
    Field::new("probabilities", DICT_STRING_FLOAT),
    Field::new("confidence", FLOAT),
    Field::new("confidence_kind", CONFIDENCE_KIND),
    Field::new("evidence", STRING),
    Field::new("evidence_kind", EVIDENCE_KIND),
]);

/// `level` is narrowed to the literal union of the declared levels. `score` is
/// the fractional position on that ordered scale.
pub const SCORE_ANSWER: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("score")),
    Field::new("level", STRING),
    Field::new("score", FLOAT),
    Field::new("probabilities", DICT_STRING_FLOAT),
    Field::new("confidence", FLOAT),
    Field::new("confidence_kind", CONFIDENCE_KIND),
    Field::new("evidence", STRING),
    Field::new("evidence_kind", EVIDENCE_KIND),
]);

pub const ANSWER: Ty = Ty::Union(&[BOOLEAN_ANSWER, CHOICE_ANSWER, SCORE_ANSWER]);

const DICT_STRING_ANSWER_ARGS: &[Ty] = &[STRING, ANSWER];
const DICT_STRING_ANSWER: Ty = Ty::Apply("dict", DICT_STRING_ANSWER_ARGS);

/// The single-boolean projection's accepted value. This is the structured
/// backend's one output schema per question as well.
pub const VERDICT: Ty = Ty::Shape(&[
    Field::new("verdict", BOOL),
    Field::new("confidence", FLOAT),
    Field::new("evidence", STRING),
]);

// -- Shared refusal arms -----------------------------------------------------
//
// Both outcome unions carry these. They are written once here and listed in
// each union below, because a `Ty::Union` needs one `'static` slice and const
// slice concatenation is not available.

const REFUSED: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("refused")),
    Field::new(
        "reason",
        Ty::Union(&[
            Ty::LitString("provider_refusal"),
            Ty::LitString("schema_invalid"),
            Ty::LitString("output_truncated"),
        ]),
    ),
    Field::new("diagnostic", STRING),
    Field::new("receipt", STRING),
]);

const BUDGET_CUT: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("budget_cut")),
    Field::new(
        "limit",
        Ty::Union(&[
            Ty::LitString("requests"),
            Ty::LitString("evaluations"),
            Ty::LitString("input_tokens"),
            Ty::LitString("output_tokens"),
            Ty::LitString("deadline"),
            Ty::LitString("evaluation_cost"),
            Ty::LitString("run_cost"),
            Ty::LitString("parent_budget"),
        ]),
    ),
    Field::new("requested", FLOAT),
    Field::new("remaining", FLOAT),
    Field::new("receipt", STRING),
]);

const UNAVAILABLE: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("unavailable")),
    Field::new(
        "reason",
        Ty::Union(&[
            Ty::LitString("model_unconfigured"),
            Ty::LitString("unsupported_options"),
            Ty::LitString("transport_failed"),
            Ty::LitString("authority_denied"),
            Ty::LitString("producer_cancelled"),
            Ty::LitString("cache_miss"),
        ]),
    ),
    Field::new("receipt", STRING),
]);

const REPLAY_MISMATCH: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("replay_mismatch")),
    Field::new("expected_identity", STRING),
    Field::new("actual_identity", STRING),
    Field::new("occurrence", INT),
    Field::new("receipt", STRING),
]);

const CANCELLED: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("cancelled")),
    Field::new("control_event", STRING),
    Field::new("receipt", STRING),
]);

/// The state did not fit the route's declared window. `estimated_tokens` is
/// the evaluator's own local estimate in both the pre-dispatch refusal and the
/// provider-reported one, so the estimator can be tuned from real data.
const STATE_TOO_LARGE: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("state_too_large")),
    Field::new("limit_tokens", INT),
    Field::new("estimated_tokens", INT),
    Field::new("receipt", STRING),
]);

/// A question the route's declared limits refuse. Checked locally, so this
/// refusal makes zero provider requests.
const QUESTION_INVALID: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("question_invalid")),
    Field::new("question", STRING),
    Field::new(
        "reason",
        Ty::Union(&[
            Ty::LitString("too_many_options"),
            Ty::LitString("too_few_levels"),
            Ty::LitString("too_many_levels"),
            Ty::LitString("empty_instructions"),
            Ty::LitString("too_many_questions"),
            Ty::LitString("unsupported_question_kind"),
        ]),
    ),
    Field::new("receipt", STRING),
]);

/// Provider 429. No implicit retry; the caller's policy decides.
const RATE_LIMITED: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("rate_limited")),
    Field::optional("retry_after_ms", INT),
    Field::new("receipt", STRING),
]);

/// Provider 529/503. No implicit retry.
const OVERLOADED: Ty = Ty::Shape(&[
    Field::new("kind", Ty::LitString("overloaded")),
    Field::new("receipt", STRING),
]);

// -- Outcomes ----------------------------------------------------------------

/// Receipt identity is opaque to callers. The run journal owns its contents.
pub const OUTCOME: Ty = Ty::Union(&[
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("verdict")),
        Field::new("value", VERDICT),
        Field::new("receipt", STRING),
    ]),
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("low_confidence")),
        Field::new("candidate", VERDICT),
        Field::new("threshold", FLOAT),
        Field::new("receipt", STRING),
    ]),
    REFUSED,
    BUDGET_CUT,
    UNAVAILABLE,
    REPLAY_MISMATCH,
    CANCELLED,
    STATE_TOO_LARGE,
    QUESTION_INVALID,
    RATE_LIMITED,
    OVERLOADED,
]);

/// The batched outcome. `answered` carries every declared question or the
/// evaluation refuses: a partial answer set is `refused/schema_invalid`, never
/// a smaller `answered`.
pub const EVALUATION_OUTCOME: Ty = Ty::Union(&[
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("answered")),
        Field::new("value", DICT_STRING_ANSWER),
        Field::new("receipt", STRING),
    ]),
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("low_confidence")),
        Field::new("candidates", DICT_STRING_ANSWER),
        Field::new("threshold", FLOAT),
        // Which questions fell under the threshold. The rest are in
        // `candidates` at their measured confidence and remain uncertain.
        Field::new("question_ids", LIST_STRING),
        Field::new("receipt", STRING),
    ]),
    REFUSED,
    BUDGET_CUT,
    UNAVAILABLE,
    REPLAY_MISMATCH,
    CANCELLED,
    STATE_TOO_LARGE,
    QUESTION_INVALID,
    RATE_LIMITED,
    OVERLOADED,
]);

/// One policy type serves both entry points. `structured_llm` answers the
/// batch through a generated JSON schema on a chat route; `native_decision`
/// dispatches through the decision operation's own protocol.
pub const POLICY: Ty = Ty::Shape(&[
    Field::new(
        "backend",
        Ty::Union(&[
            Ty::LitString("structured_llm"),
            Ty::LitString("native_decision"),
        ]),
    ),
    Field::new("provider", STRING),
    Field::new("model", STRING),
    Field::new("effort", STRING),
    Field::new("temperature", FLOAT),
    Field::new("threshold", FLOAT),
    Field::new("evaluation_cost_limit", FLOAT),
    Field::new("run_cost_limit", FLOAT),
]);

pub const EVALUATE: BuiltinSignature = BuiltinSignature::simple(
    "__cap_llm_evaluate",
    &[
        Param::new("id", STRING),
        // The checker infers and records the closed type at each call site.
        // `any` here is not permission to pass gradual or opaque inputs.
        Param::new("state", Ty::Any),
        Param::new("questions", DICT_STRING_QUESTION),
        Param::new("policy", POLICY),
    ],
    EVALUATION_OUTCOME,
);

/// Measure a state the way the evaluator's own ceiling measures it.
///
/// The `state_too_large` arm compares a route's declared window against this
/// number, so a caller that sizes its input with anything else is sizing it
/// against a different ruler. Exposing the evaluator's own estimate is what
/// lets a windowing helper promise a fit rather than approximate one.
pub const ESTIMATE_STATE_TOKENS: BuiltinSignature = BuiltinSignature::simple(
    "__cap_llm_estimate_state_tokens",
    &[Param::new("state", Ty::Any)],
    INT,
);

pub const EVALUATE_PREDICATE: BuiltinSignature = BuiltinSignature::simple(
    "__cap_llm_evaluate_predicate",
    &[
        Param::new("id", STRING),
        Param::new("question", STRING),
        Param::new("input", Ty::Any),
        Param::new("policy", POLICY),
    ],
    OUTCOME,
);
