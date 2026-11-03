use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

use super::{base_workspace_roots, normalize_for_policy, path_is_within, warn_once};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "pinned_pnpm.rs"]
mod pinned_pnpm;

pub(crate) const TMPDIR_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// The names the temp dir and toolchain cache had when they lived inside the
/// first workspace root. Nothing creates them any more; a checkout that still
/// holds one keeps it out of walks and replay snapshots.
pub(crate) const LEGACY_WORKSPACE_TMPDIR_NAME: &str = ".harn-tmp";
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
    name == OsStr::new(LEGACY_WORKSPACE_TMPDIR_NAME)
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
            match super::mkdirat_mode(parent.as_raw_fd(), name, 0o700) {
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
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let digest_named = name.len() == RUNTIME_DIR_DIGEST_HEX
            && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !digest_named || name == own {
            continue;
        }
        let Ok(dir) = super::open_dir_at(base_fd.as_raw_fd(), name) else {
            continue;
        };
        if restrict_to_owner(&dir).is_err() {
            continue;
        }
        let Ok(marker) = super::openat_file(
            dir.as_raw_fd(),
            RUNTIME_DIR_MARKER,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        ) else {
            continue;
        };
        let mut recorded = Vec::new();
        if marker
            .take(RUNTIME_DIR_MARKER_MAX + 1)
            .read_to_end(&mut recorded)
            .is_err()
            || recorded.len() as u64 > RUNTIME_DIR_MARKER_MAX
        {
            continue;
        }
        let root = Path::new(OsStr::from_bytes(&recorded));
        if !root.is_absolute() || runtime_digest(root) != name {
            continue;
        }
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

/// The temp dir of the runtime dir for `workspace_root`, without creating it.
/// For callers with no path-scoping policy to read the root from.
pub(crate) fn runtime_tmpdir_path(workspace_root: &Path) -> Option<PathBuf> {
    Some(sandbox_runtime_dir(workspace_root)?.join(RUNTIME_TMPDIR_NAME))
}

/// Create the temp dir [`runtime_tmpdir_path`] names.
pub(crate) fn create_runtime_tmpdir(workspace_root: &Path) -> std::io::Result<PathBuf> {
    ensure_runtime_dir(workspace_root, Some(RUNTIME_TMPDIR_NAME))
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
    base_workspace_roots(policy)
        .into_iter()
        .chain(planned_runtime_dir(policy))
        .any(|root| path_is_within(&resolved, &root))
        .then(|| resolved.display().to_string())
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
                seed_sanitized_npmrc(&home.join(".npmrc"), &root.join("npm/npmrc"))
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
                seed_composer_home(&home, &root.join("composer/home"))
            } else {
                root.join("composer/home")
            }
            .display()
            .to_string(),
        ));
    }
    env
}

/// Write the credential-free projection of `source` to `stand_in` and return
/// the stand-in path. The path is returned even when `source` is absent (npm
/// treats a missing userconfig as empty), and the file is rewritten only when
/// its content would change, so a spawn on a warm workspace touches nothing.
fn seed_sanitized_npmrc(source: &Path, stand_in: &Path) -> PathBuf {
    let Ok(raw) = std::fs::read_to_string(source) else {
        return stand_in.to_path_buf();
    };
    let sanitized = sanitize_npmrc(&raw);
    if std::fs::read_to_string(stand_in).ok().as_deref() != Some(sanitized.as_str()) {
        if let Some(parent) = stand_in.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(stand_in, sanitized);
    }
    stand_in.to_path_buf()
}

/// Drop every npmrc line that carries a credential. npm scopes auth to a
/// registry as `//host/path/:_authToken=…` (also `:_auth`, `:_password`,
/// `:username`, `:email`, `:certfile`, `:keyfile`), and the legacy unscoped
/// forms are `_auth`, `_authToken`, and `_password`. Any key whose final
/// segment begins with `_`, or any registry-scoped (`//…/:`) key, goes; plain
/// settings (`registry=`, `@scope:registry=`, `store-dir=`) stay.
/// Toolchain-cache Composer home: copy non-credential files, never `auth.json`.
fn seed_composer_home(operator_home: &Path, dest: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(dest);
    for name in ["config.json", "composer.json"] {
        for source in [
            operator_home.join(".composer").join(name),
            operator_home.join(".config/composer").join(name),
        ] {
            if let Ok(raw) = std::fs::read_to_string(&source) {
                let dest_file = dest.join(name);
                if std::fs::read_to_string(&dest_file).ok().as_deref() != Some(raw.as_str()) {
                    let _ = std::fs::write(&dest_file, raw);
                }
                break;
            }
        }
    }
    dest.to_path_buf()
}

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
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::orchestration::SandboxProfile;

    fn policy(root: &std::path::Path) -> CapabilityPolicy {
        CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![root.display().to_string()],
            ..CapabilityPolicy::default()
        }
    }

    /// The runtime dir is the child's to write, so a child can replace its
    /// `tmp` with a symlink. The next spawn must refuse it rather than hand
    /// the next child, or Harn's own writes, a directory somewhere else.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_runtime_child_is_refused_not_followed() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let policy = policy(workspace.path());
        let tmp = workspace_local_tmpdir(&policy).expect("ordinary temp dir");
        std::fs::remove_dir(&tmp).unwrap();
        std::os::unix::fs::symlink(outside.path(), &tmp).unwrap();

        let redirected = workspace_local_tmpdir(&policy);
        std::fs::remove_dir_all(tmp.parent().unwrap()).unwrap();
        assert_eq!(redirected, None);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    /// Only the runtime dir of a workspace that is gone is swept. A live
    /// workspace's dir, a dir with no marker, a marker naming a root of a
    /// different digest, a symlink, and this process's own dir all survive.
    #[cfg(unix)]
    #[test]
    fn the_sweep_removes_only_marked_runtime_dirs_of_vanished_workspaces() {
        use std::os::unix::ffi::OsStrExt;

        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let live = tempfile::tempdir().unwrap();
        let live_root = live.path().canonicalize().unwrap();
        let gone_root = {
            let gone = tempfile::tempdir().unwrap();
            gone.path().canonicalize().unwrap()
        };
        let mark = |name: &str, root: &Path| {
            let dir = base.path().join(name);
            std::fs::create_dir_all(dir.join("toolchain/go-build")).unwrap();
            std::fs::write(dir.join(RUNTIME_DIR_MARKER), root.as_os_str().as_bytes()).unwrap();
            dir
        };
        let stale = mark(&runtime_digest(&gone_root), &gone_root);
        let kept_live = mark(&runtime_digest(&live_root), &live_root);
        let mismatched = mark("00000000000000aa", &gone_root);
        let unmarked = base.path().join("00000000000000bb");
        std::fs::create_dir(&unmarked).unwrap();
        let own_root = gone_root.join("own");
        let own = mark(&runtime_digest(&own_root), &own_root);
        std::fs::write(
            outside.path().join(RUNTIME_DIR_MARKER),
            gone_root.as_os_str().as_bytes(),
        )
        .unwrap();
        let linked = base.path().join("00000000000000cc");
        std::os::unix::fs::symlink(outside.path(), &linked).unwrap();

        let base_fd =
            super::super::open_dir_absolute(&base.path().canonicalize().unwrap()).unwrap();
        sweep_stale_runtime_dirs(
            &base_fd,
            &base.path().canonicalize().unwrap(),
            own.file_name().unwrap().to_str().unwrap(),
        );

        assert!(
            !stale.exists(),
            "a vanished workspace's runtime dir is swept"
        );
        for survivor in [&kept_live, &mismatched, &unmarked, &own] {
            assert!(survivor.is_dir(), "{survivor:?} must survive the sweep");
        }
        assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(outside.path().join(RUNTIME_DIR_MARKER).is_file());
    }

    /// A created runtime dir records the workspace root the sweep checks.
    #[cfg(unix)]
    #[test]
    fn a_created_runtime_dir_records_its_workspace_root() {
        let workspace = tempfile::tempdir().unwrap();
        let tmp = workspace_local_tmpdir(&policy(workspace.path())).unwrap();
        let runtime = tmp.parent().unwrap().to_path_buf();
        let recorded = std::fs::read_to_string(runtime.join(RUNTIME_DIR_MARKER)).unwrap();
        std::fs::remove_dir_all(&runtime).unwrap();
        assert_eq!(
            Path::new(&recorded),
            workspace.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn process_env_lives_in_the_runtime_dir_outside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = policy(workspace.path());
        let mut env = Vec::new();
        inject_workspace_process_env(&mut env, &policy);
        let env: BTreeMap<_, _> = env.into_iter().collect();

        let runtime = sandbox_runtime_dir(workspace.path()).unwrap();
        let workspace = workspace.path().canonicalize().unwrap();
        let cache = runtime.join(RUNTIME_TOOLCHAIN_CACHE_NAME);
        let tmp = runtime.join(RUNTIME_TMPDIR_NAME);
        assert!(!runtime.starts_with(&workspace) && !workspace.starts_with(&runtime));
        // PURE CACHES relocate. These are derived and reproducible, so
        // confining them keeps a run from polluting the user's real caches.
        for key in [
            "XDG_CACHE_HOME",
            "GOCACHE",
            "GOMODCACHE",
            "GOPATH",
            "GRADLE_USER_HOME",
            "CARGO_TARGET_DIR",
            "PIP_CACHE_DIR",
            "UV_CACHE_DIR",
            "NPM_CONFIG_CACHE",
            "YARN_CACHE_FOLDER",
            "CCACHE_DIR",
            "CCACHE_TEMPDIR",
        ] {
            let value = PathBuf::from(env.get(key).unwrap());
            assert!(value.starts_with(&cache), "{key} escaped cache: {value:?}");
        }
        // IDENTITY AND INSTALL ROOTS DO NOT. Relocating these moved the user's
        // installed toolchain out from under the agent: a `pip install --user`
        // package lives under $HOME and is found through PYTHONUSERBASE, so
        // moving both made `python3 -m pytest` report "No module named pytest"
        // for a pytest that worked in the user's own terminal. Nothing was
        // refused; the interpreter never looked. The agent should see what the
        // user's terminal sees.
        for key in ["HOME", "PYTHONUSERBASE", "PNPM_HOME"] {
            assert!(
                !env.contains_key(key),
                "{key} must NOT be relocated; the child inherits the real one"
            );
        }
        assert!(
            !env.contains_key("NPM_CONFIG_STORE_DIR"),
            "pnpm storage follows the isolated HOME/XDG roots; a global npm_config store-dir leaks an unsupported option into npm"
        );
        assert!(PathBuf::from(env.get("CARGO_HOME").unwrap()).is_absolute());
        assert!(PathBuf::from(env.get("RUSTUP_HOME").unwrap()).is_absolute());
        // Harn's package cache must NOT follow the relocated HOME the way the
        // caches above do. Those can be rebuilt from a registry the child can
        // still reach; a Harn package entry was fetched from a source the
        // child usually cannot reach at all, so a workspace-local Harn cache
        // is an empty one the child then tries to fill over a denied network.
        let harn_cache = PathBuf::from(env.get("HARN_CACHE_DIR").unwrap());
        assert!(harn_cache.is_absolute());
        assert!(
            !harn_cache.starts_with(&cache),
            "HARN_CACHE_DIR followed the relocated HOME into the workspace: {harn_cache:?}"
        );
        assert_eq!(
            Some(harn_cache),
            crate::user_dirs::package_cache_dir(),
            "the child must be handed the same cache root the host resolves"
        );
        for key in TMPDIR_ENV_KEYS {
            assert_eq!(
                env.get(key).map(String::as_str),
                Some(tmp.to_str().unwrap())
            );
        }
        assert!(cache.is_dir() && tmp.is_dir());
        assert_eq!(std::fs::read_dir(&workspace).unwrap().count(), 0);
        std::fs::remove_dir_all(runtime).unwrap();
    }

    #[test]
    fn explicit_values_win_and_unrestricted_is_a_noop() {
        let workspace = tempfile::tempdir().unwrap();
        let mut env = vec![
            ("HOME".to_string(), "/caller/home".to_string()),
            ("GOCACHE".to_string(), "/caller/go".to_string()),
            ("TMPDIR".to_string(), "/caller/tmp".to_string()),
        ];
        inject_workspace_process_env(&mut env, &policy(workspace.path()));
        let env: BTreeMap<_, _> = env.into_iter().collect();
        assert_eq!(env.get("HOME").map(String::as_str), Some("/caller/home"));
        assert_eq!(env.get("GOCACHE").map(String::as_str), Some("/caller/go"));
        assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/caller/tmp"));

        let unrestricted = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Unrestricted,
            workspace_roots: vec![workspace.path().display().to_string()],
            ..CapabilityPolicy::default()
        };
        let mut env = Vec::new();
        inject_workspace_process_env(&mut env, &unrestricted);
        assert!(env.is_empty());
    }

    #[test]
    fn explicit_package_cache_root_is_forwarded_exactly() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = workspace.path().join("operator-cache");
        let env: BTreeMap<_, _> = workspace_toolchain_env_with_package_cache(
            &policy(workspace.path()),
            Some(cache.clone()),
        )
        .into_iter()
        .collect();

        assert_eq!(
            env.get("HARN_CACHE_DIR"),
            Some(&cache.display().to_string())
        );
        assert_ne!(
            env.get("HARN_CACHE_DIR"),
            Some(&cache.join("harn").display().to_string()),
            "HARN_CACHE_DIR is already the cache root, not an XDG base"
        );
    }

    #[test]
    fn the_npmrc_stand_in_keeps_settings_and_drops_every_credential_form() {
        let raw = "registry=https://registry.example.test/\n\
                   @acme:registry=https://npm.acme.test/\n\
                   //npm.acme.test/:_authToken=secret-token\n\
                   //npm.acme.test/:username=alice\n\
                   //registry.npmjs.org/:_auth=YWxpY2U6aHVudGVyMg==\n\
                   _authToken=legacy-secret\n\
                   _password=hunter2\n\
                   store-dir=/opt/pnpm-store\n\
                   always-auth=true\n";
        let sanitized = sanitize_npmrc(raw);
        assert_eq!(
            sanitized,
            "registry=https://registry.example.test/\n\
             @acme:registry=https://npm.acme.test/\n\
             store-dir=/opt/pnpm-store\n\
             always-auth=true\n"
        );
        for secret in ["secret-token", "hunter2", "YWxpY2U", "alice"] {
            assert!(!sanitized.contains(secret), "{secret} leaked: {sanitized}");
        }

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join(".npmrc");
        let stand_in = dir.path().join("cache/npm/npmrc");
        assert_eq!(seed_sanitized_npmrc(&source, &stand_in), stand_in);
        assert!(
            !stand_in.exists(),
            "no source, no file: npm treats absent as empty"
        );
        std::fs::write(&source, raw).unwrap();
        seed_sanitized_npmrc(&source, &stand_in);
        assert_eq!(std::fs::read_to_string(&stand_in).unwrap(), sanitized);
        let written = std::fs::metadata(&stand_in).unwrap().modified().unwrap();
        seed_sanitized_npmrc(&source, &stand_in);
        assert_eq!(
            std::fs::metadata(&stand_in).unwrap().modified().unwrap(),
            written,
            "an unchanged stand-in is not rewritten"
        );
    }

    #[test]
    fn a_confined_child_never_reads_npm_config_from_the_denied_home_file() {
        let workspace = tempfile::tempdir().unwrap();
        let env: BTreeMap<_, _> =
            workspace_toolchain_env_with_package_cache(&policy(workspace.path()), None)
                .into_iter()
                .collect();
        let Some(userconfig) = env.get("NPM_CONFIG_USERCONFIG") else {
            return; // no resolvable home on this host
        };
        let cache = sandbox_runtime_dir(workspace.path())
            .unwrap()
            .join(RUNTIME_TOOLCHAIN_CACHE_NAME);
        assert!(
            PathBuf::from(userconfig).starts_with(&cache),
            "userconfig must be the workspace stand-in, not ~/.npmrc: {userconfig}"
        );
        assert!(
            PathBuf::from(env.get("COMPOSER_CACHE_DIR").unwrap()).starts_with(&cache),
            "composer cache is a pure cache and relocates"
        );
        let composer_home = PathBuf::from(env.get("COMPOSER_HOME").unwrap());
        assert!(
            composer_home.starts_with(&cache),
            "COMPOSER_HOME must be the workspace stand-in: {composer_home:?}"
        );
        assert!(
            !composer_home.join("auth.json").exists(),
            "the stand-in must never carry auth.json"
        );
    }

    #[test]
    fn a_loopback_grant_makes_the_jvm_prefer_ipv6_on_macos() {
        let workspace = tempfile::tempdir().unwrap();
        let mut policy = policy(workspace.path());
        let mut env = Vec::new();
        inject_workspace_process_env(&mut env, &policy);
        assert!(
            !env.iter().any(|(key, _)| key == "JAVA_TOOL_OPTIONS"),
            "no loopback grant, no JVM option"
        );

        policy.process_sandbox.allow_tcp_loopback = true;
        let mut env = vec![("JAVA_TOOL_OPTIONS".to_string(), "-Xmx1g".to_string())];
        inject_workspace_process_env(&mut env, &policy);
        let value = env
            .iter()
            .find(|(key, _)| key == "JAVA_TOOL_OPTIONS")
            .map(|(_, value)| value.clone())
            .unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(value, format!("-Xmx1g {JVM_PREFER_IPV6_ADDRESSES}"));
        } else {
            assert_eq!(value, "-Xmx1g", "only the macOS seatbelt needs the pin");
        }

        // An operator who already chose a stack keeps their choice.
        let mut env = vec![(
            "JAVA_TOOL_OPTIONS".to_string(),
            "-Djava.net.preferIPv4Stack=true".to_string(),
        )];
        inject_workspace_process_env(&mut env, &policy);
        assert_eq!(
            env.iter()
                .filter(|(key, _)| key == "JAVA_TOOL_OPTIONS")
                .count(),
            1
        );
        assert_eq!(env[0].1, "-Djava.net.preferIPv4Stack=true");
    }
}
