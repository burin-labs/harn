//! Read-only Harn VM bridge for machine-global host leases.
//!
//! The lease registry and stale-recovery policy remain in [`crate::host_lease`].
//! This module owns only the Harn host-capability boundary: request validation,
//! explicit wire shaping, and backend error translation. Keeping the bridge
//! separate prevents product or orchestration callers from learning the SQLite
//! layout or parsing the `harn host lease` CLI envelope.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::host_lease::{HostLeaseHandle, HostLeaseState, HostLeaseStore};
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::args::{build_dict, dict_arg, require_string, str_value};

const STATUS_BUILTIN: &str = "hostlib_host_lease_status";

/// Read-only access to the authoritative local host-lease state.
///
/// The caller must name a local resource explicitly. This capability does not
/// invent remote observation, and it deliberately does not acquire, renew, or
/// release leases; those lifecycle operations need their own cancellation-safe
/// scope boundary.
#[derive(Default)]
pub struct HostLeaseCapability;

impl HostlibCapability for HostLeaseCapability {
    fn module_name(&self) -> &'static str {
        "host_lease"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_fn("host_lease", STATUS_BUILTIN, "status", handle_status);
    }
}

fn handle_status(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(STATUS_BUILTIN, args)?;
    let host = require_nonempty_string(STATUS_BUILTIN, &dict, "host")?;
    let state = HostLeaseStore::from_env()
        .and_then(|store| store.status(&host))
        .map_err(|error| HostlibError::Backend {
            builtin: STATUS_BUILTIN,
            message: error.to_string(),
        })?;
    state_to_value(&state)
}

fn require_nonempty_string(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<String, HostlibError> {
    let value = require_string(builtin, dict, key)?;
    if value.trim().is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must be a non-empty string".to_string(),
        });
    }
    Ok(value)
}

fn state_to_value(state: &HostLeaseState) -> Result<VmValue, HostlibError> {
    let active = match state.active.as_ref() {
        Some(handle) => handle_to_value(handle)?,
        None => VmValue::Nil,
    };
    Ok(build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(state.schema_version)),
        ),
        ("host", str_value(&state.host)),
        ("observed_at_ms", VmValue::Int(state.observed_at_ms)),
        ("active", active),
        (
            "recovered_stale_lease",
            VmValue::Bool(state.recovered_stale_lease),
        ),
    ]))
}

fn handle_to_value(handle: &HostLeaseHandle) -> Result<VmValue, HostlibError> {
    let owner_process_identity = match handle.owner_process_identity {
        Some(identity) => {
            VmValue::Int(i64::try_from(identity).map_err(|_| HostlibError::Backend {
                builtin: STATUS_BUILTIN,
                message: "owner process identity exceeds the Harn integer range".to_string(),
            })?)
        }
        None => VmValue::Nil,
    };
    let metadata = build_dict(
        handle
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), str_value(value))),
    );
    Ok(build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(handle.schema_version)),
        ),
        ("host", str_value(&handle.host)),
        ("lease_id", str_value(&handle.lease_id)),
        ("owner", str_value(&handle.owner)),
        ("priority_class", str_value(handle.priority_class.as_str())),
        ("acquired_at_ms", VmValue::Int(handle.acquired_at_ms)),
        ("updated_at_ms", VmValue::Int(handle.updated_at_ms)),
        (
            "expires_at_ms",
            handle
                .expires_at_ms
                .map(VmValue::Int)
                .unwrap_or(VmValue::Nil),
        ),
        (
            "owner_pid",
            handle
                .owner_pid
                .map(i64::from)
                .map(VmValue::Int)
                .unwrap_or(VmValue::Nil),
        ),
        ("owner_process_identity", owner_process_identity),
        (
            "reason",
            handle
                .reason
                .as_deref()
                .map(str_value)
                .unwrap_or(VmValue::Nil),
        ),
        ("metadata", metadata),
    ]))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::host_lease::{HostLeasePriorityClass, HostLeaseResourceClass};

    #[test]
    fn status_value_preserves_active_and_recovery_evidence() {
        let state = HostLeaseState {
            schema_version: 1,
            host: "mac-local".to_string(),
            resource_class: HostLeaseResourceClass::WholeMachine,
            observed_at_ms: 42,
            active: Some(HostLeaseHandle {
                schema_version: 1,
                host: "mac-local".to_string(),
                resource_class: HostLeaseResourceClass::WholeMachine,
                execution_context: None,
                lease_id: "lease-1".to_string(),
                owner: "owner".to_string(),
                priority_class: HostLeasePriorityClass::Measurement,
                acquired_at_ms: 10,
                updated_at_ms: 20,
                expires_at_ms: Some(30),
                owner_pid: Some(123),
                owner_process_identity: Some(456),
                reason: Some("measurement".to_string()),
                metadata: BTreeMap::from([("lane".to_string(), "meter".to_string())]),
            }),
            recovered_stale_lease: true,
        };

        let value = state_to_value(&state).expect("state converts");
        let VmValue::Dict(state) = value else {
            panic!("expected state dict");
        };
        assert!(matches!(
            state.get("recovered_stale_lease"),
            Some(VmValue::Bool(true))
        ));
        let Some(VmValue::Dict(active)) = state.get("active") else {
            panic!("expected active lease");
        };
        assert_eq!(
            active.get("priority_class").map(VmValue::display),
            Some("measurement".to_string())
        );
        assert_eq!(
            active.get("owner_process_identity").map(VmValue::display),
            Some("456".to_string())
        );
    }
}
