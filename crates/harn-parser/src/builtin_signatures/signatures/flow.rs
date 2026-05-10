//! Harn Flow predicate result builtin signatures.
//!
//! Constructors and introspectors for `InvariantResult`, the value
//! shape predicates return — graded verdicts (`Allow`/`Warn`/`Block`/
//! `RequireApproval`), structured evidence, optional remediation, and a
//! confidence scalar. See issue #581 and the runtime registrations in
//! `crates/harn-vm/src/stdlib/flow.rs`.

use super::{BuiltinSignature, Param, Ty, TY_BOOL, TY_DICT, TY_FLOAT, TY_LIST, TY_STRING};

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature::simple(
        "flow_evidence_atom",
        &[
            Param::new("atom_id", TY_STRING),
            Param::new("diff_start", Ty::Named("int")),
            Param::new("diff_end", Ty::Named("int")),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_evidence_citation",
        &[
            Param::new("url", TY_STRING),
            Param::new("quote", TY_STRING),
            Param::new("fetched_at", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_evidence_metadata",
        &[
            Param::new("directory", TY_STRING),
            Param::new("namespace", TY_STRING),
            Param::new("key", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_evidence_transcript",
        &[
            Param::new("transcript_id", TY_STRING),
            Param::new("span_start", Ty::Named("int")),
            Param::new("span_end", Ty::Named("int")),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("flow_invariant_allow", &[], TY_DICT),
    BuiltinSignature::simple(
        "flow_invariant_block",
        &[
            Param::new("code", TY_STRING),
            Param::new("message", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_invariant_confidence",
        &[Param::new("result", TY_DICT)],
        TY_FLOAT,
    ),
    BuiltinSignature::simple(
        "flow_invariant_is_blocking",
        &[Param::new("result", TY_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "flow_invariant_kind",
        &[Param::new("result", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "flow_invariant_require_approval",
        &[Param::new("kind", TY_STRING), Param::new("id", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_invariant_warn",
        &[Param::new("reason", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_remediation",
        &[Param::new("description", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_with_confidence",
        &[
            Param::new("result", TY_DICT),
            Param::new("confidence", Ty::Union(&[TY_FLOAT, Ty::Named("int")])),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_with_evidence",
        &[
            Param::new("result", TY_DICT),
            Param::new("evidence", TY_LIST),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "flow_with_remediation",
        &[
            Param::new("result", TY_DICT),
            Param::new("remediation", TY_DICT),
        ],
        TY_DICT,
    ),
];
