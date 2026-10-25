//! Make Darwin's temporary-directory API use the existing workspace temp root.
//!
//! Foundation consults confstr before TMPDIR. A native user-directory suffix
//! names an alias to .harn-tmp; Seatbelt still authorizes the resolved workspace
//! path. No shared temporary directory or alias subtree receives a grant.

use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Once;

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
    static REAPED: Once = Once::new();
    REAPED.call_once(|| reap_dangling_aliases(&base));
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

/// Serializes alias preparation against reaping across processes. A preparing
/// spawn holds it shared from reading the alias until it returns, and a reaper
/// holds it exclusive while it re-checks and unlinks one alias, so an alias that
/// a spawn just found live is never unlinked under it.
const ALIAS_LOCK: &str = "harn-aliases.lock";

/// Closing the file releases the lock, so the field is held, never read.
struct AliasLock {
    _held: std::fs::File,
}

impl AliasLock {
    fn acquire(base: &Path, exclusive: bool) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(base.join(ALIAS_LOCK))?;
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        loop {
            // SAFETY: the descriptor is open for the life of `file`.
            if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
                return Ok(Self { _held: file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn alias_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.strip_prefix("harn-").is_some_and(|digest| {
            digest.len() == ALIAS_DIGEST_HEX
                && digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    })
}

fn is_dangling_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
        && matches!(path.metadata(), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

/// Remove aliases whose workspace is gone. Only a symlink named like an alias
/// whose target no longer exists is touched, never a directory, a file, or a
/// live target. The base can be large and shared, so the scan holds no lock;
/// each candidate is re-checked and unlinked under the exclusive lock. Best
/// effort: a failure leaves the entry for the next process.
fn reap_dangling_aliases(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if alias_name(&entry.file_name()) && is_dangling_symlink(&entry.path()) {
            reap_if_dangling(base, &entry.file_name());
        }
    }
}

fn reap_if_dangling(base: &Path, name: &std::ffi::OsStr) {
    let Ok(_lock) = AliasLock::acquire(base, true) else {
        return;
    };
    let path = base.join(name);
    if is_dangling_symlink(&path) {
        let _ = std::fs::remove_file(&path);
    }
}

fn prepare_in(base: &Path, target: &Path) -> io::Result<String> {
    let _lock = AliasLock::acquire(base, false)?;
    let mut digest = harn_kernel::pure::sha256_hex(target.as_os_str().as_bytes());
    digest.truncate(ALIAS_DIGEST_HEX);
    let suffix = format!("harn-{digest}");
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
