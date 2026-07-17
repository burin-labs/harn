//! Host introspection for the `harness.system.*` capability surface.
//!
//! The methods here back the read-only `cpu()`, `memory()`, `gpus()`,
//! `temperature()`, `platform()`, and `processes()` accessors on the
//! `HarnessSystem` sub-handle (issue #1912 / epic #1765). All values are
//! returned as `serde_json::Value` shapes that `crate::stdlib::json_to_vm_value`
//! lifts into dicts/lists for the VM.
//!
//! Privacy + cross-platform notes:
//!
//! * `processes()` includes the current Harn process unconditionally; its
//!   direct children are tagged with `is_harn_owned: true` when they appear
//!   in the system snapshot. We deliberately do **not** leak
//!   `command_line` / `environ` / `cwd` for arbitrary host processes — only
//!   pid, name, cpu%, memory bytes, and the harn-ownership flag are
//!   returned. Hosts that need richer per-process introspection should
//!   reach for their own privileged surface.
//! * `temperature()` and `gpus()` may return empty / partial data on
//!   platforms whose `sysinfo` backend doesn't expose those sensors
//!   (notably Apple Silicon and most containers). Callers must treat the
//!   fields as best-effort — missing data is conveyed via empty lists or
//!   `null` field values rather than errors so scripts can degrade
//!   gracefully (`"if a local GPU is available, prefer local model"`).
//! * Tagging spawned subprocesses with the active pipeline / session id
//!   is descoped to a follow-up: it requires plumbing through the
//!   sandbox spawn path. The current implementation tags only direct
//!   children of the harn process (parent pid match), which is enough to
//!   power the emergency-signaling use case in the issue body.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde_json::{json, Value};
use sysinfo::{
    Components, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};

/// Registry of harn-owned child pids. Subprocess spawners (e.g. the
/// `command_output` path in `stdlib::sandbox`) may register their
/// children here so `processes()` can tag them with
/// `is_harn_owned: true` even after the parent->child link is broken
/// (e.g. detached agents).
static HARN_OWNED_PIDS: Mutex<BTreeMap<u32, usize>> = Mutex::new(BTreeMap::new());

/// Owns one claim that a child pid belongs to Harn.
///
/// Claims are reference-counted because independent runtime components may
/// observe the same child. The pid stays tagged until the final guard drops.
#[must_use = "dropping the registration stops claiming the pid as Harn-owned"]
pub struct HarnOwnedPidRegistration {
    pid: u32,
}

impl Drop for HarnOwnedPidRegistration {
    fn drop(&mut self) {
        unregister_harn_owned_pid(self.pid);
    }
}

/// Register a pid as Harn-owned for the lifetime of the returned guard.
pub fn register_harn_owned_pid(pid: u32) -> HarnOwnedPidRegistration {
    let mut set = HARN_OWNED_PIDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *set.entry(pid).or_default() += 1;
    HarnOwnedPidRegistration { pid }
}

fn unregister_harn_owned_pid(pid: u32) {
    let mut set = HARN_OWNED_PIDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match set.get_mut(&pid) {
        Some(claims) if *claims > 1 => *claims -= 1,
        Some(_) => {
            set.remove(&pid);
        }
        None => {}
    }
}

fn harn_owned_pids_snapshot() -> BTreeSet<u32> {
    HARN_OWNED_PIDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .copied()
        .collect()
}

/// Snapshot of CPU topology. `count` reflects logical cores; `frequency_mhz`
/// is the first-core frequency reported by the OS (typically the current
/// frequency; nominal on many platforms).
pub fn cpu_snapshot() -> Value {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(
            sysinfo::CpuRefreshKind::nothing()
                .with_cpu_usage()
                .with_frequency(),
        ),
    );
    sys.refresh_cpu_all();
    let cpus = sys.cpus();
    let count = cpus.len();
    let physical_count = System::physical_core_count();
    let (model, frequency_mhz) = match cpus.first() {
        Some(cpu) => {
            let brand = cpu.brand().trim().to_string();
            (
                if brand.is_empty() { None } else { Some(brand) },
                Some(cpu.frequency()),
            )
        }
        None => (None, None),
    };
    let cpu_usage = if cpus.is_empty() {
        None
    } else {
        let total: f32 = cpus.iter().map(|c| c.cpu_usage()).sum();
        Some(total as f64 / cpus.len() as f64)
    };
    json!({
        "count": count,
        "physical_count": physical_count,
        "model": model,
        "frequency_mhz": frequency_mhz,
        "usage_pct": cpu_usage,
    })
}

/// Snapshot of host memory. All sizes are bytes; cross-platform with
/// graceful zeroes on hosts where a metric is unavailable.
pub fn memory_snapshot() -> Value {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    );
    sys.refresh_memory();
    let total = sys.total_memory();
    let used = sys.used_memory();
    let available = sys.available_memory();
    let total_gb = bytes_to_gb(total);
    let used_gb = bytes_to_gb(used);
    let available_gb = bytes_to_gb(available);
    let pressure = if total == 0 {
        "unknown"
    } else {
        let ratio = used as f64 / total as f64;
        if ratio >= 0.85 {
            "high"
        } else if ratio >= 0.6 {
            "medium"
        } else {
            "low"
        }
    };
    json!({
        "total_bytes": total,
        "used_bytes": used,
        "available_bytes": available,
        "total_gb": total_gb,
        "used_gb": used_gb,
        "available_gb": available_gb,
        "pressure": pressure,
    })
}

/// Resident bytes for the current Harn process.
pub fn current_process_memory_bytes() -> Option<u64> {
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map(|process| process.memory())
}

/// Snapshot of attached GPUs. `sysinfo` does not expose GPU details
/// directly across all platforms; we surface a non-fatal empty list so
/// scripts can write `if !gpus.is_empty()` portably. Richer detection
/// (NVML, Metal, OpenCL) is a follow-up tracked in the issue body.
pub fn gpus_snapshot() -> Value {
    Value::Array(Vec::new())
}

/// Snapshot of per-component temperatures (celsius). Returns `null`
/// fields when the host does not expose a sensor, and an empty list
/// when no thermal sensors are visible at all — common in containers,
/// VMs, and on macOS where `sysinfo`'s thermal API has long-standing
/// gaps.
pub fn temperature_snapshot() -> Value {
    let components = Components::new_with_refreshed_list();
    let mut entries = Vec::new();
    for component in &components {
        entries.push(json!({
            "label": component.label(),
            "celsius": component.temperature(),
            "max_celsius": component.max(),
            "critical_celsius": component.critical(),
        }));
    }
    json!({
        "components": entries,
    })
}

/// Snapshot of the host platform: os, arch, version, kernel.
pub fn platform_snapshot() -> Value {
    json!({
        "os": System::name(),
        "arch": std::env::consts::ARCH,
        "version": System::os_version(),
        "kernel": System::kernel_version(),
        "long_os_version": System::long_os_version(),
        "hostname": System::host_name(),
    })
}

/// Snapshot of currently visible processes. The current Harn process is
/// always included. Other processes are listed but with limited
/// metadata — name, pid, cpu%, memory bytes, and an `is_harn_owned`
/// flag derived from the parent pid match or the explicit
/// [`register_harn_owned_pid`] registry. We do not return command line
/// arguments, environment, or working directory: those can leak
/// credentials and prompts from peer agents.
pub fn processes_snapshot() -> Value {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    let our_pid = std::process::id();
    let our_pid_sys = Pid::from_u32(our_pid);
    let registry = harn_owned_pids_snapshot();

    let mut entries = Vec::new();
    for (pid, process) in sys.processes() {
        let pid_u32 = pid.as_u32();
        let parent_u32 = process.parent().map(|p| p.as_u32());
        let is_harn_owned =
            pid_u32 == our_pid || registry.contains(&pid_u32) || parent_u32 == Some(our_pid);
        if !is_harn_owned {
            // Limit per-process detail leakage: peer processes appear
            // in the list as bare {pid, name} entries. Scripts that
            // need the broader topology can opt into it via a future
            // capability extension.
            entries.push(json!({
                "pid": pid_u32,
                "name": process.name().to_string_lossy(),
                "is_harn_owned": false,
            }));
            continue;
        }
        entries.push(json!({
            "pid": pid_u32,
            "parent_pid": parent_u32,
            "name": process.name().to_string_lossy(),
            "cpu_pct": process.cpu_usage(),
            "mem_bytes": process.memory(),
            "is_harn_owned": true,
            "is_self": pid_u32 == our_pid,
        }));
    }

    // Stable ordering: harn-owned first, then by pid ascending.
    entries.sort_by(|a, b| {
        let a_owned = a
            .get("is_harn_owned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let b_owned = b
            .get("is_harn_owned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        b_owned.cmp(&a_owned).then_with(|| {
            a.get("pid")
                .and_then(Value::as_u64)
                .cmp(&b.get("pid").and_then(Value::as_u64))
        })
    });

    // sysinfo doesn't always include our own pid before the first
    // refresh on some platforms (Windows); synthesize an entry so the
    // contract "processes() always contains the running harn process"
    // holds even on cold snapshots.
    if !entries
        .iter()
        .any(|entry| entry.get("pid").and_then(Value::as_u64).map(|p| p as u32) == Some(our_pid))
    {
        entries.insert(
            0,
            json!({
                "pid": our_pid,
                "parent_pid": Value::Null,
                "name": current_process_name(&sys, our_pid_sys),
                "cpu_pct": 0.0,
                "mem_bytes": 0,
                "is_harn_owned": true,
                "is_self": true,
            }),
        );
    }

    Value::Array(entries)
}

fn current_process_name(sys: &System, pid: Pid) -> String {
    sys.process(pid)
        .map(|process| process.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| "harn".to_string())
}

fn bytes_to_gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_snapshot_reports_nonzero_count() {
        let snapshot = cpu_snapshot();
        let count = snapshot
            .get("count")
            .and_then(Value::as_u64)
            .expect("count present");
        assert!(count >= 1, "expected at least one logical cpu, got {count}");
    }

    #[test]
    fn memory_snapshot_has_nonzero_total() {
        let snapshot = memory_snapshot();
        let total = snapshot
            .get("total_bytes")
            .and_then(Value::as_u64)
            .expect("total_bytes present");
        assert!(total > 0, "total memory should be non-zero, got {total}");
        let pressure = snapshot
            .get("pressure")
            .and_then(Value::as_str)
            .expect("pressure present");
        assert!(
            matches!(pressure, "low" | "medium" | "high" | "unknown"),
            "pressure should be a known bucket, got {pressure:?}"
        );
    }

    #[test]
    fn gpus_snapshot_returns_list() {
        let snapshot = gpus_snapshot();
        assert!(snapshot.is_array(), "gpus snapshot is a list");
    }

    #[test]
    fn temperature_snapshot_returns_components_field() {
        let snapshot = temperature_snapshot();
        assert!(
            snapshot.get("components").is_some(),
            "components field present"
        );
        assert!(
            snapshot.get("components").unwrap().is_array(),
            "components is array"
        );
    }

    #[test]
    fn platform_snapshot_includes_arch() {
        let snapshot = platform_snapshot();
        assert_eq!(
            snapshot.get("arch").and_then(Value::as_str),
            Some(std::env::consts::ARCH)
        );
    }

    #[test]
    fn processes_snapshot_includes_self() {
        let snapshot = processes_snapshot();
        let entries = snapshot.as_array().expect("array");
        let our_pid = std::process::id() as u64;
        let self_entry = entries
            .iter()
            .find(|entry| entry.get("pid").and_then(Value::as_u64) == Some(our_pid))
            .expect("self entry present");
        assert_eq!(
            self_entry.get("is_harn_owned").and_then(Value::as_bool),
            Some(true),
            "self entry must be harn-owned"
        );
    }

    #[test]
    fn current_process_memory_bytes_reports_self_when_available() {
        if let Some(bytes) = current_process_memory_bytes() {
            assert!(bytes > 0, "current process memory should be non-zero");
        }
    }

    #[test]
    fn harn_owned_pid_process_global_lifetime_waits_for_every_owner() {
        // pick a pid that's vanishingly unlikely to collide with self
        let fake = u32::MAX - 1;
        let first = register_harn_owned_pid(fake);
        let second = register_harn_owned_pid(fake);
        assert!(harn_owned_pids_snapshot().contains(&fake));
        drop(first);
        assert!(
            harn_owned_pids_snapshot().contains(&fake),
            "one owner dropping must not erase another owner's live claim"
        );
        drop(second);
        assert!(!harn_owned_pids_snapshot().contains(&fake));
    }
}
