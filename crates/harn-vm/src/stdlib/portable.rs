//! Irreducible Harn bindings for the Portable Harn Kernel.
//!
//! The public composition layer lives in `std/portable`. These builtins only
//! cross the VM value boundary, compile or decode the versioned artifact, and
//! enter the kernel's deterministic start/resume machine.

use std::sync::Arc;

use harn_kernel::{
    CapabilityRequest, CapabilityResult, DataValue, Diagnostic, EntryKind, Execution, GrantSet,
    ValueShape,
};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const PROGRAM_SCHEMA: &str = "harn.portable_program.v1";

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &PORTABLE_COMPILE_IMPL_DEF,
    &PORTABLE_START_IMPL_DEF,
    &PORTABLE_RESUME_IMPL_DEF,
];

pub(crate) fn register_portable_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__portable_compile(source: string, entry: string, kind: string) -> dict",
    category = "portable"
)]
fn portable_compile_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = expect_string(args, 0, "__portable_compile", "source")?;
    let entry = expect_string(args, 1, "__portable_compile", "entry")?;
    let kind_name = expect_string(args, 2, "__portable_compile", "kind")?;
    let kind = match kind_name.parse::<EntryKind>() {
        Ok(kind) => kind,
        Err(diagnostic) => return Ok(compile_failure(vec![diagnostic])),
    };

    Ok(match harn_kernel::compile_program(source, entry, kind) {
        Ok(program) => VmValue::dict([
            ("ok", VmValue::Bool(true)),
            (
                "program",
                VmValue::dict([
                    ("schema", VmValue::String(PROGRAM_SCHEMA.into())),
                    (
                        "artifact",
                        VmValue::Bytes(Arc::new(program.bytes().to_vec())),
                    ),
                    ("digest", VmValue::String(program.digest_hex().into())),
                    ("entry", VmValue::String(entry.into())),
                    ("kind", VmValue::String(kind_name.into())),
                ]),
            ),
            ("diagnostics", VmValue::List(Arc::new(Vec::new()))),
        ]),
        Err(diagnostics) => compile_failure(diagnostics),
    })
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__portable_start(artifact: bytes, input: any, grants: dict) -> any",
    category = "portable"
)]
fn portable_start_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let artifact = expect_bytes(args, 0, "__portable_start", "artifact")?;
    let input = data_value(args.get(1), "__portable_start", "input")?;
    let grants = grant_set(args.get(2), "__portable_start")?;
    let execution = match crate::portable::start(artifact, input, &grants) {
        Ok(execution) => execution,
        Err(diagnostic) => Execution::Failed { diagnostic },
    };
    Ok(execution_value(execution))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__portable_resume(artifact: bytes, snapshot: bytes, outcome: dict, grants: dict) -> any",
    category = "portable"
)]
fn portable_resume_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let artifact = expect_bytes(args, 0, "__portable_resume", "artifact")?;
    let snapshot = expect_bytes(args, 1, "__portable_resume", "snapshot")?;
    let outcome_json = strict_json(args.get(2), "__portable_resume", "outcome")?;
    let outcome: CapabilityResult = serde_json::from_value(outcome_json)
        .map_err(|error| builtin_error("__portable_resume", format!("invalid outcome: {error}")))?;
    let grants = grant_set(args.get(3), "__portable_resume")?;
    let execution = match crate::portable::resume(artifact, snapshot, outcome, &grants) {
        Ok(execution) => execution,
        Err(diagnostic) => Execution::Failed { diagnostic },
    };
    Ok(execution_value(execution))
}

fn compile_failure(diagnostics: Vec<Diagnostic>) -> VmValue {
    VmValue::dict([
        ("ok", VmValue::Bool(false)),
        ("program", VmValue::Nil),
        (
            "diagnostics",
            VmValue::List(Arc::new(
                diagnostics.into_iter().map(diagnostic_value).collect(),
            )),
        ),
    ])
}

fn execution_value(execution: Execution) -> VmValue {
    match execution {
        Execution::Completed { value } => VmValue::dict([
            ("status", VmValue::String("completed".into())),
            ("value", data_vm_value(value)),
        ]),
        Execution::Suspended { request, snapshot } => VmValue::dict([
            ("status", VmValue::String("suspended".into())),
            ("request", request_value(request)),
            ("snapshot", VmValue::Bytes(Arc::new(snapshot))),
        ]),
        Execution::Failed { diagnostic } => VmValue::dict([
            ("status", VmValue::String("failed".into())),
            ("diagnostic", diagnostic_value(diagnostic)),
        ]),
    }
}

fn request_value(request: CapabilityRequest) -> VmValue {
    VmValue::dict([
        ("id", VmValue::String(request.id.into())),
        ("capability", VmValue::String(request.capability.into())),
        ("operation", VmValue::String(request.operation.into())),
        ("arguments", data_vm_value(request.arguments)),
        (
            "expected",
            VmValue::String(value_shape_name(request.expected).into()),
        ),
    ])
}

fn value_shape_name(shape: ValueShape) -> &'static str {
    match shape {
        ValueShape::Any => "any",
        ValueShape::Nil => "nil",
        ValueShape::Bool => "bool",
        ValueShape::Int => "int",
        ValueShape::Float => "float",
        ValueShape::String => "string",
        ValueShape::Bytes => "bytes",
        ValueShape::List => "list",
        ValueShape::Record => "record",
    }
}

fn diagnostic_value(diagnostic: Diagnostic) -> VmValue {
    VmValue::dict([
        ("code", VmValue::String(diagnostic.code.into())),
        ("message", VmValue::String(diagnostic.message.into())),
        (
            "line",
            diagnostic
                .line
                .map(|value| VmValue::Int(i64::from(value)))
                .unwrap_or(VmValue::Nil),
        ),
        (
            "column",
            diagnostic
                .column
                .map(|value| VmValue::Int(i64::from(value)))
                .unwrap_or(VmValue::Nil),
        ),
    ])
}

fn data_vm_value(value: DataValue) -> VmValue {
    match value {
        DataValue::Nil => VmValue::Nil,
        DataValue::Bool(value) => VmValue::Bool(value),
        DataValue::Int(value) => VmValue::Int(value),
        DataValue::Float(value) => VmValue::Float(value),
        DataValue::String(value) => VmValue::String(value.into()),
        DataValue::Bytes(value) => VmValue::Bytes(Arc::new(value)),
        DataValue::List(values) => {
            VmValue::List(Arc::new(values.into_iter().map(data_vm_value).collect()))
        }
        DataValue::Record(entries) => VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (key, data_vm_value(value))),
        ),
    }
}

fn data_value(value: Option<&VmValue>, builtin: &str, name: &str) -> Result<DataValue, VmError> {
    let json = strict_json(value, builtin, name)?;
    DataValue::from_json(json).map_err(|diagnostic| {
        builtin_error(
            builtin,
            format!("{}: {}", diagnostic.code, diagnostic.message),
        )
    })
}

fn strict_json(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<serde_json::Value, VmError> {
    let value = value.ok_or_else(|| builtin_error(builtin, format!("missing {name}")))?;
    crate::llm::vm_value_to_json_strict(value, name).map_err(|error| builtin_error(builtin, error))
}

fn grant_set(value: Option<&VmValue>, builtin: &str) -> Result<GrantSet, VmError> {
    let value = value.ok_or_else(|| builtin_error(builtin, "missing grants"))?;
    let (capabilities, snapshot_key) = match value {
        VmValue::Dict(record) => {
            let capabilities = match record.get("capabilities") {
                Some(VmValue::List(values)) => string_list(values, builtin, "capabilities")?,
                Some(other) => {
                    return Err(builtin_error(
                        builtin,
                        format!("capabilities must be a list, got {}", other.type_name()),
                    ));
                }
                None => Vec::new(),
            };
            let snapshot_key = match record.get("snapshot_key") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::Bytes(value)) if value.len() == 32 => {
                    let mut key = [0_u8; 32];
                    key.copy_from_slice(value);
                    Some(key)
                }
                Some(VmValue::Bytes(_)) => {
                    return Err(builtin_error(
                        builtin,
                        "snapshot_key must contain exactly 32 bytes",
                    ));
                }
                Some(other) => {
                    return Err(builtin_error(
                        builtin,
                        format!("snapshot_key must be bytes, got {}", other.type_name()),
                    ));
                }
            };
            (capabilities, snapshot_key)
        }
        other => {
            return Err(builtin_error(
                builtin,
                format!("grants must be a record, got {}", other.type_name()),
            ));
        }
    };
    let grants = GrantSet::from_names(capabilities).map_err(|diagnostic| {
        builtin_error(
            builtin,
            format!("{}: {}", diagnostic.code, diagnostic.message),
        )
    })?;
    Ok(match snapshot_key {
        Some(key) => grants.with_snapshot_key(key),
        None => grants,
    })
}

fn string_list(values: &[VmValue], builtin: &str, name: &str) -> Result<Vec<String>, VmError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| match value {
            VmValue::String(value) => Ok(value.to_string()),
            other => Err(builtin_error(
                builtin,
                format!(
                    "{name}[{index}] must be a string, got {}",
                    other.type_name()
                ),
            )),
        })
        .collect()
}

fn expect_string<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value),
        Some(other) => Err(builtin_error(
            builtin,
            format!("{name} must be a string, got {}", other.type_name()),
        )),
        None => Err(builtin_error(builtin, format!("missing {name}"))),
    }
}

fn expect_bytes<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<&'a [u8], VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(value)) => Ok(value),
        Some(other) => Err(builtin_error(
            builtin,
            format!("{name} must be bytes, got {}", other.type_name()),
        )),
        None => Err(builtin_error(builtin, format!("missing {name}"))),
    }
}

fn builtin_error(builtin: &str, message: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("{builtin}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(value: &str) -> VmValue {
        VmValue::String(value.into())
    }

    fn assert_string(value: Option<&VmValue>, expected: &str) {
        assert!(matches!(value, Some(VmValue::String(actual)) if actual.as_str() == expected));
    }

    #[test]
    fn builtins_keep_artifact_and_values_typed() {
        let compiled = portable_compile_impl(
            &[
                string("fn add(input: int) -> int { return input + 2 }"),
                string("add"),
                string("function"),
            ],
            &mut String::new(),
        )
        .expect("compile succeeds");
        let program = compiled
            .as_dict()
            .and_then(|value| value.get("program"))
            .and_then(VmValue::as_dict)
            .expect("typed program");
        let artifact = program.get("artifact").cloned().expect("artifact");
        let invalid_grants = portable_start_impl(
            &[
                artifact.clone(),
                VmValue::Int(40),
                VmValue::List(Arc::new(vec![])),
            ],
            &mut String::new(),
        )
        .expect_err("bare grant lists are rejected");
        assert!(invalid_grants
            .to_string()
            .contains("grants must be a record"));
        let execution = portable_start_impl(
            &[
                artifact,
                VmValue::Int(40),
                VmValue::dict([("capabilities", VmValue::List(Arc::new(vec![])))]),
            ],
            &mut String::new(),
        )
        .expect("start succeeds");
        let execution = execution.as_dict().expect("execution record");
        assert_string(execution.get("status"), "completed");
        assert!(matches!(execution.get("value"), Some(VmValue::Int(42))));
    }

    #[test]
    fn denied_capability_is_a_structured_execution_failure() {
        let compiled = portable_compile_impl(
            &[
                string(
                    "fn ask(harness: Harness, input: string) -> string { return harness.interaction.ask(input) }",
                ),
                string("ask"),
                string("function"),
            ],
            &mut String::new(),
        )
        .expect("compile succeeds");
        let artifact = compiled
            .as_dict()
            .and_then(|value| value.get("program"))
            .and_then(VmValue::as_dict)
            .and_then(|value| value.get("artifact"))
            .cloned()
            .expect("artifact");
        let execution = portable_start_impl(
            &[
                artifact,
                string("Continue?"),
                VmValue::dict([("capabilities", VmValue::List(Arc::new(vec![])))]),
            ],
            &mut String::new(),
        )
        .expect("start returns failure");
        let execution = execution.as_dict().expect("execution record");
        assert_string(execution.get("status"), "failed");
        let code = execution
            .get("diagnostic")
            .and_then(VmValue::as_dict)
            .and_then(|value| value.get("code"));
        assert_string(code, "capability_denied");
    }
}
