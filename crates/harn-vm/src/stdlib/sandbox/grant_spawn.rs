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
    let base = crate::mcp_oauth::harn_home_dir().join("grant-spawns");
    private_dir_builder(true).create(&base)?;
    let canonical = std::fs::canonicalize(&base)?;
    if is_untrusted(&canonical, untrusted) {
        return Err(refuse(format!(
            "{} is inside a directory a workspace or confined process can write",
            canonical.display()
        )));
    }
    check_private(&base)?;
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
}
