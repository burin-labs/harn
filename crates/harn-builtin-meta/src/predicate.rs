//! The predicate evaluation boundary shared by checking and execution.
//!
//! This is a capability method, not a second expression grammar. The checker
//! supplements the ordinary signature with literal site identity, closed data
//! input, and outcome consumption checks. Execution remains unavailable until
//! the evaluator implements reservation, replay, and receipt ownership.

use crate::{BuiltinSignature, Param, ShapeFieldDescriptor as Field, Ty};

const STRING: Ty = Ty::Named("string");
const FLOAT: Ty = Ty::Named("float");
const INT: Ty = Ty::Named("int");

pub const VERDICT: Ty = Ty::Shape(&[
    Field::new("verdict", Ty::Named("bool")),
    Field::new("confidence", FLOAT),
    Field::new("evidence", STRING),
]);

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
    Ty::Shape(&[
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
    ]),
    Ty::Shape(&[
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
    ]),
    Ty::Shape(&[
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
    ]),
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("replay_mismatch")),
        Field::new("expected_identity", STRING),
        Field::new("actual_identity", STRING),
        Field::new("occurrence", INT),
        Field::new("receipt", STRING),
    ]),
    Ty::Shape(&[
        Field::new("kind", Ty::LitString("cancelled")),
        Field::new("control_event", STRING),
        Field::new("receipt", STRING),
    ]),
]);

/// The first executor profile is structured generation. Native decision
/// options join this union only with an implemented operation contract.
pub const POLICY: Ty = Ty::Shape(&[
    Field::new("backend", Ty::LitString("structured_llm")),
    Field::new("provider", STRING),
    Field::new("model", STRING),
    Field::new("effort", STRING),
    Field::new("temperature", FLOAT),
    Field::new("threshold", FLOAT),
    Field::new("evaluation_cost_limit", FLOAT),
    Field::new("run_cost_limit", FLOAT),
]);

pub const EVALUATE: BuiltinSignature = BuiltinSignature::simple(
    "__cap_llm_evaluate_predicate",
    &[
        Param::new("id", STRING),
        Param::new("question", STRING),
        // The checker infers and records the closed type at each call site.
        // `any` here is not permission to pass gradual or opaque inputs.
        Param::new("input", Ty::Any),
        Param::new("policy", POLICY),
    ],
    OUTCOME,
);
