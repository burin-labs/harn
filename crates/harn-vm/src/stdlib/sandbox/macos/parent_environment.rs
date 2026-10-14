//! Protect the spawning host's launch environment before executing confined code.
//!
//! XNU's numeric KERN_PROCARGS and KERN_PROCARGS2 path does not enforce Seatbelt's
//! sysctl-read or process-info rules. It omits the environment for ordinary readers when
//! that process has CS_RESTRICT. Set the sticky flag on this host, never a child
//! or an unrelated process, and read it back before permitting the spawn.
//! This protects this host's environment, not every same-user process on macOS.

use crate::stdlib::sandbox::sandbox_rejection;
use crate::value::VmError;

const CS_OPS_STATUS: libc::c_uint = 0;
const CS_OPS_MARKRESTRICT: libc::c_uint = 8;
const CS_RESTRICT: u32 = 0x0000_0800;
const CSR_ALLOW_UNRESTRICTED_DTRACE: u32 = 0x0020;

pub(super) fn protect() -> Result<(), VmError> {
    // XNU explicitly bypasses environment redaction on development kernels
    // and when unrestricted DTrace is enabled. Do not report protection there.
    let mut development: libc::c_int = -1;
    let mut size = std::mem::size_of_val(&development);
    let mut integrity_config: u32 = 0;
    if unsafe {
        libc::sysctlbyname(
            c"kern.development".as_ptr(),
            (&raw mut development).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size != std::mem::size_of_val(&development)
        || development != 0
        || unsafe { csr_get_active_config(&raw mut integrity_config) } != 0
        || integrity_config & CSR_ALLOW_UNRESTRICTED_DTRACE != 0
    {
        return Err(sandbox_rejection(
            "macOS cannot protect the host environment on this kernel or System Integrity \
             Protection configuration; the confined child was not started"
                .to_string(),
        ));
    }
    // A PID of zero names the calling process. MARKRESTRICT needs no signature
    // rewrite or elevated privileges and cannot remove an existing restriction.
    if unsafe { csops(0, CS_OPS_MARKRESTRICT, std::ptr::null_mut(), 0) } != 0 {
        return Err(sandbox_rejection(format!(
            "macOS could not protect the host environment before starting a confined child: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut flags: u32 = 0;
    if unsafe {
        csops(
            0,
            CS_OPS_STATUS,
            (&raw mut flags).cast(),
            std::mem::size_of::<u32>(),
        )
    } != 0
        || flags & CS_RESTRICT == 0
    {
        return Err(sandbox_rejection(
            "macOS did not confirm the host environment restriction; the child was not started"
                .to_string(),
        ));
    }
    Ok(())
}

unsafe extern "C" {
    fn csr_get_active_config(config: *mut u32) -> libc::c_int;
    fn csops(
        pid: libc::pid_t,
        operation: libc::c_uint,
        address: *mut libc::c_void,
        size: libc::size_t,
    ) -> libc::c_int;
}
