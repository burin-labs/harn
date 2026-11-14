//! Where a command-bound grant's program and private files may come from.
//!
//! A credential handed to one program is only as safe as the files that
//! program reads and executes. Everything a confined child can write, and
//! everything inside a workspace, is untrusted here: the grant is exposed
//! only to a program file outside all of it, and that program's private
//! configuration lives in a fresh owner-only directory outside all of it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Every directory a workspace file or a confined child could occupy, made
/// canonical: workspace roots, sandbox write roots, toolchain and package
/// caches, the workspace-local temp dir, the process and spawn cwd, and the
/// host temp dir.
pub(crate) fn untrusted_roots(spawn_cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        spawn_cwd.to_path_buf(),
        std::env::temp_dir(),
        crate::stdlib::process::execution_root_path(),
    ];
    roots.extend(std::env::current_dir());
    roots.extend(crate::stdlib::process::project_root_path());
    roots.extend(crate::user_dirs::package_cache_dir());
    let policies = crate::orchestration::current_execution_policy()
        .into_iter()
        .chain(super::super::active_sandbox_policy().map(|(policy, _)| policy));
    for policy in policies {
        roots.extend(super::super::process_sandbox_roots(&policy));
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "openbsd",
            target_os = "windows"
        ))]
        roots.extend(super::super::process_sandbox_policy_write_roots(&policy));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        roots.extend(super::super::process_sandbox_developer_toolchain_cache_roots(&policy));
        roots.extend(super::workspace_local_tmpdir(&policy));
    }
    let mut roots: Vec<PathBuf> = roots
        .into_iter()
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root))
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

fn is_untrusted(path: &Path, untrusted: &[PathBuf]) -> bool {
    untrusted.iter().any(|root| path.starts_with(root))
}

/// The canonical `PATH` entries a granted program may be found in, and may
/// find its own helpers (git, ssh) in: absolute, existing, and outside every
/// untrusted root. Relative and empty entries are dropped.
pub(crate) fn trusted_path_entries(path: &str, untrusted: &[PathBuf]) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    for entry in std::env::split_paths(path) {
        if !entry.is_absolute() {
            continue;
        }
        let Ok(canonical) = std::fs::canonicalize(&entry) else {
            continue;
        };
        if canonical.is_dir()
            && !is_untrusted(&canonical, untrusted)
            && !entries.contains(&canonical)
        {
            entries.push(canonical);
        }
    }
    entries
}

/// The one file a spawn of `program` may run with a grant: the first match for
/// its bare name on the trusted entries, made canonical. A program named by a
/// relative path is never trusted, and one named by an absolute path is
/// trusted only when it is that same file. `None` when the first match
/// resolves into an untrusted root.
pub(crate) fn trusted_program(
    program: &str,
    entries: &[PathBuf],
    untrusted: &[PathBuf],
) -> Option<PathBuf> {
    let requested = Path::new(program);
    let name = requested.file_name()?.to_str()?;
    if requested.components().count() > 1 && !requested.is_absolute() {
        return None;
    }
    let found = entries.iter().find_map(|entry| {
        executable_names(name)
            .map(|candidate| entry.join(candidate))
            .find(|candidate| is_executable_file(candidate))
    })?;
    let canonical = std::fs::canonicalize(found).ok()?;
    if is_untrusted(&canonical, untrusted) || !is_native_executable(&canonical) {
        return None;
    }
    if requested.is_absolute() && std::fs::canonicalize(requested).ok()? != canonical {
        return None;
    }
    Some(canonical)
}

/// Whether `path` is a compiled executable, not a script or a version-manager
/// shim that would pick the real program from files in the working directory.
fn is_native_executable(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut magic));
    read.is_ok()
        && (magic == *b"\x7fELF"
            || matches!(
                u32::from_be_bytes(magic),
                0xFEED_FACE | 0xFEED_FACF | 0xCEFA_EDFE | 0xCFFA_EDFE | 0xCAFE_BABE | 0xBEBA_FECA
            )
            || (cfg!(windows) && magic[..2] == *b"MZ"))
}

/// The files git and gh read as the user's own configuration.
pub(crate) fn trusted_config_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        home.join(".gitconfig"),
        home.join(".config/git"),
        home.join(".ssh"),
        home.join(".config/gh"),
        super::gh_config_source(home),
        PathBuf::from("/etc/gitconfig"),
        PathBuf::from("/etc/ssh"),
    ];
    paths.extend(std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from));
    paths
        .into_iter()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .collect()
}

fn executable_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let windows = cfg!(windows) && Path::new(name).extension().is_none();
    std::iter::once(name.to_string()).chain(
        [".exe", ".cmd", ".bat"]
            .into_iter()
            .filter(move |_| windows)
            .map(move |suffix| format!("{name}{suffix}")),
    )
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// A fresh owner-only directory for one granted spawn, removed on drop.
pub(crate) struct SpawnDir {
    path: PathBuf,
}

impl SpawnDir {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SpawnDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

static NEXT_SPAWN_DIR: AtomicU64 = AtomicU64::new(0);

/// Where every per-spawn directory lives. It holds credentials while a granted
/// spawn runs, so every confined child has it on its read denylist
/// ([`denied_read_root`]). The one exception is the granted spawn's own leaf,
/// which its own profile re-allows ([`own_spawn_read_root`]).
pub(crate) fn grant_spawns_root() -> PathBuf {
    crate::mcp_oauth::harn_home_dir().join("grant-spawns")
}

thread_local! {
    static OWN_SPAWN_LAUNCHING: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The per-spawn root every confined child is denied. A process with neither a
/// Harn home nor a user home has none: its fallback is the temp dir, where
/// [`create_spawn_dir`] always refuses, so no credential can land there, and
/// denying it would refuse every hardened spawn whose workspace holds that dir.
///
/// The path is canonical, like every per-spawn directory [`create_spawn_dir`]
/// makes: the sandbox matches resolved paths, so a Harn home reached through a
/// link would otherwise leave the denial naming a path nothing resolves to.
pub(crate) fn denied_read_root() -> Option<PathBuf> {
    crate::mcp_oauth::configured_harn_home_dir().map(|home| canonical_spawns_root(&home))
}

/// `home`'s per-spawn root with every link resolved, before or after the root
/// itself exists.
fn canonical_spawns_root(home: &Path) -> PathBuf {
    let root = home.join("grant-spawns");
    std::fs::canonicalize(&root)
        .or_else(|_| std::fs::canonicalize(home).map(|home| home.join("grant-spawns")))
        .unwrap_or(root)
}

/// The one per-spawn directory the spawn this thread is launching may read:
/// its own, re-allowed inside the denied root by the backend that builds its
/// profile. Every other spawn's directory stays denied to it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn own_spawn_read_root() -> Option<PathBuf> {
    OWN_SPAWN_LAUNCHING.with(|own| own.borrow().clone())
}

/// Re-allows one per-spawn directory to the program launched on this thread
/// until dropped. Held only around launching the granted program itself.
pub struct OwnSpawnLaunch(());

impl OwnSpawnLaunch {
    pub(crate) fn begin(dir: &Path) -> Self {
        OWN_SPAWN_LAUNCHING.with(|own| *own.borrow_mut() = Some(dir.to_path_buf()));
        Self(())
    }
}

impl Drop for OwnSpawnLaunch {
    fn drop(&mut self) {
        OWN_SPAWN_LAUNCHING.with(|own| *own.borrow_mut() = None);
    }
}

/// Network variables for a granted spawn: the managed egress proxy when one
/// confines this spawn, which the host itself runs, and otherwise every proxy
/// variable set empty so a caller or ambient value cannot route the request.
pub(crate) fn network_env() -> Vec<(String, String)> {
    const PROXY_KEYS: [&str; 8] = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ];
    match super::super::active_sandbox_policy()
        .filter(|(policy, _)| policy.process_network_proxy.is_some())
    {
        Some((policy, _)) => super::super::process_output::managed_proxy_environment(&policy)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        None => PROXY_KEYS
            .iter()
            .map(|key| (key.to_string(), String::new()))
            .collect(),
    }
}

/// Create a [`SpawnDir`] with `subdirs` under the Harn home, refusing when that
/// location is inside an untrusted root, is a link, or is reachable by any
/// other user. The leaf is created exclusively, so nothing planted in
/// advance can be adopted.
pub(crate) fn create_spawn_dir(
    untrusted: &[PathBuf],
    subdirs: &[&str],
) -> std::io::Result<SpawnDir> {
    let refuse =
        |message: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, message);
    let base = grant_spawns_root();
    private_dir_builder(true).create(&base)?;
    let canonical = std::fs::canonicalize(&base)?;
    if is_untrusted(&canonical, untrusted) {
        return Err(refuse(format!(
            "{} is inside a directory a workspace or confined process can write",
            canonical.display()
        )));
    }
    check_private(&base)?;
    sweep_stale_spawn_dirs(&canonical);
    let leaf = canonical.join(format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT_SPAWN_DIR.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    private_dir_builder(false).create(&leaf)?;
    let dir = SpawnDir { path: leaf };
    for subdir in subdirs {
        private_dir_builder(false).create(dir.path.join(subdir))?;
    }
    Ok(dir)
}

/// A per-spawn directory older than this is removed even if its process
/// still runs: no granted command is meant to live that long, and a reused
/// process id must not keep a crashed spawn's credential forever.
const STALE_SPAWN_DIR: std::time::Duration = std::time::Duration::from_hours(24);

/// Remove every per-spawn directory a crashed or killed process left behind:
/// its creating process is gone, or it is older than [`STALE_SPAWN_DIR`]. An
/// entry that is not a real directory this user owns, or whose name is not a
/// per-spawn name, is left alone and never followed.
fn sweep_stale_spawn_dirs(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    for entry in entries.flatten() {
        let path = entry.path();
        if check_private(&path).is_err() {
            continue;
        }
        let name = entry.file_name();
        let mut parts = name.to_str().unwrap_or_default().split('-');
        let (Some(pid), Some(_), Some(created), None) = (
            parts.next().and_then(|pid| pid.parse::<u32>().ok()),
            parts.next().and_then(|n| n.parse::<u64>().ok()),
            parts.next().and_then(|nanos| nanos.parse::<u128>().ok()),
            parts.next(),
        ) else {
            continue;
        };
        let expired = now.saturating_sub(created) > STALE_SPAWN_DIR.as_nanos();
        if expired || (pid != std::process::id() && !process_is_alive(pid)) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a cheap liveness check, only the age bound applies.
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn private_dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// The base must be a real directory this user owns that nobody else can enter.
fn check_private(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} is not a private directory of this user", path.display()),
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a directory", path.display()),
        ));
    }
    Ok(())
}

/// Write `contents` to a new file only the owner can read.
pub(crate) fn write_private_file(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents.as_bytes())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_trusted_only_outside_every_untrusted_root() {
        let user = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let make = |dir: &Path| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(dir).unwrap();
            let program = dir.join("tool");
            std::fs::write(&program, b"\x7fELF\x02\x01\x01").unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::canonicalize(program).unwrap()
        };
        let trusted = make(&user.path().join("bin"));
        let planted = make(&workspace.path().join("bin"));
        let untrusted = vec![std::fs::canonicalize(workspace.path()).unwrap()];
        let path = std::env::join_paths([
            Path::new("bin"),
            &workspace.path().join("bin"),
            &user.path().join("bin"),
        ])
        .unwrap();
        let entries = trusted_path_entries(path.to_str().unwrap(), &untrusted);
        assert_eq!(entries, vec![trusted.parent().unwrap().to_path_buf()]);
        assert_eq!(
            trusted_program("tool", &entries, &untrusted),
            Some(trusted.clone())
        );
        assert_eq!(
            trusted_program(trusted.to_str().unwrap(), &entries, &untrusted),
            Some(trusted)
        );
        for refused in ["./tool", "bin/tool", planted.to_str().unwrap()] {
            assert_eq!(
                trusted_program(refused, &entries, &untrusted),
                None,
                "{refused}"
            );
        }
        // A script, such as a version-manager shim, is not a native executable.
        let shim = user.path().join("shims");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(shim.join("tool"), "#!/bin/sh\nexec mise x -- tool\n").unwrap();
        std::fs::set_permissions(
            shim.join("tool"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let entries = trusted_path_entries(shim.to_str().unwrap(), &untrusted);
        assert_eq!(trusted_program("tool", &entries, &untrusted), None);
        // A trusted entry whose program links into the workspace is refused.
        let linked = user.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&planted, linked.join("tool")).unwrap();
        let entries = trusted_path_entries(linked.to_str().unwrap(), &untrusted);
        assert_eq!(trusted_program("tool", &entries, &untrusted), None);
    }

    /// A Harn home reached through a link: the denial must name the resolved
    /// root the per-spawn directories live under, or Seatbelt never matches it
    /// and every confined sibling may read them and ask their sockets.
    #[test]
    fn a_linked_harn_home_denies_the_resolved_spawn_root() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real-home");
        std::fs::create_dir(&real).unwrap();
        let linked = base.path().join("linked-home");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let resolved = std::fs::canonicalize(&real).unwrap().join("grant-spawns");
        assert_ne!(linked.join("grant-spawns"), resolved);
        // Before the root exists, and after a spawn directory is made in it.
        assert_eq!(canonical_spawns_root(&linked), resolved);
        std::fs::create_dir(linked.join("grant-spawns")).unwrap();
        let leaf = std::fs::canonicalize(linked.join("grant-spawns"))
            .unwrap()
            .join("1-0-0");
        std::fs::create_dir(&leaf).unwrap();
        assert_eq!(canonical_spawns_root(&linked), resolved);
        assert!(leaf.starts_with(canonical_spawns_root(&linked)));
    }
}
