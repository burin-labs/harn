//! Read-and-execute grants a confined child earns from its `PATH` (harn#8998).

use std::path::{Path, PathBuf};

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};
use crate::stdlib::sandbox::paths::normalize_for_policy;
use crate::stdlib::sandbox::{process_sandbox_presets, sandbox_user_home_dir};

/// One read-and-execute grant derived from a confined child's `PATH`, with the
/// entry that produced it so the grant can be reported against its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathEntryGrant {
    pub(crate) entry: PathBuf,
    pub(crate) root: PathBuf,
}

/// The directories a confined child needs to run the tools on its own `PATH`.
///
/// `PATH` is the host's statement of which programs the child runs, but a
/// shell resolves a bare name inside the already-confined child, so a tool
/// installed outside the preset toolchain homes was refused even when it was
/// first on `PATH`. setup-node on a self-hosted runner installs `node` under
/// `~/actions-runners/<runner>/_work/_tool`, and every sandboxed `node` there
/// exited 126 (harn#8998).
///
/// Outside `home`, an entry grants its install prefix: the parent of a `bin`
/// or `sbin` directory, so the tool reaches its own `lib` and `share`. Inside
/// `home` it grants only the entry itself, because a toolchain home's parent
/// is where credentials live (`~/.cargo/credentials.toml` beside
/// `~/.cargo/bin`, `~/.local/share/keyrings` beside `~/.local/bin`). Neither
/// `/`, `home`, nor an ancestor of `home` is ever granted, and a relative entry
/// grants nothing. The credential denylist is applied by each backend after
/// these grants and wins over them.
pub(crate) fn path_entry_grants(
    path_var: &std::ffi::OsStr,
    home: Option<&Path>,
) -> Vec<PathEntryGrant> {
    let home = home.map(normalize_for_policy);
    let too_broad = |candidate: &Path| {
        candidate.parent().is_none()
            || home
                .as_deref()
                .is_some_and(|home| home.starts_with(candidate))
    };
    let mut grants: Vec<PathEntryGrant> = Vec::new();
    for entry in std::env::split_paths(path_var) {
        if !entry.is_absolute() {
            continue;
        }
        let normalized = normalize_for_policy(&entry);
        if too_broad(&normalized) {
            continue;
        }
        let inside_home = home
            .as_deref()
            .is_some_and(|home| normalized.starts_with(home));
        let prefix = normalized
            .file_name()
            .filter(|name| *name == "bin" || *name == "sbin")
            .and_then(|_| normalized.parent())
            .map(Path::to_path_buf);
        let root = match prefix {
            Some(prefix) if !inside_home && !too_broad(&prefix) => prefix,
            _ => normalized,
        };
        if !grants.iter().any(|grant| grant.root == root) {
            grants.push(PathEntryGrant { entry, root });
        }
    }
    grants
}

/// The read-and-execute grants this process's `PATH` earns a confined child,
/// gated on the `DeveloperToolchains` preset like every other toolchain root.
/// See [`path_entry_grants`] for which directory each entry grants and why.
///
/// It is this process's `PATH`, the one its children inherit, because every
/// spawn path prepares confinement before it applies a command's own
/// environment; a per-command `PATH` override is not visible here. A child
/// given a narrower `PATH` holds grants it will not use, and one given a
/// wider `PATH` is refused the extra entries, which fails closed.
pub(crate) fn process_sandbox_path_entry_grants(policy: &CapabilityPolicy) -> Vec<PathEntryGrant> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        return Vec::new();
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let grants = path_entry_grants(&path_var, sandbox_user_home_dir().as_deref());
    for grant in &grants {
        tracing::debug!(
            target: "harn::sandbox",
            entry = %grant.entry.display(),
            root = %grant.root.display(),
            "sandbox granted a PATH entry's install root read and execute"
        );
    }
    grants
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(path_var: &str, home: &Path) -> Vec<PathBuf> {
        path_entry_grants(std::ffi::OsStr::new(path_var), Some(home))
            .into_iter()
            .map(|grant| grant.root)
            .collect()
    }

    #[test]
    fn outside_home_a_bin_entry_grants_its_install_prefix() {
        let outside = tempfile::tempdir().expect("outside");
        let bin = outside.path().join("node/24/x64/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let home = tempfile::tempdir().expect("home");
        assert_eq!(
            roots(&bin.display().to_string(), home.path()),
            vec![normalize_for_policy(&outside.path().join("node/24/x64"))],
        );
    }

    #[test]
    fn inside_home_only_the_entry_itself_is_granted() {
        let home = tempfile::tempdir().expect("home");
        let cargo_bin = home.path().join(".cargo/bin");
        let local_bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&cargo_bin).expect("cargo bin");
        std::fs::create_dir_all(&local_bin).expect("local bin");
        let granted = roots(
            &format!("{}:{}", cargo_bin.display(), local_bin.display()),
            home.path(),
        );
        assert_eq!(
            granted,
            vec![
                normalize_for_policy(&cargo_bin),
                normalize_for_policy(&local_bin)
            ],
        );
        assert!(
            !granted.contains(&normalize_for_policy(&home.path().join(".cargo"))),
            "the parent of ~/.cargo/bin holds credentials.toml and must not be granted",
        );
    }

    #[test]
    fn root_home_its_ancestors_and_relative_entries_grant_nothing() {
        let home = tempfile::tempdir().expect("home");
        let ancestor = home.path().parent().expect("home has a parent");
        let path_var = format!(
            "/:{}:{}:relative/bin:.",
            home.path().display(),
            ancestor.display()
        );
        assert!(roots(&path_var, home.path()).is_empty());
    }

    #[test]
    fn a_top_level_bin_is_granted_itself_never_the_filesystem_root() {
        let home = tempfile::tempdir().expect("home");
        // `/bin` may resolve to `/usr/bin` (merged-usr Linux), whose prefix is
        // `/usr`; unmerged, its parent is `/` and the entry itself is granted.
        let granted = roots("/bin", home.path());
        assert_eq!(granted.len(), 1, "one grant for one entry: {granted:?}");
        assert!(
            granted[0].parent().is_some(),
            "never the filesystem root: {granted:?}"
        );
    }
}
