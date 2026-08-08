use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::bridge::json_result_to_vm_value;
use crate::value::{VmClosure, VmValue};
use crate::vm::Vm;
use crate::ConnectorError;

const METADATA_EXPORTS: [&str; 3] = ["provider_id", "kinds", "payload_schema"];

// Metadata exports are pure and deliberately absent. This is the one
// authoritative registry for exports the connector runtime invokes with root
// authority. Contract validation and language tooling project from it.
const RUNTIME_EXPORTS: [(&str, usize); 6] = [
    ("init", 1),
    ("activate", 1),
    ("shutdown", 0),
    ("normalize_inbound", 1),
    ("call", 2),
    ("poll_tick", 1),
];

/// Whether `name` is invoked by the connector runtime with root authority.
pub fn is_runtime_export(name: &str) -> bool {
    RUNTIME_EXPORTS
        .iter()
        .any(|(export, _host_arg_count)| *export == name)
}

/// Pure metadata exports required to identify a Harn connector module.
pub const fn metadata_exports() -> &'static [&'static str] {
    &METADATA_EXPORTS
}

pub(super) fn validate_runtime_export_abi(
    exports: &BTreeMap<String, Arc<VmClosure>>,
) -> Result<(), ConnectorError> {
    // Every runtime export receives root authority first; helpers inside the
    // connector should immediately attenuate it to the nominal handles they
    // actually need.
    for (name, host_arg_count) in RUNTIME_EXPORTS {
        let Some(closure) = exports.get(name) else {
            continue;
        };
        let function = closure.func.as_ref();
        let first_is_harness = function.params.first().is_some_and(|param| {
            matches!(
                param.type_expr.as_ref(),
                Some(harn_parser::TypeExpr::Named(kind)) if kind == "Harness"
            )
        });
        if !first_is_harness {
            return Err(ConnectorError::HarnRuntime(format!(
                "connector runtime export '{name}' must declare `harness: Harness` \
                 as its first parameter; metadata exports remain pure"
            )));
        }
        let supplied = host_arg_count + 1;
        let accepts_supplied = function.minimum_arg_count() <= supplied
            && (function.has_rest_param || supplied <= function.params.len());
        if !accepts_supplied {
            return Err(ConnectorError::HarnRuntime(format!(
                "connector runtime export '{name}' must accept {supplied} arguments \
                 including the leading Harness; found {} parameter(s)",
                function.params.len()
            )));
        }
    }
    Ok(())
}

pub(super) fn runtime_export_args(
    vm: &Vm,
    name: &str,
    args: Vec<JsonValue>,
) -> Result<Vec<VmValue>, ConnectorError> {
    // Runtime connector exports are effectful execution boundaries. Their
    // first argument is always the embedder-owned, unforgeable root Harness.
    let harness = vm.root_harness_value().ok_or_else(|| {
        ConnectorError::HarnRuntime(format!(
            "connector export '{name}' requires an installed root Harness"
        ))
    })?;
    let mut vm_args = Vec::with_capacity(args.len() + 1);
    vm_args.push(harness);
    vm_args.extend(
        args.into_iter()
            .map(|value| json_result_to_vm_value(&value)),
    );
    Ok(vm_args)
}
