//! Harn Flow predicate result builtin signatures.
//!
//! Constructors and introspectors for `InvariantResult`, the value
//! shape predicates return — graded verdicts (`Allow`/`Warn`/`Block`/
//! `RequireApproval`), structured evidence, optional remediation, and a
//! confidence scalar. See issue #581 and the runtime registrations in
//! `crates/harn-vm/src/stdlib/flow.rs`.

use super::{BuiltinSignature, Param, Ty, TY_BOOL, TY_DICT, TY_FLOAT, TY_LIST, TY_STRING};

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature {
        name: "flow_evidence_atom",
        params: &[
            Param::new("atom_id", TY_STRING),
            Param::new("diff_start", Ty::Named("int")),
            Param::new("diff_end", Ty::Named("int")),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_evidence_citation",
        params: &[
            Param::new("url", TY_STRING),
            Param::new("quote", TY_STRING),
            Param::new("fetched_at", TY_STRING),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_evidence_metadata",
        params: &[
            Param::new("directory", TY_STRING),
            Param::new("namespace", TY_STRING),
            Param::new("key", TY_STRING),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_evidence_transcript",
        params: &[
            Param::new("transcript_id", TY_STRING),
            Param::new("span_start", Ty::Named("int")),
            Param::new("span_end", Ty::Named("int")),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_allow",
        params: &[],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_block",
        params: &[
            Param::new("code", TY_STRING),
            Param::new("message", TY_STRING),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_confidence",
        params: &[Param::new("result", TY_DICT)],
        returns: TY_FLOAT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_is_blocking",
        params: &[Param::new("result", TY_DICT)],
        returns: TY_BOOL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_kind",
        params: &[Param::new("result", TY_DICT)],
        returns: TY_STRING,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_require_approval",
        params: &[Param::new("kind", TY_STRING), Param::new("id", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_invariant_warn",
        params: &[Param::new("reason", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_remediation",
        params: &[Param::new("description", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_with_confidence",
        params: &[
            Param::new("result", TY_DICT),
            Param::new("confidence", Ty::Union(&[TY_FLOAT, Ty::Named("int")])),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_with_evidence",
        params: &[
            Param::new("result", TY_DICT),
            Param::new("evidence", TY_LIST),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "flow_with_remediation",
        params: &[
            Param::new("result", TY_DICT),
            Param::new("remediation", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
];
