//! Harn Flow predicate result builtin signatures.
//!
//! Constructors and introspectors for `InvariantResult`, the value
//! shape predicates return — graded verdicts (`Allow`/`Warn`/`Block`/
//! `RequireApproval`), structured evidence, optional remediation, and a
//! confidence scalar. See issue #581 and the runtime registrations in
//! `crates/harn-vm/src/stdlib/flow.rs`.

use super::{BuiltinReturn, BuiltinSig};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "flow_evidence_atom",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_evidence_citation",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_evidence_metadata",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_evidence_transcript",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_invariant_allow",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_invariant_block",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_invariant_confidence",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "flow_invariant_is_blocking",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "flow_invariant_kind",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "flow_invariant_require_approval",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_invariant_warn",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_remediation",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_with_confidence",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_with_evidence",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "flow_with_remediation",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
];
