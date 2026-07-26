//! Correction-record builtins: capturing operator corrections and querying them.
//!
//! Same surface in two spellings — the flat `correction_*` builtins and the
//! `corrections.*` namespace. The record and filter shapes themselves are owned
//! by the crate root; this module only bridges Harn values into them.

use std::collections::BTreeMap;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::args::value_from_serde;
use super::journal::ensure_trigger_event_log;

#[harn_builtin(
    sig = "correction_record(correction: dict) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn correction_record_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let value = args.first().ok_or_else(|| {
        VmError::Runtime("correction_record: expected correction dict".to_string())
    })?;
    let record = correction_record_from_value("correction_record", value)?;
    let log = ensure_trigger_event_log();
    let record = crate::append_correction_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("correction_record: {error}")))?;
    Ok(value_from_serde(&record))
}

#[harn_builtin(
    sig = "correction_query(filters?: dict) -> list",
    kind = "async",
    category = "triggers"
)]
async fn correction_query_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let filters = args
        .first()
        .map(|value| correction_query_filters_from_value("correction_query", value))
        .transpose()?
        .unwrap_or_default();
    let log = ensure_trigger_event_log();
    let records = crate::query_correction_records(&log, &filters)
        .await
        .map_err(|error| VmError::Runtime(format!("correction_query: {error}")))?;
    Ok(VmValue::List(std::sync::Arc::new(
        records
            .into_iter()
            .map(|record| value_from_serde(&record))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "corrections.record(correction: dict) -> string",
    kind = "async",
    category = "triggers"
)]
async fn corrections_record_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let value = args.first().ok_or_else(|| {
        VmError::Runtime("corrections.record: expected correction dict".to_string())
    })?;
    let record = correction_record_from_value("corrections.record", value)?;
    let log = ensure_trigger_event_log();
    let record = crate::append_correction_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("corrections.record: {error}")))?;
    Ok(VmValue::String(arcstr::ArcStr::from(record.correction_id)))
}

#[harn_builtin(
    sig = "corrections.query(filters?: dict) -> list",
    kind = "async",
    category = "triggers"
)]
async fn corrections_query_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let filters = args
        .first()
        .map(|value| correction_query_filters_from_value("corrections.query", value))
        .transpose()?
        .unwrap_or_default();
    let log = ensure_trigger_event_log();
    let records = crate::query_correction_records(&log, &filters)
        .await
        .map_err(|error| VmError::Runtime(format!("corrections.query: {error}")))?;
    Ok(VmValue::List(std::sync::Arc::new(
        records
            .into_iter()
            .map(|record| value_from_serde(&record))
            .collect(),
    )))
}

pub(super) fn register_corrections_namespace(vm: &mut Vm) {
    let names = ["query", "record"];
    vm.set_global(
        "corrections",
        VmValue::dict(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(arcstr::ArcStr::from("corrections")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(format!("corrections.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        ),
    );
}

fn correction_record_from_value(
    builtin: &str,
    value: &VmValue,
) -> Result<crate::CorrectionRecord, VmError> {
    crate::correction_record_from_json(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn correction_query_filters_from_value(
    builtin: &str,
    value: &VmValue,
) -> Result<crate::CorrectionQueryFilters, VmError> {
    crate::correction_query_filters_from_json(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}
