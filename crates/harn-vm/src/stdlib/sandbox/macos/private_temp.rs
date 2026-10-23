//! Make Darwin's temporary-directory API use the existing workspace temp root.
//!
//! Foundation consults confstr before TMPDIR. A native user-directory suffix
//! names an alias to .harn-tmp; Seatbelt still authorizes the resolved workspace
//! path. No shared temporary directory or alias subtree receives a grant.

use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

const SUFFIX_ENV: &str = "DIRHELPER_USER_DIR_SUFFIX";

pub(super) struct Binding {
    pub directory: PathBuf,
    pub suffix: String,
}

pub(super) fn prepare(policy: &CapabilityPolicy) -> io::Result<Option<Binding>> {
    let Some(temp) = super::super::workspace_local_tmpdir(policy) else {
        return Ok(None);
    };
    let target = std::fs::canonicalize(temp)?;
    if !super::super::normalized_workspace_roots(policy)
        .iter()
        .any(|root| target.starts_with(root))
    {
        return Err(io::Error::other(
            "workspace temporary directory escapes its workspace",
        ));
    }
    let base = native_temp_base()?;
    let suffix = prepare_in(&base, &target)?;
    Ok(Some(Binding {
        directory: target,
        suffix,
    }))
}

pub(in crate::stdlib::sandbox) fn inject_env(
    env: &mut Vec<(String, String)>,
    policy: &CapabilityPolicy,
) {
    if !policy.sandbox_profile.confines_processes()
        || super::super::effective_fallback(policy.sandbox_profile)
            == super::super::SandboxFallback::Off
    {
        return;
    }
    match prepare(policy) {
        Ok(Some(binding)) if !env.iter().any(|(key, _)| key == SUFFIX_ENV) => {
            env.push((SUFFIX_ENV.to_string(), binding.suffix));
        }
        Ok(_) => {}
        Err(error) => super::super::warn_once(
            "handler_sandbox_native_temp",
            &format!("cannot map Darwin temporary storage into the workspace: {error}; shared temporary storage remains denied"),
        ),
    }
}

/// Hex digits of the workspace digest in the alias name. A Unix socket address
/// holds 104 bytes on Darwin, and the per-user temp base already takes about
/// 50: the full digest left no room for a socket such as the JVM's
/// `.java_pid<pid>`. A collision is refused below, never shared.
const ALIAS_DIGEST_HEX: usize = 16;

fn prepare_in(base: &Path, target: &Path) -> io::Result<String> {
    let digest = harn_kernel::pure::sha256_hex(target.as_os_str().as_bytes());
    let suffix = format!("harn-{}", &digest[..ALIAS_DIGEST_HEX]);
    let alias = base.join(&suffix);
    // An existing alias is read, never replaced. Atomic exclusive creation
    // handles two spawns preparing the same workspace concurrently; a planted
    // file or a different symlink target is a refusal, never an overwrite.
    match std::fs::read_link(&alias) {
        Ok(existing) if existing == target => return Ok(suffix),
        Ok(_) => {
            return Err(io::Error::other(
                "Darwin temporary alias has a different target",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match std::os::unix::fs::symlink(target, &alias) {
        Ok(()) => Ok(suffix),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::read_link(&alias)? == target {
                Ok(suffix)
            } else {
                Err(io::Error::other(
                    "Darwin temporary alias has a different target",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

fn native_temp_base() -> io::Result<PathBuf> {
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: the buffer is writable for the length supplied to confstr.
    let written = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if written == 0 || written > buffer.len() {
        return Err(io::Error::other(
            "cannot resolve Darwin temporary directory",
        ));
    }
    buffer.truncate(written - 1);
    let path = PathBuf::from(OsString::from_vec(buffer));
    unsafe extern "C" {
        fn _get_user_dir_suffix() -> *const libc::c_char;
    }
    // SAFETY: CoreServices owns the nullable, NUL-terminated suffix string.
    // Harn never changes that process-global setting.
    let suffix = unsafe { _get_user_dir_suffix() };
    let suffix = if suffix.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(suffix) }.to_bytes())
    };
    let base = unsuffixed_base(&path, suffix.map(OsStr::from_bytes))?;
    std::fs::canonicalize(base)
}

fn unsuffixed_base(path: &Path, suffix: Option<&OsStr>) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::other(
            "Darwin temporary directory is not absolute",
        ));
    }
    match suffix.filter(|suffix| !suffix.is_empty()) {
        None => Ok(path.to_path_buf()),
        Some(suffix) if path.file_name() == Some(suffix) => Ok(path
            .parent()
            .expect("absolute path with a file name")
            .to_path_buf()),
        Some(_) => Err(io::Error::other(
            "Darwin temporary directory and native suffix disagree",
        )),
    }
}

#[cfg(test)]
#[path = "private_temp_tests.rs"]
mod tests;
