//! Harn VM bridge for cancellation-safe machine-global host lease scopes.
//!
//! The lease registry and stale-recovery policy remain in [`crate::host_lease`].
//! This module owns only the Harn host-capability boundary: request validation,
//! explicit wire shaping, and backend error translation. Keeping the bridge
//! separate prevents product or orchestration callers from learning the SQLite
//! layout or parsing the `harn host lease` CLI envelope.

use std::collections::BTreeMap;
use std::time::Duration;

use harn_vm::{VmResourceGuardHandle, VmValue};

use crate::error::HostlibError;
use crate::host_lease::{
    HostLeaseAcquireReceipt, HostLeaseAcquireStatus, HostLeaseDeferReceipt, HostLeaseHandle,
    HostLeasePriorityClass, HostLeaseReleaseReceipt, HostLeaseRequest, HostLeaseResourceClass,
    HostLeaseState, HostLeaseStore, DEFAULT_HOST_LEASE_DOMAIN,
};
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

const STATUS_BUILTIN: &str = "hostlib_host_lease_status";
const ACQUIRE_BUILTIN: &str = "hostlib_host_lease_acquire";
const RELEASE_BUILTIN: &str = "hostlib_host_lease_release";
const MAX_WAIT_SLICE_MS: i64 = 5_000;

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
        registry.register_fn("host_lease", ACQUIRE_BUILTIN, "acquire", handle_acquire);
        registry.register_fn("host_lease", RELEASE_BUILTIN, "release", handle_release);
    }
}

fn handle_status(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(STATUS_BUILTIN, args)?;
    let host = require_nonempty_string(STATUS_BUILTIN, &dict, "host")?;
    let resource_class = resource_class(STATUS_BUILTIN, dict.get("resource_class"))?;
    let domain = optional_string(STATUS_BUILTIN, &dict, "domain")?
        .unwrap_or_else(|| DEFAULT_HOST_LEASE_DOMAIN.to_string());
    let state = HostLeaseStore::from_env()
        .and_then(|store| store.status_for_domain(&host, resource_class, &domain))
        .map_err(|error| HostlibError::Backend {
            builtin: STATUS_BUILTIN,
            message: error.to_string(),
        })?;
    state_to_value(&state)
}

fn handle_acquire(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(ACQUIRE_BUILTIN, args)?;
    let host = match optional_string(ACQUIRE_BUILTIN, &dict, "host")? {
        Some(host) if !host.trim().is_empty() => host,
        Some(_) => {
            return Err(HostlibError::InvalidParameter {
                builtin: ACQUIRE_BUILTIN,
                param: "host",
                message: "must be a non-empty string when provided".to_string(),
            });
        }
        None => HostLeaseStore::default_host(),
    };
    let owner = require_nonempty_string(ACQUIRE_BUILTIN, &dict, "owner")?;
    let resource_class = resource_class(ACQUIRE_BUILTIN, dict.get("resource_class"))?;
    let domain = optional_string(ACQUIRE_BUILTIN, &dict, "domain")?
        .unwrap_or_else(|| DEFAULT_HOST_LEASE_DOMAIN.to_string());
    let priority_class = priority_class(ACQUIRE_BUILTIN, dict.get("priority_class"))?;
    let ttl_ms = optional_positive_u64(ACQUIRE_BUILTIN, &dict, "ttl_ms")?;
    let wait_slice_ms = optional_int(ACQUIRE_BUILTIN, &dict, "wait_slice_ms", 0)?;
    if !(0..=MAX_WAIT_SLICE_MS).contains(&wait_slice_ms) {
        return Err(HostlibError::InvalidParameter {
            builtin: ACQUIRE_BUILTIN,
            param: "wait_slice_ms",
            message: format!("must be between 0 and {MAX_WAIT_SLICE_MS}"),
        });
    }
    let reason = optional_string(ACQUIRE_BUILTIN, &dict, "reason")?;
    let metadata = string_map(ACQUIRE_BUILTIN, dict.get("metadata"))?;
    let request = HostLeaseRequest {
        host,
        resource_class,
        domain,
        execution_context: None,
        owner,
        priority_class,
        ttl_ms,
        owner_pid: Some(std::process::id()),
        reason,
        metadata,
    };
    let store = HostLeaseStore::from_env().map_err(|error| backend(ACQUIRE_BUILTIN, error))?;
    let receipt = if wait_slice_ms == 0 {
        store.try_acquire(request)
    } else {
        store.acquire_wait(request, Duration::from_millis(wait_slice_ms as u64))
    }
    .map_err(|error| backend(ACQUIRE_BUILTIN, error))?;
    acquire_to_value(store, receipt)
}

fn handle_release(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(RELEASE_BUILTIN, args)?;
    let Some(VmValue::ResourceGuard(guard)) = dict.get("guard") else {
        return Err(HostlibError::InvalidParameter {
            builtin: RELEASE_BUILTIN,
            param: "guard",
            message: "must be a resource_guard returned by host_lease.acquire".to_string(),
        });
    };
    guard
        .release()
        .map_err(|error| backend(RELEASE_BUILTIN, error))
}

fn backend(builtin: &'static str, error: impl std::fmt::Display) -> HostlibError {
    HostlibError::Backend {
        builtin,
        message: error.to_string(),
    }
}

fn resource_class(
    builtin: &'static str,
    value: Option<&VmValue>,
) -> Result<HostLeaseResourceClass, HostlibError> {
    match value {
        None | Some(VmValue::Nil) => Ok(HostLeaseResourceClass::WholeMachine),
        Some(VmValue::String(value)) if value.as_str() == "whole-machine" => {
            Ok(HostLeaseResourceClass::WholeMachine)
        }
        Some(VmValue::String(value)) if value.as_str() == "rust-heavy" => {
            Ok(HostLeaseResourceClass::RustHeavy)
        }
        _ => Err(HostlibError::InvalidParameter {
            builtin,
            param: "resource_class",
            message: "must be whole-machine or rust-heavy".to_string(),
        }),
    }
}

fn priority_class(
    builtin: &'static str,
    value: Option<&VmValue>,
) -> Result<HostLeasePriorityClass, HostlibError> {
    match value {
        None | Some(VmValue::Nil) => Ok(HostLeasePriorityClass::Deferrable),
        Some(VmValue::String(value)) => match value.as_str() {
            "interactive" => Ok(HostLeasePriorityClass::Interactive),
            "measurement" => Ok(HostLeasePriorityClass::Measurement),
            "ci-verify" => Ok(HostLeasePriorityClass::CiVerify),
            "deferrable" => Ok(HostLeasePriorityClass::Deferrable),
            _ => Err(HostlibError::InvalidParameter {
                builtin,
                param: "priority_class",
                message: "must be interactive, measurement, ci-verify, or deferrable".to_string(),
            }),
        },
        _ => Err(HostlibError::InvalidParameter {
            builtin,
            param: "priority_class",
            message: "must be a string".to_string(),
        }),
    }
}

fn optional_positive_u64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) if *value > 0 => Ok(Some(*value as u64)),
        _ => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must be a positive integer or nil".to_string(),
        }),
    }
}

fn string_map(
    builtin: &'static str,
    value: Option<&VmValue>,
) -> Result<BTreeMap<String, String>, HostlibError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    if matches!(value, VmValue::Nil) {
        return Ok(BTreeMap::new());
    }
    let VmValue::Dict(entries) = value else {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "metadata",
            message: "must be a dict of string values".to_string(),
        });
    };
    entries
        .iter()
        .map(|(key, value)| match value {
            VmValue::String(value) => Ok((key.to_string(), value.to_string())),
            _ => Err(HostlibError::InvalidParameter {
                builtin,
                param: "metadata",
                message: "must contain only string values".to_string(),
            }),
        })
        .collect()
}

fn acquire_to_value(
    store: HostLeaseStore,
    receipt: HostLeaseAcquireReceipt,
) -> Result<VmValue, HostlibError> {
    let handle = match receipt.handle.as_ref() {
        Some(handle) => handle_to_value(ACQUIRE_BUILTIN, handle)?,
        None => VmValue::Nil,
    };
    let defer = receipt
        .defer
        .as_ref()
        .map(defer_to_value)
        .transpose()?
        .unwrap_or(VmValue::Nil);
    let guard = if receipt.status == HostLeaseAcquireStatus::Acquired {
        let handle = receipt
            .handle
            .as_ref()
            .ok_or_else(|| HostlibError::Backend {
                builtin: ACQUIRE_BUILTIN,
                message: "acquired receipt omitted its lease handle".to_string(),
            })?;
        let host = handle.host.clone();
        let resource_class = handle.resource_class;
        let domain = handle.domain.clone();
        let lease_id = handle.lease_id.clone();
        VmValue::resource_guard(VmResourceGuardHandle::new("host_lease", move || {
            store
                .release_for_domain(&host, resource_class, &domain, &lease_id)
                .map(|receipt| release_to_value(&receipt))
                .map_err(|error| error.to_string())
        }))
    } else {
        VmValue::Nil
    };
    Ok(build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(receipt.schema_version)),
        ),
        (
            "status",
            str_value(match receipt.status {
                HostLeaseAcquireStatus::Acquired => "acquired",
                HostLeaseAcquireStatus::Deferred => "deferred",
            }),
        ),
        ("observed_at_ms", VmValue::Int(receipt.observed_at_ms)),
        ("waited_ms", VmValue::Int(receipt.waited_ms as i64)),
        ("handle", handle),
        ("defer", defer),
        ("guard", guard),
        (
            "recovered_stale_lease",
            VmValue::Bool(receipt.recovered_stale_lease),
        ),
        (
            "recovered",
            receipt
                .recovered
                .as_ref()
                .map(|handle| handle_to_value(ACQUIRE_BUILTIN, handle))
                .transpose()?
                .unwrap_or(VmValue::Nil),
        ),
    ]))
}

fn defer_to_value(defer: &HostLeaseDeferReceipt) -> Result<VmValue, HostlibError> {
    Ok(build_dict([
        ("host", str_value(&defer.host)),
        ("resource_class", str_value(defer.resource_class.as_str())),
        ("domain", str_value(&defer.domain)),
        ("deferred_reason", str_value(defer.deferred_reason.as_str())),
        ("observed_at_ms", VmValue::Int(defer.observed_at_ms)),
        (
            "next_wake_at_ms",
            defer
                .next_wake_at_ms
                .map(VmValue::Int)
                .unwrap_or(VmValue::Nil),
        ),
        (
            "deadline_at_ms",
            defer
                .deadline_at_ms
                .map(VmValue::Int)
                .unwrap_or(VmValue::Nil),
        ),
        (
            "active",
            defer
                .active
                .as_ref()
                .map(|handle| handle_to_value(ACQUIRE_BUILTIN, handle))
                .transpose()?
                .unwrap_or(VmValue::Nil),
        ),
    ]))
}

fn release_to_value(receipt: &HostLeaseReleaseReceipt) -> VmValue {
    build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(receipt.schema_version)),
        ),
        ("released", VmValue::Bool(receipt.released)),
        ("host", str_value(&receipt.host)),
        ("resource_class", str_value(receipt.resource_class.as_str())),
        ("domain", str_value(&receipt.domain)),
        ("lease_id", str_value(&receipt.lease_id)),
        ("observed_at_ms", VmValue::Int(receipt.observed_at_ms)),
    ])
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
        Some(handle) => handle_to_value(STATUS_BUILTIN, handle)?,
        None => VmValue::Nil,
    };
    Ok(build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(state.schema_version)),
        ),
        ("host", str_value(&state.host)),
        ("resource_class", str_value(state.resource_class.as_str())),
        ("domain", str_value(&state.domain)),
        ("observed_at_ms", VmValue::Int(state.observed_at_ms)),
        ("active", active),
        (
            "recovered_stale_lease",
            VmValue::Bool(state.recovered_stale_lease),
        ),
        (
            "recovered",
            state
                .recovered
                .as_ref()
                .map(|handle| handle_to_value(STATUS_BUILTIN, handle))
                .transpose()?
                .unwrap_or(VmValue::Nil),
        ),
    ]))
}

fn handle_to_value(
    builtin: &'static str,
    handle: &HostLeaseHandle,
) -> Result<VmValue, HostlibError> {
    let owner_process_identity = match handle.owner_process_identity {
        Some(identity) => {
            VmValue::Int(i64::try_from(identity).map_err(|_| HostlibError::Backend {
                builtin,
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
        ("resource_class", str_value(handle.resource_class.as_str())),
        ("domain", str_value(&handle.domain)),
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
            domain: DEFAULT_HOST_LEASE_DOMAIN.to_string(),
            observed_at_ms: 42,
            active: None,
            recovered_stale_lease: true,
            recovered: Some(HostLeaseHandle {
                schema_version: 1,
                host: "mac-local".to_string(),
                resource_class: HostLeaseResourceClass::WholeMachine,
                domain: DEFAULT_HOST_LEASE_DOMAIN.to_string(),
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
        };

        let value = state_to_value(&state).expect("state converts");
        let VmValue::Dict(state) = value else {
            panic!("expected state dict");
        };
        assert!(matches!(
            state.get("recovered_stale_lease"),
            Some(VmValue::Bool(true))
        ));
        assert!(matches!(state.get("active"), Some(VmValue::Nil)));
        let Some(VmValue::Dict(recovered)) = state.get("recovered") else {
            panic!("expected recovered lease");
        };
        assert_eq!(
            recovered.get("priority_class").map(VmValue::display),
            Some("measurement".to_string())
        );
        assert_eq!(
            recovered
                .get("owner_process_identity")
                .map(VmValue::display),
            Some("456".to_string())
        );
    }
}
