use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

use super::{base_workspace_roots, normalize_for_policy, path_is_within, warn_once};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "pinned_pnpm.rs"]
mod pinned_pnpm;

#[path = "gh_config.rs"]
pub(crate) mod gh_config;
#[path = "grant_spawn.rs"]
pub(crate) mod grant_spawn;

#[cfg(test)]
#[path = "workspace_env_owner_tests.rs"]
mod owner_tests;

pub(crate) const TMPDIR_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// The self-ignored scratch dir a Harn script gets inside its workspace, from
/// `harness.fs.workspace_temp_dir()`. A confined child's `TMPDIR` is not here.
pub(crate) const WORKSPACE_TMPDIR_NAME: &str = ".harn-tmp";

/// The name the toolchain cache had when it lived inside the first workspace
/// root. Nothing creates it any more; a checkout that still holds one keeps it
/// out of walks and replay snapshots.
pub(crate) const LEGACY_WORKSPACE_TOOLCHAIN_CACHE_NAME: &str = ".harn-toolchain-cache";

/// The temp dir and toolchain cache, as children of the runtime dir.
const RUNTIME_TMPDIR_NAME: &str = "tmp";
const RUNTIME_TOOLCHAIN_CACHE_NAME: &str = "toolchain";

/// Hex digits of the workspace digest that names its runtime dir. A Unix
/// socket address holds 104 bytes on Darwin, and a socket under `TMPDIR`
/// (the JVM's `.java_pid<pid>`) needs room after the directory.
const RUNTIME_DIR_DIGEST_HEX: usize = 16;

/// Reserved workspace directory names owned by the runtime rather than the application.
pub fn is_workspace_runtime_directory(name: &OsStr) -> bool {
    name == OsStr::new(WORKSPACE_TMPDIR_NAME)
        || name == OsStr::new(LEGACY_WORKSPACE_TOOLCHAIN_CACHE_NAME)
}

/// The file in a runtime dir that names the canonical workspace root it
/// belongs to, so a later process can tell when the workspace is gone.
const RUNTIME_DIR_MARKER: &str = "workspace-root";

/// The longest workspace root a marker is read back as.
const RUNTIME_DIR_MARKER_MAX: u64 = 4096;

/// The per-workspace runtime directory a confined child's temp dir and
/// toolchain cache live in: `<user cache>/harn-sandbox/<digest>`, the digest
/// taken over the canonical workspace root. It sits outside every workspace,
/// so a linter or test collector walking the tree never meets a child's
/// temporary files, and is stable, so caches survive across runs. Computed
/// without touching disk; `None` without a resolvable user cache directory.
pub fn sandbox_runtime_dir(workspace_root: &Path) -> Option<PathBuf> {
    Some(runtime_base()?.join(runtime_digest(&normalize_for_policy(workspace_root))))
}

/// `<user cache>/harn-sandbox`, which holds every workspace's runtime dir.
fn runtime_base() -> Option<PathBuf> {
    // Resolved once: a spawn and the grant that admits its writes must name
    // the same directory, even if something in the process later changes
    // `HOME` or `XDG_CACHE_HOME`.
    static BASE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(crate::user_dirs::sandbox_runtime_base);
    Some(normalize_for_policy(base.as_ref()?))
}

/// [`runtime_base`] for the ignore policy, which skips it like other scratch.
pub(crate) fn sandbox_runtime_base_dir() -> Option<PathBuf> {
    runtime_base()
}

/// The runtime dir name for an already-normalized workspace root.
fn runtime_digest(root: &Path) -> String {
    let mut digest = harn_kernel::pure::sha256_hex(root.as_os_str().as_encoded_bytes());
    digest.truncate(RUNTIME_DIR_DIGEST_HEX);
    digest
}

/// Whether `name` has the shape [`runtime_digest`] gives a runtime dir.
fn is_runtime_digest_name(name: &str) -> bool {
    name.len() == RUNTIME_DIR_DIGEST_HEX
        && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// How many runtime dirs nested in one another [`runtime_dir_owners`] follows.
const MAX_RUNTIME_OWNER_DEPTH: usize = 8;

/// The workspaces whose runtime dirs hold the normalized `path`, innermost
/// first. A runtime dir stands in for the scratch dir that once sat inside its
/// workspace, so a root covering the workspace covers the runtime dir too; a
/// root made inside one workspace's temp dir has a runtime dir of its own, so
/// the chain is followed outward. Empty for a path outside every runtime dir.
pub(super) fn runtime_dir_owners(path: &Path) -> Vec<PathBuf> {
    runtime_base().map_or_else(Vec::new, |base| runtime_dir_owners_in(&base, path))
}

/// [`runtime_dir_owners`] under an explicit normalized runtime base.
fn runtime_dir_owners_in(base: &Path, path: &Path) -> Vec<PathBuf> {
    let mut owners: Vec<PathBuf> = Vec::new();
    while owners.len() < MAX_RUNTIME_OWNER_DEPTH {
        let current = owners.last().map_or(path, PathBuf::as_path);
        match containing_runtime_dir(base, current).and_then(|name| recorded_owner(base, &name)) {
            Some(owner) => owners.push(owner),
            None => break,
        }
    }
    owners
}

/// The policy-intersection view of [`runtime_dir_owners`]: whether the policy
/// root `candidate` lies in the runtime dir of a workspace inside the policy
/// root `root`, so requesting it narrows `root` rather than leaving it.
pub(crate) fn root_in_owned_runtime_dir(candidate: &str, root: &str) -> bool {
    let owners = runtime_dir_owners(&super::render_policy_root(candidate));
    if owners.is_empty() {
        return false;
    }
    let root = super::render_policy_root(root);
    owners.iter().any(|owner| path_is_within(owner, &root))
}

/// The runtime dir holding the normalized `path`, when it has a valid owner.
/// The fd-scoped mutation path opens it as the root of such a write.
pub(super) fn owned_runtime_dir_containing(path: &Path) -> Option<PathBuf> {
    let base = runtime_base()?;
    let name = containing_runtime_dir(&base, path)?;
    recorded_owner(&base, &name).map(|_| base.join(name))
}

/// The name of the runtime dir under `base` that holds the normalized `path`.
fn containing_runtime_dir(base: &Path, path: &Path) -> Option<String> {
    let name = path.strip_prefix(base).ok()?.components().next()?;
    let name = name.as_os_str().to_str()?;
    is_runtime_digest_name(name).then(|| name.to_string())
}

/// The workspace root a runtime dir's marker records. The marker is only a
/// hint: it is trusted when the root it names digests to the dir's own name,
/// so no file can claim a runtime dir for a workspace it does not belong to.
#[cfg(unix)]
fn recorded_owner(base: &Path, name: &str) -> Option<PathBuf> {
    read_runtime_marker(&super::open_dir_absolute(base).ok()?, name)
}

/// The one reader of a runtime dir's marker, shared by the trust check and the
/// stale sweep. The child can rewrite its runtime dir, so nothing it plants
/// may block or redirect the read: the dir is opened without following and
/// must be this user's, and the marker is opened without following or
/// blocking and must be a regular file before its bounded read. A FIFO,
/// symlink, oversized, relative, or mismatched marker names no owner.
#[cfg(unix)]
fn read_runtime_marker(base_fd: &std::os::fd::OwnedFd, name: &str) -> Option<PathBuf> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let dir = super::open_dir_at(base_fd.as_raw_fd(), name).ok()?;
    restrict_to_owner(&dir).ok()?;
    let marker = super::openat_file(
        dir.as_raw_fd(),
        RUNTIME_DIR_MARKER,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .ok()?;
    if !marker.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut recorded = Vec::new();
    marker
        .take(RUNTIME_DIR_MARKER_MAX + 1)
        .read_to_end(&mut recorded)
        .ok()?;
    trusted_owner(
        PathBuf::from(OsStr::from_bytes(&recorded)),
        recorded.len(),
        name,
    )
}

/// Windows has no FIFO or no-follow open; a marker that is not a plain regular
/// file (a symlink or other reparse point included) names no owner.
#[cfg(not(unix))]
fn recorded_owner(base: &Path, name: &str) -> Option<PathBuf> {
    use std::io::Read;

    let marker = base.join(name).join(RUNTIME_DIR_MARKER);
    if !marker.symlink_metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut recorded = Vec::new();
    std::fs::File::open(marker)
        .ok()?
        .take(RUNTIME_DIR_MARKER_MAX + 1)
        .read_to_end(&mut recorded)
        .ok()?;
    let length = recorded.len();
    trusted_owner(
        PathBuf::from(String::from_utf8(recorded).ok()?),
        length,
        name,
    )
}

/// A read-back marker root, trusted only within the bound, absolute, and
/// digesting to its runtime dir's own name.
fn trusted_owner(root: PathBuf, length: usize, name: &str) -> Option<PathBuf> {
    let trusted = length as u64 <= RUNTIME_DIR_MARKER_MAX
        && root.is_absolute()
        && runtime_digest(&root) == name;
    trusted.then_some(root)
}

/// The runtime dir for the policy's first workspace root, without creating it.
/// The first normalized root is always the first base root, so this skips the
/// git topology walk.
fn planned_runtime_dir(policy: &CapabilityPolicy) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    sandbox_runtime_dir(base_workspace_roots(policy).first()?)
}

/// The runtime dir as a write root, created when missing. Every backend and
/// the in-process scope check grant it beside the workspace roots, including
/// when only declared roots are in force. `None` when it cannot be created
/// safely, so no backend is asked to grant a path that is not there.
pub(super) fn granted_runtime_dir(
    policy: &CapabilityPolicy,
    base_roots: &[PathBuf],
) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    let root = base_roots.first()?;
    let dir = sandbox_runtime_dir(root)?;
    let present = dir
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir());
    if !present {
        if let Err(error) = ensure_runtime_dir(root, None) {
            warn_once(
                "handler_sandbox_runtime_dir",
                &format!(
                    "could not create the sandbox runtime dir '{}': {error}; \
                     confined children get no private temp dir or toolchain cache",
                    dir.display()
                ),
            );
            return None;
        }
    }
    Some(dir)
}

fn create_runtime_dir(
    policy: &CapabilityPolicy,
    name: &str,
    warning_key: &str,
    label: &str,
) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    let root = base_workspace_roots(policy).into_iter().next()?;
    match ensure_runtime_dir(&root, Some(name)) {
        Ok(path) => Some(path),
        Err(error) => {
            warn_once(
                warning_key,
                &format!(
                    "could not create the sandbox {label} for '{}': {error}; \
                     leaving the child's inherited environment in place",
                    root.display()
                ),
            );
            None
        }
    }
}

/// Create the runtime dir for `workspace_root`, and `child` inside it, and
/// return the deepest. `harn-sandbox`, the digest dir, and the child are
/// owner-only. The user cache directory above them is the user's own and may
/// be a symlink; every owned component is opened without following one, and
/// a component owned by another user is refused rather than shared. The
/// digest dir records its workspace root, and the first creation in a process
/// sweeps the runtime dirs of workspaces that no longer exist.
#[cfg(unix)]
fn ensure_runtime_dir(workspace_root: &Path, child: Option<&str>) -> std::io::Result<PathBuf> {
    let root = normalize_for_policy(workspace_root);
    let base = runtime_base().ok_or_else(|| {
        std::io::Error::other("no user cache directory for the sandbox runtime dir")
    })?;
    let digest = runtime_digest(&root);
    let (Some(cache), Some(base_name)) = (base.parent(), base.file_name().and_then(OsStr::to_str))
    else {
        return Err(std::io::Error::other(
            "the sandbox runtime base has no parent",
        ));
    };
    std::fs::create_dir_all(cache)?;
    let cache_fd = super::open_dir_absolute(&std::fs::canonicalize(cache)?)?;
    let base_fd = open_private_dir_at(&cache_fd, base_name)?;
    let runtime_fd = open_private_dir_at(&base_fd, &digest)?;
    write_runtime_marker(&runtime_fd, &root)?;
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| sweep_stale_runtime_dirs(&base_fd, &base, &digest));
    let runtime = base.join(&digest);
    match child {
        Some(name) => {
            open_private_dir_at(&runtime_fd, name)?;
            Ok(runtime.join(name))
        }
        None => Ok(runtime),
    }
}

/// Open the owner-only directory `name` under `parent`, creating it 0700.
#[cfg(unix)]
fn open_private_dir_at(
    parent: &std::os::fd::OwnedFd,
    name: &str,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;

    let dir = match super::open_dir_at(parent.as_raw_fd(), name) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match mkdirat_private(parent.as_raw_fd(), name) {
                Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                    return Err(error)
                }
                _ => {}
            }
            super::open_dir_at(parent.as_raw_fd(), name)?
        }
        Err(error) => return Err(error),
    };
    restrict_to_owner(&dir)?;
    Ok(dir)
}

/// Create the directory `name` under `parent_fd` with mode 0700.
#[cfg(unix)]
fn mkdirat_private(parent_fd: libc::c_int, name: &str) -> std::io::Result<()> {
    let c_name = super::c_name(name)?;
    // SAFETY: `c_name` is a valid NUL-terminated string for the whole call.
    if unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Record the workspace root in its runtime dir, once.
#[cfg(unix)]
fn write_runtime_marker(runtime: &std::os::fd::OwnedFd, root: &Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    match super::openat_file(
        runtime.as_raw_fd(),
        RUNTIME_DIR_MARKER,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    ) {
        Ok(mut file) => file.write_all(root.as_os_str().as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Remove the runtime dirs under `base` whose workspace root no longer exists.
///
/// Only a digest-named directory owned by this user, reached without following
/// a symlink, whose marker names a root that hashes to that same digest, is a
/// candidate, and only when that root is definitely gone. A dir with no
/// marker, a marker a child rewrote, or a root that cannot be examined is
/// left alone. `own` is never removed. Best effort: a failure leaves the dir
/// for the next process.
#[cfg(unix)]
fn sweep_stale_runtime_dirs(base_fd: &std::os::fd::OwnedFd, base: &Path, own: &str) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_runtime_digest_name(name) || name == own {
            continue;
        }
        let Some(root) = read_runtime_marker(base_fd, name) else {
            continue;
        };
        if matches!(root.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            let _ = std::fs::remove_dir_all(base.join(name));
        }
    }
}

#[cfg(unix)]
fn restrict_to_owner(dir: &std::os::fd::OwnedFd) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::File::from(dir.try_clone()?).metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "sandbox runtime dir is owned by another user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        // SAFETY: the descriptor is open for the duration of the call.
        if unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_runtime_dir(workspace_root: &Path, child: Option<&str>) -> std::io::Result<PathBuf> {
    let root = normalize_for_policy(workspace_root);
    let runtime = sandbox_runtime_dir(&root).ok_or_else(|| {
        std::io::Error::other("no user cache directory for the sandbox runtime dir")
    })?;
    std::fs::create_dir_all(&runtime)?;
    let marker = runtime.join(RUNTIME_DIR_MARKER);
    if !marker.exists() {
        let _ = std::fs::write(marker, root.to_string_lossy().as_bytes());
    }
    let path = child.map_or(runtime.clone(), |name| runtime.join(name));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// A script's scratch dir, `.harn-tmp` in the first writable workspace root,
/// created on first use. Its paths are inside the workspace, which is what
/// `mkdtemp_in_workspace` promises: an embedder's workspace-root checks accept
/// them. `None` when the profile does not scope paths.
pub(crate) fn workspace_script_scratch_dir(policy: &CapabilityPolicy) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    let root = base_workspace_roots(policy).into_iter().next()?;
    if let Err(error) = initialize_script_scratch_dir(&root) {
        warn_once(
            "handler_sandbox_workspace_scratch",
            &format!(
                "could not create the workspace scratch dir in '{}': {error}",
                root.display()
            ),
        );
        return None;
    }
    Some(root.join(WORKSPACE_TMPDIR_NAME))
}

#[cfg(unix)]
fn initialize_script_scratch_dir(root: &Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let (parent, leaf) = super::ensure_parent_dirs_scoped(&super::ScopedMutationTarget {
        root: root.to_path_buf(),
        relative: Path::new(WORKSPACE_TMPDIR_NAME).join(".gitignore"),
    })?;
    // Retain the directory descriptor through publication: replacing the
    // workspace path after mkdir-p must never create a host ignore file.
    if let Ok(mut file) = super::openat_file(
        parent.as_raw_fd(),
        &leaf,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    ) {
        let _ = file.write_all(crate::runtime_paths::SELF_IGNORE_CONTENTS.as_bytes());
    }
    Ok(())
}

#[cfg(not(unix))]
fn initialize_script_scratch_dir(root: &Path) -> std::io::Result<()> {
    super::create_dir_all_scoped_target(&super::ScopedMutationTarget {
        root: root.to_path_buf(),
        relative: PathBuf::from(WORKSPACE_TMPDIR_NAME),
    })?;
    let _ = crate::runtime_paths::ensure_self_ignored_dir(&root.join(WORKSPACE_TMPDIR_NAME));
    Ok(())
}

/// The session's own temp dir, `tmp` in the [`sandbox_runtime_dir`] of the
/// first writable workspace root, created on first use. A confined child's
/// `TMPDIR`, `TMP`, and `TEMP` point here. `None` when the profile does not
/// scope paths.
pub fn workspace_local_tmpdir(policy: &CapabilityPolicy) -> Option<PathBuf> {
    create_runtime_dir(
        policy,
        RUNTIME_TMPDIR_NAME,
        "handler_sandbox_workspace_tmpdir",
        "temp dir",
    )
}

/// The toolchain cache, `toolchain` in the same runtime dir.
pub(super) fn workspace_local_toolchain_cache(policy: &CapabilityPolicy) -> Option<PathBuf> {
    create_runtime_dir(
        policy,
        RUNTIME_TOOLCHAIN_CACHE_NAME,
        "handler_sandbox_workspace_toolchain_cache",
        "toolchain cache",
    )
}

/// Preserve a caller-selected toolchain path only when it resolves inside a
/// writable workspace root or the workspace's runtime dir. This lets an outer
/// harness prewarm one cache for the whole run, and a nested confined run keep
/// the cache its parent handed it, without allowing an inherited global path
/// to widen the jail.
fn inherited_workspace_cache_path(policy: &CapabilityPolicy, key: &str) -> Option<String> {
    if !crate::security::environment_policy::TOOLCHAIN_CACHE_ENV_VARS.contains(&key) {
        return None;
    }
    let raw = match crate::stdlib::process::current_session_environment() {
        Some(environment) => environment.launcher_value(key)?.to_string(),
        None => crate::test_env::env_var_seamed(key)?,
    };
    let candidate = PathBuf::from(raw.trim());
    if !candidate.is_absolute() {
        return None;
    }
    let resolved = normalize_for_policy(&candidate);
    // A path under one of the policy's own process write roots is kept too.
    // The operator already made that directory writable for every confined
    // child, so pointing the toolchain at it grants nothing new. Relocating it
    // instead hid an existing cache: a warm package cache the embedder had
    // granted for exactly this purpose was replaced by an empty one, and an
    // offline resolve that works in the operator's terminal failed in the
    // child.
    if granted_write_root_contains(policy, &resolved) {
        return Some(resolved.display().to_string());
    }
    base_workspace_roots(policy)
        .into_iter()
        .chain(planned_runtime_dir(policy))
        .any(|root| path_is_within(&resolved, &root))
        .then(|| resolved.display().to_string())
}

/// Whether `path` lies under one of the policy's explicit process write roots.
fn granted_write_root_contains(policy: &CapabilityPolicy, path: &Path) -> bool {
    policy
        .process_sandbox
        .write_roots
        .iter()
        .map(|root| PathBuf::from(root.trim()))
        .filter(|root| root.is_absolute())
        .any(|root| path_is_within(path, &normalize_for_policy(&root)))
}

/// Where a toolchain home came from, so a caller can say which it did rather
/// than reporting only the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolchainHomeSource {
    /// The variable was already set, and that value is kept.
    Inherited,
    /// The variable was unset, so it is derived from the home directory.
    DerivedFromHome,
}

/// A toolchain home and the reason it holds that value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolchainHome {
    pub path: String,
    pub source: ToolchainHomeSource,
}

/// Resolve one toolchain home for a confined child.
///
/// An explicitly set value wins. Deriving from the home directory is the
/// fallback for a name nobody set, never an override of one somebody did.
///
/// The order matters, and not only for a relocated home. A value that is
/// recomputed is not a value that is inherited, and the difference is
/// invisible in the result: both are absolute paths that look plausible. It
/// shows up one layer down as a toolchain that is not there.
///
/// An empty or relative setting is treated as unset. A relative toolchain home
/// resolves against the child's working directory, which is not where the
/// caller meant, and silently honouring one would be a third answer nobody
/// asked for.
pub(crate) fn toolchain_home(name: &str, suffix: &str) -> Option<ToolchainHome> {
    toolchain_home_from(
        std::env::var_os(name).as_deref(),
        crate::user_dirs::home_dir(),
        suffix,
    )
}

/// The resolution itself, taking what it reads.
///
/// Split from the reader the same way [`crate::user_dirs::home_dir_from`] is,
/// and for the same reason: a rule that consults the process environment can
/// only be asserted by mutating it, and a test that mutates the process
/// environment is a test that races every other test in the binary. This one
/// takes its inputs, so the rule is provable without touching the process at
/// all.
pub(crate) fn toolchain_home_from(
    explicit: Option<&OsStr>,
    home: Option<PathBuf>,
    suffix: &str,
) -> Option<ToolchainHome> {
    if let Some(explicit) = explicit
        .map(PathBuf::from)
        .filter(|value| value.is_absolute())
    {
        return Some(ToolchainHome {
            path: explicit.display().to_string(),
            source: ToolchainHomeSource::Inherited,
        });
    }
    let home = home.filter(|home| home.is_absolute())?;
    Some(ToolchainHome {
        path: home.join(suffix).display().to_string(),
        source: ToolchainHomeSource::DerivedFromHome,
    })
}

/// `CARGO_HOME` and `RUSTUP_HOME` as a child should see them, by the rule of
/// [`toolchain_home_from`]. Hosts that start a Rust tool outside the
/// workspace environment (a language server) read the same answer.
pub fn rust_toolchain_homes() -> Vec<(&'static str, PathBuf)> {
    [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .into_iter()
        .filter_map(|(name, suffix)| {
            toolchain_home(name, suffix).map(|resolved| (name, PathBuf::from(resolved.path)))
        })
        .collect()
}

fn workspace_toolchain_env_with_package_cache(
    policy: &CapabilityPolicy,
    package_cache: Option<PathBuf>,
) -> Vec<(String, String)> {
    let Some(root) = workspace_local_toolchain_cache(policy) else {
        return Vec::new();
    };
    toolchain_env_at(policy, &root, package_cache, true)
}

/// The toolchain environment rooted at `root`. `seed` writes the credential-free
/// npm and Composer stand-ins the values point at; a read passes `false` and
/// gets the same values.
fn toolchain_env_at(
    policy: &CapabilityPolicy,
    root: &Path,
    package_cache: Option<PathBuf>,
    seed: bool,
) -> Vec<(String, String)> {
    let path = |key: &str, suffix: &str| {
        inherited_workspace_cache_path(policy, key)
            .unwrap_or_else(|| root.join(suffix).display().to_string())
    };
    // HOME and PYTHONUSERBASE are deliberately NOT relocated.
    //
    // Relocating them was hermetic but wrong for an agent: it moved the user's
    // installed toolchain out from under every command the agent ran. A
    // `pip install --user` package lives at `$HOME/.local/lib/...` and is found
    // through `site.getusersitepackages()`, which follows PYTHONUSERBASE — so
    // with both moved, `python3 -m pytest` reported "No module named pytest"
    // for a pytest that was installed, readable, and working in the user's own
    // terminal. Nothing was refused; the interpreter simply never looked. The
    // agent had no way to learn that, and retried the same command twenty
    // times.
    //
    // The rule is that the agent should see what works in the user's terminal.
    // PURE CACHES stay relocated below — those are derived, reproducible, and
    // writable, so confining them keeps a run from polluting the user's real
    // caches without hiding anything the user installed. Identity and install
    // roots do not move.
    //
    // Read confinement of `$HOME` is a separate axis and still applies: the
    // credential denylist (`ProcessSandboxPolicy::read_deny_roots`) refuses
    // `~/.ssh`, `~/.aws`, and friends whether or not HOME points there.
    let mut env = vec![
        (
            "XDG_CACHE_HOME".to_string(),
            path("XDG_CACHE_HOME", "xdg-cache"),
        ),
        ("GOCACHE".to_string(), path("GOCACHE", "go-build")),
        ("GOMODCACHE".to_string(), path("GOMODCACHE", "go-mod")),
        ("GOPATH".to_string(), path("GOPATH", "go")),
        (
            "GRADLE_USER_HOME".to_string(),
            path("GRADLE_USER_HOME", "gradle"),
        ),
        (
            "SWIFTPM_MODULECACHE_OVERRIDE".to_string(),
            path("SWIFTPM_MODULECACHE_OVERRIDE", "swiftpm/modules"),
        ),
        // clang and swift-frontend default their module cache to the per-user
        // cache dir under `/var/folders`, and xcrun (behind every `/usr/bin`
        // developer shim) keeps its lookup cache in the per-user temp dir.
        // Neither follows TMPDIR, and the profile grants neither: the host's
        // shared temp and cache dirs hold every other process's files, and a
        // confined child that could write these caches could poison the
        // user's own builds. Without the module cache swiftc cannot load the
        // standard library at all.
        (
            "CLANG_MODULE_CACHE_PATH".to_string(),
            path("CLANG_MODULE_CACHE_PATH", "clang-module-cache"),
        ),
        (
            "xcrun_db".to_string(),
            root.join("xcrun/xcrun_db").display().to_string(),
        ),
        (
            "CARGO_TARGET_DIR".to_string(),
            path("CARGO_TARGET_DIR", "cargo-target"),
        ),
        ("PIP_CACHE_DIR".to_string(), path("PIP_CACHE_DIR", "pip")),
        ("UV_CACHE_DIR".to_string(), path("UV_CACHE_DIR", "uv")),
        (
            "NPM_CONFIG_CACHE".to_string(),
            path("NPM_CONFIG_CACHE", "npm"),
        ),
        (
            "YARN_CACHE_FOLDER".to_string(),
            path("YARN_CACHE_FOLDER", "yarn"),
        ),
        (
            "COMPOSER_CACHE_DIR".to_string(),
            path("COMPOSER_CACHE_DIR", "composer"),
        ),
        // ccache is a pure cache like the rest, but its TEMPDIR is the one that
        // actually breaks builds: it defaults to XDG_RUNTIME_DIR
        // (`/run/user/<uid>`), which no workspace write root covers, so a cgo
        // build fails "Permission denied" before compiling anything. Both are
        // relocated for the same reason the other caches are.
        ("CCACHE_DIR".to_string(), path("CCACHE_DIR", "ccache")),
        (
            "CCACHE_TEMPDIR".to_string(),
            path("CCACHE_TEMPDIR", "ccache-tmp"),
        ),
    ];

    // Harn's package cache is resolved before HOME/XDG are relocated. In
    // particular, HARN_CACHE_DIR already names the root itself; treating it as
    // an XDG base and appending `harn` would split install and nested-run state.
    if let Some(package_cache) = package_cache {
        env.push((
            "HARN_CACHE_DIR".to_string(),
            package_cache.display().to_string(),
        ));
    }

    // HOME is no longer relocated (see above), but these keys are still set
    // explicitly so a child that inherits a relocated-looking environment from
    // an outer harness still resolves the user's real toolchains. Cargo's mutable registry/git cache already
    // has the narrowly scoped write grants added in #5170; keeping CARGO_HOME
    // there also preserves private-registry configuration without copying
    // credentials into the workspace.
    //
    // An explicitly set value is kept rather than recomputed, which is what
    // makes the sentence above true. Deriving these from the home directory
    // was the opposite of resolving the user's real toolchains whenever the
    // home was the relocated one: the derivation follows the relocation, so
    // the child was pointed at a toolchain that does not exist. It is also
    // wrong for anyone whose toolchain simply lives somewhere else, which is
    // ordinary on a shared or containerized machine.
    for (name, path) in rust_toolchain_homes() {
        env.push((name.to_string(), path.display().to_string()));
    }
    if let Some(home) = crate::user_dirs::home_dir().filter(|home| home.is_absolute()) {
        for (key, candidate) in [
            ("GIT_CONFIG_GLOBAL", home.join(".gitconfig")),
            ("PIP_CONFIG_FILE", home.join(".config/pip/pip.conf")),
        ] {
            // An explicitly selected Git global config is the source the
            // sandbox root discovery queried. Replacing it with ~/.gitconfig
            // here would send the child to a different config after its roots
            // have already been decided.
            if key == "GIT_CONFIG_GLOBAL" && std::env::var_os(key).is_some() {
                continue;
            }
            if candidate.is_file() {
                env.push((key.to_string(), candidate.display().to_string()));
            }
        }
        // `~/.npmrc` is on the credential denylist, and npm and pnpm read it
        // at startup and exit on the EPERM rather than proceeding without it.
        // Pointing the tool at the denied file, or leaving the default, is the
        // same crash. The stand-in keeps the non-credential lines (registry
        // URLs, scopes, store settings) so a private registry still resolves,
        // and never carries a token into the toolchain cache.
        env.push((
            "NPM_CONFIG_USERCONFIG".to_string(),
            if seed {
                seed_sanitized_npmrc(&home.join(".npmrc"), root, "npm/npmrc")
            } else {
                root.join("npm/npmrc")
            }
            .display()
            .to_string(),
        ));
        // Composer treats an unreadable `auth.json` as fatal, and that file
        // is on the credential denylist. Pointing COMPOSER_HOME at a
        // toolchain-cache stand-in that carries `config.json` but never `auth.json`
        // keeps repositories and preferred-install settings and drops tokens.
        env.push((
            "COMPOSER_HOME".to_string(),
            if seed {
                seed_composer_home(&home, root, "composer/home")
            } else {
                root.join("composer/home")
            }
            .display()
            .to_string(),
        ));
        // GitHub CLI reads `hosts.yml` on every command and exits on the EPERM
        // even when GH_TOKEN is set; a missing file is fine. `hosts.yml` is on
        // the credential denylist, so the stand-in carries `config.yml`
        // (aliases, git protocol, editor) and never `hosts.yml`. A host that
        // grants GH_TOKEN then gets a working `gh`; without one, gh prints its
        // own "gh auth login" message instead of a permission error.
        env.push((
            "GH_CONFIG_DIR".to_string(),
            if seed {
                seed_gh_config_dir(&gh_config_source(&home), root, "gh")
            } else {
                root.join("gh")
            }
            .display()
            .to_string(),
        ));
    }
    env
}

/// The user's GitHub CLI config directory: GH_CONFIG_DIR, then
/// `$XDG_CONFIG_HOME/gh`, then `~/.config/gh`, the order gh itself uses. Read
/// from the session's launcher environment, like the cache paths above, so an
/// embedding host seeds from the session it serves rather than its own process.
pub(crate) fn gh_config_source(operator_home: &Path) -> PathBuf {
    let launcher = |key: &str| match crate::stdlib::process::current_session_environment() {
        Some(environment) => environment.launcher_value(key).map(str::to_string),
        None => crate::test_env::env_var_seamed(key),
    };
    if let Some(dir) = launcher("GH_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = launcher("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir).join("gh");
    }
    operator_home.join(".config/gh")
}

/// Copy the credential-free `config.yml` into the stand-in. gh releases before
/// 1.0 kept `oauth_token` in `config.yml`; such a file is not copied, and a copy
/// left from an earlier spawn is removed. Shell aliases are dropped; see
/// [`gh_config::sanitize_gh_config`].
pub(crate) fn seed_gh_config_dir(source: &Path, cache: &Path, relative: &str) -> PathBuf {
    let raw = std::fs::read_to_string(source.join("config.yml"))
        .ok()
        .and_then(|raw| gh_config::sanitize_gh_config(&raw));
    let _ = publish_stand_in(
        cache,
        &Path::new(relative).join("config.yml"),
        raw.as_deref(),
    );
    cache.join(relative)
}

/// Write the credential-free projection of `source` to the stand-in and return
/// its path. The path is returned even when `source` is absent (npm treats a
/// missing userconfig as empty).
fn seed_sanitized_npmrc(source: &Path, cache: &Path, relative: &str) -> PathBuf {
    let sanitized = std::fs::read_to_string(source)
        .ok()
        .map(|raw| sanitize_npmrc(&raw));
    let _ = publish_stand_in(cache, Path::new(relative), sanitized.as_deref());
    cache.join(relative)
}

/// Toolchain-cache Composer home: copy non-credential files, never `auth.json`.
fn seed_composer_home(operator_home: &Path, cache: &Path, relative: &str) -> PathBuf {
    for name in ["config.json", "composer.json"] {
        let raw = [".composer", ".config/composer"]
            .iter()
            .find_map(|dir| std::fs::read_to_string(operator_home.join(dir).join(name)).ok());
        let _ = publish_stand_in(cache, &Path::new(relative).join(name), raw.as_deref());
    }
    cache.join(relative)
}

/// Publish `contents` at `relative` under the toolchain `cache`, or remove the
/// file when `contents` is `None`, so a stand-in never outlives its source.
///
/// This runs in the unconfined host process, and confined children can write
/// the cache. Every step therefore refuses to follow a symlink: the cache
/// directory and each component under it are opened without following, the
/// existing file is read with `O_NOFOLLOW`, and the new content is published
/// by renaming a staged file over the name, which replaces a planted link
/// rather than writing through it. A spawn on a warm workspace whose stand-in
/// already matches touches nothing.
#[cfg(unix)]
fn publish_stand_in(cache: &Path, relative: &Path, contents: Option<&str>) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    let (Some(root), Some(name)) = (cache.parent(), cache.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "toolchain cache has no parent directory",
        ));
    };
    let (parent, leaf) = super::ensure_parent_dirs_scoped(&super::ScopedMutationTarget {
        root: root.to_path_buf(),
        relative: Path::new(name).join(relative),
    })?;
    let parent = parent.as_raw_fd();
    let Some(contents) = contents else {
        return match super::unlinkat_name(parent, &leaf, 0) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    };
    // O_NONBLOCK keeps a planted FIFO from stalling the spawn.
    if let Ok(mut existing) = super::openat_file(
        parent,
        &leaf,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    ) {
        // Compare the length first so a planted huge file is never read in.
        let mut current = String::new();
        if existing
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() == contents.len() as u64)
            && existing.read_to_string(&mut current).is_ok()
            && current == contents
        {
            return Ok(());
        }
    }
    // A per-spawn staging name, so concurrent seeds never rename each other's
    // half-written file into place.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staging = format!(".{leaf}.harn-staging.{}.{counter}", std::process::id());
    let mut staged = super::openat_file(
        parent,
        &staging,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    )?;
    let published = staged
        .write_all(contents.as_bytes())
        .and_then(|()| super::renameat_name(parent, &staging, parent, &leaf));
    if published.is_err() {
        let _ = super::unlinkat_name(parent, &staging, 0);
    }
    published
}

#[cfg(not(unix))]
fn publish_stand_in(cache: &Path, relative: &Path, contents: Option<&str>) -> std::io::Result<()> {
    let path = cache.join(relative);
    let Some(contents) = contents else {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    };
    if std::fs::read_to_string(&path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Drop every npmrc line that carries a credential. npm scopes auth to a
/// registry as `//host/path/:_authToken=…` (also `:_auth`, `:_password`,
/// `:username`, `:email`, `:certfile`, `:keyfile`), and the legacy unscoped
/// forms are `_auth`, `_authToken`, and `_password`. Any key whose final
/// segment begins with `_`, or any registry-scoped (`//…/:`) key, goes; plain
/// settings (`registry=`, `@scope:registry=`, `store-dir=`) stay.
fn sanitize_npmrc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let key = trimmed
            .split_once('=')
            .map(|(key, _)| key.trim_end())
            .unwrap_or(trimmed);
        let leaf = key.rsplit(':').next().unwrap_or(key).trim();
        let credential = key.starts_with("//") || leaf.starts_with('_');
        if !credential {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn workspace_toolchain_env(policy: &CapabilityPolicy) -> Vec<(String, String)> {
    workspace_toolchain_env_with_package_cache(policy, crate::user_dirs::package_cache_dir())
}

pub(crate) fn inject_workspace_tmpdir(env: &mut Vec<(String, String)>, policy: &CapabilityPolicy) {
    if let Some(tmpdir) = workspace_local_tmpdir(policy) {
        push_tmpdir(env, &tmpdir);
    }
}

fn push_tmpdir(env: &mut Vec<(String, String)>, tmpdir: &Path) {
    let tmpdir = tmpdir.display().to_string();
    for key in TMPDIR_ENV_KEYS {
        if !env.iter().any(|(existing, _)| existing == key) {
            env.push((key.to_string(), tmpdir.clone()));
        }
    }
}

fn push_absent(env: &mut Vec<(String, String)>, entries: Vec<(String, String)>) {
    for (key, value) in entries {
        if !env.iter().any(|(existing, _)| existing == &key) {
            env.push((key, value));
        }
    }
}

/// The environment a confined child starts with. Creates the workspace temp
/// and cache directories and, on macOS, the launchers that keep the child's
/// temporary storage in the workspace's runtime dir.
pub(crate) fn inject_workspace_process_env(
    env: &mut Vec<(String, String)>,
    policy: &CapabilityPolicy,
) {
    inject_workspace_tmpdir(env, policy);
    #[cfg(target_os = "macos")]
    super::macos::private_temp::inject_env(env, policy);
    push_absent(env, workspace_toolchain_env(policy));
    inject_jvm_loopback_env(env, policy);
    #[cfg(target_os = "macos")]
    super::macos::swiftpm::inject_env(env, policy);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pinned_pnpm::inject_env(env, policy);
}

/// The workspace defaults an in-process environment read sees, computed
/// without touching disk. A read must not materialize anything: a provider
/// catalog reads the environment once per provider, and building the child
/// environment for each read rewrote toolchain launchers every agent step.
/// The macOS launchers and temp alias exist only for children, so they are
/// absent here.
fn workspace_process_env_for_read(policy: &CapabilityPolicy) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let runtime = planned_runtime_dir(policy);
    if let Some(tmpdir) = runtime.as_ref().map(|dir| dir.join(RUNTIME_TMPDIR_NAME)) {
        push_tmpdir(&mut env, &tmpdir);
    }
    if let Some(root) = runtime.map(|dir| dir.join(RUNTIME_TOOLCHAIN_CACHE_NAME)) {
        push_absent(
            &mut env,
            toolchain_env_at(policy, &root, crate::user_dirs::package_cache_dir(), false),
        );
    }
    inject_jvm_loopback_env(&mut env, policy);
    env
}

/// The JVM option that makes a loopback-only grant hold for Java children.
const JVM_PREFER_IPV6_ADDRESSES: &str = "-Djava.net.preferIPv6Addresses=true";

/// Make `allow_tcp_loopback` mean what it says for JVM children on macOS.
///
/// The seatbelt matches loopback endpoints by the literal `localhost` filter,
/// and that filter does not match an IPv4-mapped IPv6 address. A dual-stack
/// JVM — the default — binds and connects `127.0.0.1` as `::ffff:127.0.0.1`,
/// so a Gradle daemon, an sbt server, or any Java test server on a granted
/// loopback dies with a bare `SocketException: Operation not permitted`
/// while a Python or Node server beside it works. The seatbelt grammar
/// offers no spelling for the mapped form (measured: numeric hosts are
/// rejected at profile parse), so the fix is on the JVM side: prefer IPv6
/// addresses, so `localhost` resolves to `::1`, which the filter matches.
///
/// Not the IPv4 stack, which also works for one JVM: a server JVM then
/// advertises only `127.0.0.1`, and a child JVM launched without this
/// variable connects to it mapped. Gradle starts its Checkstyle, PMD and
/// CodeNarc workers with a fresh environment, so every code-quality task
/// failed. With IPv6 preferred the server advertises `::1` first, and the
/// child reaches it whatever its own setting. A JVM that names the literal
/// `127.0.0.1` is still refused.
///
/// `JAVA_TOOL_OPTIONS` is the one JVM-wide knob every launcher honours
/// (Gradle's wrapper, sbt, Maven, `java` itself). A caller's own value is
/// kept and appended to, never replaced; a caller that already chose an IP
/// stack is left alone. The JVM announces the variable on stderr
/// (`Picked up JAVA_TOOL_OPTIONS: ...`); that line is the price of a loopback
/// grant that actually works, and it is on stderr, not stdout.
fn inject_jvm_loopback_env(env: &mut Vec<(String, String)>, policy: &CapabilityPolicy) {
    if !cfg!(target_os = "macos") || !policy.process_sandbox.allow_tcp_loopback {
        return;
    }
    let existing = env
        .iter()
        .position(|(key, _)| key == "JAVA_TOOL_OPTIONS")
        .map(|index| env[index].1.clone())
        .or_else(
            || match crate::stdlib::process::current_session_environment() {
                Some(environment) => environment
                    .launcher_value("JAVA_TOOL_OPTIONS")
                    .map(str::to_string),
                None => crate::test_env::env_var_seamed("JAVA_TOOL_OPTIONS"),
            },
        )
        .unwrap_or_default();
    if existing.contains("java.net.preferIPv4Stack")
        || existing.contains("java.net.preferIPv6Addresses")
    {
        return;
    }
    let value = if existing.trim().is_empty() {
        JVM_PREFER_IPV6_ADDRESSES.to_string()
    } else {
        format!("{} {JVM_PREFER_IPV6_ADDRESSES}", existing.trim_end())
    };
    env.retain(|(key, _)| key != "JAVA_TOOL_OPTIONS");
    env.push(("JAVA_TOOL_OPTIONS".to_string(), value));
}

/// Per-workspace temp and mutable toolchain-state defaults for the active
/// restricted execution policy.
pub fn active_workspace_process_env() -> Vec<(String, String)> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Vec::new();
    };
    let mut env = Vec::new();
    inject_workspace_process_env(&mut env, &policy);
    env
}

/// [`active_workspace_process_env`] for one spawn of `program`, without the
/// keys a session grant reaching that spawn already owns. Spawn seams apply
/// these defaults on top of the session-closed environment, so without the
/// filter a workspace default would overwrite a host grant for the same name
/// (for example a `GH_CONFIG_DIR` granted to `gh`).
pub fn active_workspace_process_env_for(program: &str) -> Vec<(String, String)> {
    let mut env = active_workspace_process_env();
    if let Some(environment) = crate::stdlib::process::current_session_environment() {
        env.retain(|(key, _)| {
            !environment.grants().iter().any(|grant| {
                grant.exposed_env_var() == Some(key.as_str()) && grant.reaches_spawn(Some(program))
            })
        });
    }
    env
}

/// [`active_workspace_process_env`] for an in-process read: the same values a
/// read can see, with nothing created on disk.
pub(crate) fn active_workspace_env_for_read() -> Vec<(String, String)> {
    crate::orchestration::current_execution_policy()
        .map(|policy| workspace_process_env_for_read(&policy))
        .unwrap_or_default()
}

#[cfg(test)]
mod toolchain_home_tests {
    use super::{toolchain_home_from, ToolchainHomeSource};
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// An absolute path for the platform the test is running on.
    ///
    /// `/opt/cargo` is absolute on Unix and is NOT on Windows, where a leading
    /// separator without a drive letter is rooted but relative to the current
    /// drive. The function under test asks `is_absolute`, correctly, so a
    /// fixture written with Unix spellings makes every case here read as "no
    /// absolute input" on Windows and the derivation branch answers `None`.
    /// The production behaviour is right on both; only the fixture was Unix.
    fn absolute(parts: &[&str]) -> PathBuf {
        let mut path = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        for part in parts {
            path.push(part);
        }
        assert!(path.is_absolute(), "the fixture root must be absolute here");
        path
    }

    /// What `absolute` renders to, so an expectation never hard-codes `/`.
    fn absolute_string(parts: &[&str]) -> String {
        absolute(parts).display().to_string()
    }

    /// The premise every fixture here rests on, asserted rather than assumed.
    ///
    /// This is the case that would have caught the defect these fixtures had.
    /// A Unix-spelled path is rooted but NOT absolute on Windows, so fixtures
    /// written that way silently stopped exercising the inherited branch there
    /// and the derivation answered nothing at all. Both halves are asserted on
    /// both platforms, so neither can quietly become vacuous again.
    #[test]
    fn the_fixture_root_is_absolute_and_a_unix_spelling_is_not_portable() {
        assert!(absolute(&["opt", "cargo"]).is_absolute());
        assert!(absolute_string(&["users", "real", ".cargo"]).ends_with(".cargo"));
        assert_eq!(PathBuf::from("/opt/cargo").is_absolute(), !cfg!(windows));
    }

    /// An explicitly set toolchain home is kept, not recomputed.
    ///
    /// This is the whole defect. A confined child was handed
    /// `<home>/.cargo` whatever the caller had set, so an outer harness that
    /// relocated the home pointed the child at a toolchain that does not
    /// exist, and an operator whose toolchain lives elsewhere had theirs
    /// silently replaced.
    #[test]
    fn an_explicit_toolchain_home_is_inherited_not_derived() {
        let resolved = toolchain_home_from(
            Some(&OsString::from(absolute(&["opt", "cargo"]))),
            Some(absolute(&["relocated", "home"])),
            ".cargo",
        )
        .expect("a set value always resolves");
        assert_eq!(resolved.path, absolute_string(&["opt", "cargo"]));
        assert_eq!(resolved.source, ToolchainHomeSource::Inherited);
    }

    /// And the derivation still happens for a name nobody set, so the fix does
    /// not turn into "the child gets nothing".
    #[test]
    fn an_unset_toolchain_home_is_derived_from_the_home_directory() {
        let resolved = toolchain_home_from(None, Some(absolute(&["users", "real"])), ".rustup")
            .expect("an absolute home always resolves");
        assert_eq!(
            resolved.path,
            absolute_string(&["users", "real", ".rustup"])
        );
        assert_eq!(resolved.source, ToolchainHomeSource::DerivedFromHome);
    }

    /// The negative control for the overwrite: with both inputs present the
    /// derived path must not be what comes back. Asserting only the value
    /// would pass against a version that returned the explicit path for the
    /// wrong reason, which is why the source is asserted beside it.
    #[test]
    fn a_set_value_is_never_overwritten_by_the_derived_one() {
        let derived_would_be = absolute_string(&["relocated", "home", ".rustup"]);
        let resolved = toolchain_home_from(
            Some(&OsString::from(absolute(&["opt", "rustup"]))),
            Some(absolute(&["relocated", "home"])),
            ".rustup",
        )
        .expect("a set value always resolves");
        assert_ne!(
            resolved.path, derived_would_be,
            "the derivation overwrote a value the caller set"
        );
        assert_eq!(resolved.source, ToolchainHomeSource::Inherited);
    }

    /// A relative or empty setting is not a third answer.
    ///
    /// A relative toolchain home resolves against the child's working
    /// directory, which is not where whoever set it meant. Honouring it would
    /// be worse than either branch, so it reads as unset.
    #[test]
    fn a_relative_or_empty_setting_reads_as_unset() {
        for setting in ["", "relative/cargo"] {
            let resolved = toolchain_home_from(
                Some(&OsString::from(setting)),
                Some(absolute(&["users", "real"])),
                ".cargo",
            )
            .expect("the home directory still resolves");
            assert_eq!(resolved.path, absolute_string(&["users", "real", ".cargo"]));
            assert_eq!(resolved.source, ToolchainHomeSource::DerivedFromHome);
        }
    }

    /// With nothing set and no usable home there is no answer to give, and
    /// inventing a relative one would point the child at its own workspace.
    #[test]
    fn no_setting_and_no_absolute_home_resolves_to_nothing() {
        assert!(toolchain_home_from(None, None, ".cargo").is_none());
        assert!(toolchain_home_from(None, Some(PathBuf::from("relative")), ".cargo").is_none());
    }
}

#[cfg(test)]
#[path = "workspace_env_tests.rs"]
mod tests;
