//! Confining this process itself with the Seatbelt profile a child would get.
//!
//! `sandbox-exec` is a thin wrapper over `sandbox_init`, which applies a
//! profile to the calling process. Calling it directly applies exactly the
//! profile [`super::render_profile_for_program`] renders for a confined child,
//! to this process and everything it later spawns.

use std::ffi::{c_char, CStr, CString};
use std::path::Path;

use super::nested;
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::sandbox_rejection;
use crate::value::VmError;

/// A path no workspace profile grants a write to, so a denial there has one
/// meaning: the profile is in force. A workspace rooted at `/` would grant it,
/// and is then reported as not confined, which it effectively is not.
const BOUNDARY_PROBE: &str = "/.harn-confinement-probe";

pub(super) fn confine(policy: &CapabilityPolicy) -> Result<(), VmError> {
    // A process already inside a sandbox (App Sandbox, or `sandbox-exec`)
    // cannot take a second profile: macOS refuses to stack them.
    if nested::process_is_sandboxed() {
        return Err(sandbox_rejection(
            "this process is already inside a macOS sandbox, and macOS cannot apply a second \
             profile over it"
                .to_string(),
        ));
    }
    let program = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let profile = CString::new(super::render_profile_for_program(policy, &program))
        .map_err(|_| sandbox_rejection("the rendered profile contains a NUL byte".to_string()))?;
    let mut error: *mut c_char = std::ptr::null_mut();
    // Flags 0: `profile` is profile source, not the name of a built-in one.
    let status = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    if status != 0 {
        let reason = if error.is_null() {
            "no reason given".to_string()
        } else {
            let reason = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error) };
            reason
        };
        return Err(sandbox_rejection(format!(
            "macOS refused to confine this process: {reason}"
        )));
    }
    // `sandbox_init` returning zero says the kernel accepted the profile, not
    // that it is enforcing it. Ask the kernel about a write no profile grants.
    if !nested::process_is_sandboxed()
        || !nested::outer_denies("file-write-create", Some(Path::new(BOUNDARY_PROBE)))
    {
        return Err(sandbox_rejection(format!(
            "the profile was applied but the boundary is not holding: a write to \
             {BOUNDARY_PROBE} is still allowed, so this process is not confined"
        )));
    }
    Ok(())
}

unsafe extern "C" {
    // 0 on success; on failure, -1 with `errorbuf` pointing at a message that
    // `sandbox_free_error` releases.
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> libc::c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}
