//! OpenBSD sandbox backend — pledge/unveil applied via `pre_exec`.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

use super::{
    policy_allows_network, policy_allows_workspace_write, process_sandbox_readonly_roots,
    process_sandbox_roots, PrepareOutcome, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "openbsd"
    }

    fn available() -> bool {
        true
    }

    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(policy)?;
        // SAFETY: `unveil` and `pledge` are async-signal-safe by
        // design — they restrict the calling process and perform no
        // allocation, locking, or stdio.
        unsafe {
            command.pre_exec(move || apply_profile(&prep));
        }
        Ok(PrepareOutcome::Direct)
    }

    fn prepare_tokio_command(
        _program: &str,
        _args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(policy)?;
        // SAFETY: see OpenBSD `prepare_std_command` above.
        unsafe {
            command.pre_exec(move || apply_profile(&prep));
        }
        Ok(PrepareOutcome::Direct)
    }
}

struct ProcessProfile {
    unveil_rules: Vec<(String, String)>,
    promises: String,
}

fn profile_setup(policy: &CapabilityPolicy) -> Result<ProcessProfile, VmError> {
    let workspace_permissions = if policy_allows_workspace_write(policy) {
        "rwcx"
    } else {
        "rx"
    };
    let mut unveil_rules = vec![
        ("/bin".to_string(), "rx".to_string()),
        ("/usr".to_string(), "rx".to_string()),
        ("/lib".to_string(), "rx".to_string()),
        ("/etc".to_string(), "r".to_string()),
        ("/dev".to_string(), "rw".to_string()),
    ];
    for root in process_sandbox_roots(policy) {
        unveil_rules.push((
            root.display().to_string(),
            workspace_permissions.to_string(),
        ));
    }
    // Read-only roots get read+execute but never write/create, even when
    // the policy otherwise allows workspace writes.
    for root in process_sandbox_readonly_roots(policy) {
        unveil_rules.push((root.display().to_string(), "rx".to_string()));
    }

    let mut promises = vec!["stdio", "rpath", "proc", "exec"];
    if policy_allows_workspace_write(policy) {
        promises.extend(["wpath", "cpath", "dpath"]);
    }
    if policy_allows_network(policy) {
        promises.extend(["inet", "dns"]);
    }
    Ok(ProcessProfile {
        unveil_rules,
        promises: promises.join(" "),
    })
}

fn apply_profile(profile: &ProcessProfile) -> io::Result<()> {
    for (path, permissions) in &profile.unveil_rules {
        let path = CString::new(path.as_str())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "unveil path contains NUL"))?;
        let permissions = CString::new(permissions.as_str()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unveil permissions contain NUL",
            )
        })?;
        unsafe {
            if unveil(path.as_ptr(), permissions.as_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    unsafe {
        if unveil(std::ptr::null(), std::ptr::null()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let promises = CString::new(profile.promises.as_str())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pledge promises contain NUL"))?;
    unsafe {
        if pledge(promises.as_ptr(), std::ptr::null()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

extern "C" {
    fn pledge(promises: *const libc::c_char, execpromises: *const libc::c_char) -> libc::c_int;
    fn unveil(path: *const libc::c_char, permissions: *const libc::c_char) -> libc::c_int;
}
