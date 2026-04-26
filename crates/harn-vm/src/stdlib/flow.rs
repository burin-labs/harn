//! Harn-side bindings for Flow predicate result construction.
//!
//! These builtins let `.harn` predicate authors return idiomatic
//! [`InvariantResult`](crate::flow::InvariantResult) values: graded verdicts
//! (`Allow`, `Warn`, `Block`, `RequireApproval`), structured evidence, and
//! optional remediation suggestions.
//!
//! See issue #581 for the type contract and parent epic #571 for the wider
//! Harn Flow shipping substrate. The Rust types live in
//! `crates/harn-vm/src/flow/predicates/result.rs`; this module is a thin layer
//! that constructs equivalent record values via the VM and exposes a few
//! introspection helpers (`flow_invariant_kind`, `flow_invariant_is_blocking`,
//! `flow_invariant_confidence`).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::flow::{
    Approver, ByteSpan, EvidenceItem, InvariantBlockError, InvariantResult, Remediation, Verdict,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_flow_builtins(vm: &mut Vm) {
    vm.register_builtin("flow_invariant_allow", |_args, _out| {
        Ok(InvariantResult::allow().to_vm_value())
    });

    vm.register_builtin("flow_invariant_warn", |args, _out| {
        let reason = required_string(args, 0, "flow_invariant_warn", "reason")?;
        Ok(InvariantResult::warn(reason).to_vm_value())
    });

    vm.register_builtin("flow_invariant_block", |args, _out| {
        let code = required_string(args, 0, "flow_invariant_block", "code")?;
        let message = required_string(args, 1, "flow_invariant_block", "message")?;
        Ok(InvariantResult::block(InvariantBlockError::new(code, message)).to_vm_value())
    });

    vm.register_builtin("flow_invariant_require_approval", |args, _out| {
        let kind = required_string(args, 0, "flow_invariant_require_approval", "kind")?;
        let id = required_string(args, 1, "flow_invariant_require_approval", "id")?;
        let approver = match kind.as_str() {
            "principal" => Approver::principal(id),
            "role" => Approver::role(id),
            other => {
                return Err(VmError::Runtime(format!(
                    "flow_invariant_require_approval: kind must be \"principal\" or \"role\", got \"{other}\""
                )));
            }
        };
        Ok(InvariantResult::require_approval(approver).to_vm_value())
    });

    vm.register_builtin("flow_evidence_atom", |args, _out| {
        let atom_hex = required_string(args, 0, "flow_evidence_atom", "atom_id")?;
        let atom = parse_atom_id(&atom_hex, "flow_evidence_atom")?;
        let start = required_u64(args, 1, "flow_evidence_atom", "diff_start")?;
        let end = required_u64(args, 2, "flow_evidence_atom", "diff_end")?;
        validate_span(start, end, "flow_evidence_atom")?;
        Ok(serde_to_vm(&EvidenceItem::AtomPointer {
            atom,
            diff_span: ByteSpan::new(start, end),
        }))
    });

    vm.register_builtin("flow_evidence_metadata", |args, _out| {
        let directory = required_string(args, 0, "flow_evidence_metadata", "directory")?;
        let namespace = required_string(args, 1, "flow_evidence_metadata", "namespace")?;
        let key = required_string(args, 2, "flow_evidence_metadata", "key")?;
        Ok(serde_to_vm(&EvidenceItem::MetadataPath {
            directory,
            namespace,
            key,
        }))
    });

    vm.register_builtin("flow_evidence_transcript", |args, _out| {
        let transcript_id = required_string(args, 0, "flow_evidence_transcript", "transcript_id")?;
        let start = required_u64(args, 1, "flow_evidence_transcript", "span_start")?;
        let end = required_u64(args, 2, "flow_evidence_transcript", "span_end")?;
        validate_span(start, end, "flow_evidence_transcript")?;
        Ok(serde_to_vm(&EvidenceItem::TranscriptExcerpt {
            transcript_id,
            span: ByteSpan::new(start, end),
        }))
    });

    vm.register_builtin("flow_evidence_citation", |args, _out| {
        let url = required_string(args, 0, "flow_evidence_citation", "url")?;
        let quote = required_string(args, 1, "flow_evidence_citation", "quote")?;
        let fetched_at = required_string(args, 2, "flow_evidence_citation", "fetched_at")?;
        Ok(serde_to_vm(&EvidenceItem::ExternalCitation {
            url,
            quote,
            fetched_at,
        }))
    });

    vm.register_builtin("flow_remediation", |args, _out| {
        let description = required_string(args, 0, "flow_remediation", "description")?;
        Ok(serde_to_vm(&Remediation::describe(description)))
    });

    vm.register_builtin("flow_with_evidence", |args, _out| {
        let mut result = require_invariant(args, 0, "flow_with_evidence")?;
        let list = require_list_arg(args, 1, "flow_with_evidence", "evidence")?;
        let evidence = list
            .iter()
            .map(decode_evidence_item)
            .collect::<Result<Vec<_>, _>>()?;
        result = result.with_evidence(evidence);
        Ok(result.to_vm_value())
    });

    vm.register_builtin("flow_with_remediation", |args, _out| {
        let mut result = require_invariant(args, 0, "flow_with_remediation")?;
        let remediation = decode_remediation(args.get(1).unwrap_or(&VmValue::Nil))?;
        result = result.with_remediation(remediation);
        Ok(result.to_vm_value())
    });

    vm.register_builtin("flow_with_confidence", |args, _out| {
        let mut result = require_invariant(args, 0, "flow_with_confidence")?;
        let confidence = required_f64(args, 1, "flow_with_confidence", "confidence")?;
        result = result.with_confidence(confidence);
        Ok(result.to_vm_value())
    });

    vm.register_builtin("flow_invariant_kind", |args, _out| {
        let result = require_invariant(args, 0, "flow_invariant_kind")?;
        let kind = match &result.verdict {
            Verdict::Allow => "allow",
            Verdict::Warn { .. } => "warn",
            Verdict::Block { .. } => "block",
            Verdict::RequireApproval { .. } => "require_approval",
        };
        Ok(VmValue::String(Rc::from(kind)))
    });

    vm.register_builtin("flow_invariant_is_blocking", |args, _out| {
        let result = require_invariant(args, 0, "flow_invariant_is_blocking")?;
        Ok(VmValue::Bool(result.is_blocking()))
    });

    vm.register_builtin("flow_invariant_confidence", |args, _out| {
        let result = require_invariant(args, 0, "flow_invariant_confidence")?;
        Ok(VmValue::Float(result.confidence))
    });
}

fn serde_to_vm<T: serde::Serialize>(value: &T) -> VmValue {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    crate::stdlib::json_to_vm_value(&json)
}

fn required_string(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required string argument `{name}`"
        ))),
    }
}

fn required_u64(args: &[VmValue], index: usize, builtin: &str, name: &str) -> Result<u64, VmError> {
    match args.get(index) {
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(VmValue::Int(n)) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be non-negative, got {n}"
        ))),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be an int, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required int argument `{name}`"
        ))),
    }
}

fn required_f64(args: &[VmValue], index: usize, builtin: &str, name: &str) -> Result<f64, VmError> {
    match args.get(index) {
        Some(VmValue::Float(n)) => Ok(*n),
        Some(VmValue::Int(n)) => Ok(*n as f64),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a number, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required number argument `{name}`"
        ))),
    }
}

fn require_list_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<&'a [VmValue], VmError> {
    match args.get(index) {
        Some(VmValue::List(items)) => Ok(items.as_slice()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a list, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required list argument `{name}`"
        ))),
    }
}

fn require_invariant(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<InvariantResult, VmError> {
    let value = args.get(index).ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: missing required invariant result argument"
        ))
    })?;
    InvariantResult::from_vm_value(value)
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn parse_atom_id(hex_str: &str, builtin: &str) -> Result<crate::flow::AtomId, VmError> {
    crate::flow::AtomId::from_hex(hex_str)
        .map_err(|error| VmError::Runtime(format!("{builtin}: invalid atom id: {error}")))
}

fn validate_span(start: u64, end: u64, builtin: &str) -> Result<(), VmError> {
    if end < start {
        return Err(VmError::Runtime(format!(
            "{builtin}: span end ({end}) must be >= start ({start})"
        )));
    }
    Ok(())
}

fn decode_evidence_item(value: &VmValue) -> Result<EvidenceItem, VmError> {
    let json = value_to_json(value);
    serde_json::from_value(json)
        .map_err(|error| VmError::Runtime(format!("flow_with_evidence: invalid evidence: {error}")))
}

fn decode_remediation(value: &VmValue) -> Result<Remediation, VmError> {
    let json = value_to_json(value);
    serde_json::from_value(json).map_err(|error| {
        VmError::Runtime(format!(
            "flow_with_remediation: invalid remediation: {error}"
        ))
    })
}

fn value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::Value::from(*n),
        VmValue::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        VmValue::Dict(map) => {
            let mut object = BTreeMap::new();
            for (key, item) in map.iter() {
                object.insert(key.clone(), value_to_json(item));
            }
            serde_json::Value::Object(object.into_iter().collect())
        }
        other => serde_json::Value::String(other.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;

    fn vm_with_flow_builtins() -> Vm {
        let mut vm = Vm::new();
        register_flow_builtins(&mut vm);
        vm
    }

    fn call(vm: &Vm, name: &str, args: &[VmValue]) -> VmValue {
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get(name)
            .unwrap_or_else(|| panic!("builtin {name} not registered"))
            .clone();
        builtin(args, &mut out).expect("builtin call failed")
    }

    #[test]
    fn flow_invariant_allow_returns_dict_round_trippable() {
        let vm = vm_with_flow_builtins();
        let result = call(&vm, "flow_invariant_allow", &[]);
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        assert_eq!(decoded, InvariantResult::allow());
    }

    #[test]
    fn flow_invariant_warn_carries_reason() {
        let vm = vm_with_flow_builtins();
        let result = call(
            &vm,
            "flow_invariant_warn",
            &[VmValue::String(Rc::from("untested helper"))],
        );
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        match decoded.verdict {
            Verdict::Warn { reason } => assert_eq!(reason, "untested helper"),
            other => panic!("expected warn verdict, got {other:?}"),
        }
    }

    #[test]
    fn flow_invariant_block_carries_code_and_message() {
        let vm = vm_with_flow_builtins();
        let result = call(
            &vm,
            "flow_invariant_block",
            &[
                VmValue::String(Rc::from("missing_test")),
                VmValue::String(Rc::from("no test covers this atom")),
            ],
        );
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        assert!(decoded.is_blocking());
        let error = decoded.block_error().unwrap();
        assert_eq!(error.code, "missing_test");
        assert_eq!(error.message, "no test covers this atom");
    }

    #[test]
    fn flow_invariant_require_approval_routes_to_principal_or_role() {
        let vm = vm_with_flow_builtins();
        let principal_value = call(
            &vm,
            "flow_invariant_require_approval",
            &[
                VmValue::String(Rc::from("principal")),
                VmValue::String(Rc::from("user:alice")),
            ],
        );
        let role_value = call(
            &vm,
            "flow_invariant_require_approval",
            &[
                VmValue::String(Rc::from("role")),
                VmValue::String(Rc::from("security-reviewer")),
            ],
        );
        let principal = InvariantResult::from_vm_value(&principal_value).unwrap();
        let role = InvariantResult::from_vm_value(&role_value).unwrap();
        assert_eq!(
            principal.verdict,
            Verdict::RequireApproval {
                approver: Approver::principal("user:alice"),
            }
        );
        assert_eq!(
            role.verdict,
            Verdict::RequireApproval {
                approver: Approver::role("security-reviewer"),
            }
        );
    }

    #[test]
    fn flow_invariant_require_approval_rejects_unknown_kind() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get("flow_invariant_require_approval")
            .unwrap()
            .clone();
        let error = builtin(
            &[
                VmValue::String(Rc::from("squad")),
                VmValue::String(Rc::from("ship-captains")),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("kind must be"));
    }

    #[test]
    fn flow_with_evidence_attaches_all_four_evidence_kinds() {
        let vm = vm_with_flow_builtins();
        let allow = call(&vm, "flow_invariant_allow", &[]);
        let atom_hex = "01".repeat(32);
        let atom_evidence = call(
            &vm,
            "flow_evidence_atom",
            &[
                VmValue::String(Rc::from(atom_hex.as_str())),
                VmValue::Int(0),
                VmValue::Int(64),
            ],
        );
        let metadata_evidence = call(
            &vm,
            "flow_evidence_metadata",
            &[
                VmValue::String(Rc::from("src/auth")),
                VmValue::String(Rc::from("policy")),
                VmValue::String(Rc::from("min_review_count")),
            ],
        );
        let transcript_evidence = call(
            &vm,
            "flow_evidence_transcript",
            &[
                VmValue::String(Rc::from("transcript-0001")),
                VmValue::Int(128),
                VmValue::Int(256),
            ],
        );
        let citation_evidence = call(
            &vm,
            "flow_evidence_citation",
            &[
                VmValue::String(Rc::from("https://harnlang.com/spec")),
                VmValue::String(Rc::from("verdicts may grade")),
                VmValue::String(Rc::from("2026-04-26T00:00:00Z")),
            ],
        );
        let evidence_list = VmValue::List(Rc::new(vec![
            atom_evidence,
            metadata_evidence,
            transcript_evidence,
            citation_evidence,
        ]));
        let attached = call(&vm, "flow_with_evidence", &[allow, evidence_list]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(decoded.evidence.len(), 4);
        assert!(matches!(
            decoded.evidence[0],
            EvidenceItem::AtomPointer { .. }
        ));
        assert!(matches!(
            decoded.evidence[1],
            EvidenceItem::MetadataPath { .. }
        ));
        assert!(matches!(
            decoded.evidence[2],
            EvidenceItem::TranscriptExcerpt { .. }
        ));
        assert!(matches!(
            decoded.evidence[3],
            EvidenceItem::ExternalCitation { .. }
        ));
    }

    #[test]
    fn flow_with_remediation_attaches_description() {
        let vm = vm_with_flow_builtins();
        let block = call(
            &vm,
            "flow_invariant_block",
            &[
                VmValue::String(Rc::from("style")),
                VmValue::String(Rc::from("trailing whitespace")),
            ],
        );
        let remediation = call(
            &vm,
            "flow_remediation",
            &[VmValue::String(Rc::from("strip trailing whitespace"))],
        );
        let attached = call(&vm, "flow_with_remediation", &[block, remediation]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(
            decoded.remediation.unwrap().description,
            "strip trailing whitespace"
        );
    }

    #[test]
    fn flow_with_confidence_clamps_to_unit_interval() {
        let vm = vm_with_flow_builtins();
        let warn = call(
            &vm,
            "flow_invariant_warn",
            &[VmValue::String(Rc::from("low signal"))],
        );
        let attached = call(&vm, "flow_with_confidence", &[warn, VmValue::Float(1.5)]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(decoded.confidence, 1.0);
    }

    #[test]
    fn flow_invariant_kind_returns_string_label() {
        let vm = vm_with_flow_builtins();
        let allow = call(&vm, "flow_invariant_allow", &[]);
        let kind = call(&vm, "flow_invariant_kind", &[allow]);
        match kind {
            VmValue::String(s) => assert_eq!(s.as_ref(), "allow"),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn flow_evidence_atom_rejects_invalid_span() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
        let atom_hex = "ab".repeat(32);
        let error = builtin(
            &[
                VmValue::String(Rc::from(atom_hex.as_str())),
                VmValue::Int(64),
                VmValue::Int(32),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("must be >="));
    }

    #[test]
    fn flow_evidence_atom_rejects_bad_hex() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
        let error = builtin(
            &[
                VmValue::String(Rc::from("not-hex")),
                VmValue::Int(0),
                VmValue::Int(8),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("invalid atom id"));
    }
}
