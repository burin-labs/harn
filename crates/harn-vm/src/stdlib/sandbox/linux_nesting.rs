//! What a confined Linux child needs to confine its own children (harn#9454).
//!
//! The parent's seccomp ceiling admits the Landlock calls and one filtered
//! `seccomp` form, and the Landlock probe needs a witness directory the child
//! can still open. Both mechanisms only narrow, so a nested child holds the
//! intersection of every layer above it.

use std::path::PathBuf;

use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

use super::super::sandbox_rejection;
use crate::value::VmError;

/// `seccomp(SECCOMP_SET_MODE_FILTER, 0, ...)` and nothing else: both arguments
/// are `unsigned int`, so a 32-bit comparison is the whole value the kernel
/// reads.
pub(super) fn nested_seccomp_filter_rule() -> Result<SeccompRule, VmError> {
    let condition = |index, value| {
        SeccompCondition::new(index, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, value).map_err(
            |err| {
                sandbox_rejection(format!(
                    "failed to build the nested seccomp condition: {err}"
                ))
            },
        )
    };
    SeccompRule::new(vec![
        condition(0, u64::from(libc::SECCOMP_SET_MODE_FILTER))?,
        condition(1, 0)?,
    ])
    .map_err(|err| sandbox_rejection(format!("failed to build the nested seccomp rule: {err}")))
}

/// A directory this process can open now, which an empty ruleset must then
/// close to it. The root comes first; a process already inside a Landlock
/// domain usually cannot read the root, so its working directory and temp
/// directory follow. No readable candidate means no measurement, which reads
/// as unavailable rather than as enforcement.
pub(super) fn landlock_probe_witness() -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    let candidates = [
        Some(PathBuf::from("/")),
        std::env::current_dir().ok(),
        Some(std::env::temp_dir()),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|path| path.is_absolute())
        .find(|path| std::fs::read_dir(path).is_ok())
        .and_then(|path| std::ffi::CString::new(path.as_os_str().as_bytes()).ok())
}
