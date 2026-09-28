//! The per-user root sets the sandbox grants beyond the workspace.
//!
//! These are data, not policy: each function answers "which directories does
//! this class of tooling keep under a user's home", and the preset-gated
//! wrappers in the parent module decide whether a given policy gets them. Both
//! the OS backends and the pure path-scope checks read the same answers from
//! here, which is what keeps a backend's rendered grant and the parent's view
//! of the jail from drifting apart.

use std::path::{Path, PathBuf};

use super::paths::normalize_for_policy;

/// Per-user toolchain *cache* roots that JVM/iOS build tools read **and write**
/// while a sandboxed build runs (Gradle, Maven, CocoaPods, Xcode, Kotlin
/// Native). Unlike [`developer_toolchain_read_roots_for_home`] these are not
/// read-only: a build legitimately populates `~/.gradle/caches`,
/// `~/.m2/repository`, `~/Library/Developer/Xcode/DerivedData`, etc. They are
/// gated on the `DeveloperToolchains` preset and granted *write* only when the
/// active policy already permits workspace writes (mirroring `UserTemp`); under
/// a read-only policy they fall back to read access so dependency resolution
/// still works.
// Cache *write* roots are only consumed by the macOS (seatbelt) and Linux
// (Landlock) sandbox backends; the Windows backend deliberately does not grant
// recursive home-scoped cache roots (see `windows.rs`). Gating to those two
// targets keeps `-D warnings` happy on Windows, where this would otherwise be
// dead code.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn sandbox_user_home_dir() -> Option<PathBuf> {
    // Only an absolute home grounds the user-scope read-roots below; a
    // relative or unset home yields no extra roots (the safe direction).
    crate::user_dirs::home_dir().filter(|path| path.is_absolute())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn developer_toolchain_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".asdf",
        ".bun",
        ".cargo",
        ".fnm",
        ".juliaup",
        ".local/bin",
        ".local/share/mise",
        ".local/share/pnpm",
        ".local/share/uv",
        ".nvm",
        ".pyenv",
        ".rbenv",
        ".rustup",
        ".sdkman",
        ".swiftly",
        ".volta",
        "Library/pnpm",
        "go",
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    #[cfg(target_os = "windows")]
    roots.extend(
        [
            "AppData/Local/Programs/Python",
            "AppData/Local/uv",
            "AppData/Roaming/uv",
            "scoop",
        ]
        .into_iter()
        .map(|entry| normalize_for_policy(&home.join(entry))),
    );
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// Per-user JVM/iOS toolchain cache roots (read+write). Kept platform-shared so
/// the macOS seatbelt and Linux Landlock backends render the same set; the
/// macOS-only `~/Library/...` entries are simply absent on Linux disk and the
/// `optional`/NotFound handling in each backend skips roots that do not exist.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn developer_toolchain_cache_write_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".gradle",                             // Gradle (JVM/Android/Kotlin)
        ".m2",                                 // Maven (JVM)
        ".konan",                              // Kotlin/Native
        "Library/Caches/CocoaPods",            // CocoaPods (iOS/macOS)
        "Library/Developer/Xcode/DerivedData", // Xcode build products
        // Go build + module caches. `go build`/`go test` write compiled
        // package objects to GOCACHE and downloaded modules to GOMODCACHE;
        // when neither is granted, the toolchain fails — and go reports the
        // write miss as the misleading "package X is not in std (GOROOT/...)"
        // rather than a permissions error, so it reads as a code defect. The
        // default GOCACHE differs by OS (macOS `~/Library/Caches/go-build`,
        // Linux `~/.cache/go-build`); listing both is safe because the
        // OS-foreign entry is simply absent on disk and skipped. GOMODCACHE
        // defaults to `$GOPATH/pkg/mod` (`~/go/pkg/mod`); `~/go` itself stays
        // read-only via `developer_toolchain_read_roots_for_home`.
        "Library/Caches/go-build", // Go build cache (GOCACHE, macOS default)
        ".cache/go-build",         // Go build cache (GOCACHE, Linux default)
        "go/pkg/mod",              // Go module cache (GOMODCACHE default)
        // Harn's own package cache, which needs write and not merely read: an
        // entry is claimed through a lock file under `locks/` before it is
        // read, so a read-only grant denies the claim and the cached entry
        // reads as unusable. Both OS defaults are listed for the same reason
        // the Go caches above are; the OS-foreign one is simply absent.
        // `HARN_CACHE_DIR` overrides the location, and the workspace env hands
        // the child the resolved root so the two always agree.
        "Library/Caches/harn", // Harn package cache (macOS default)
        ".cache/harn",         // Harn package cache (Linux default)
        // Go env config (GOENV). `go` rewrites `go/env` on first use (e.g. to
        // record GOTOOLCHAIN); when its parent is not writable the toolchain
        // fails with `writing go env config: ... operation not permitted`. The
        // macOS default is `~/Library/Application Support/go/env`
        // (`os.UserConfigDir()/go`). The Linux default `~/.config/go/env` sits
        // under the read-only `.config` package-manager root, so granting it
        // needs a nested carve-out and is tracked separately.
        "Library/Application Support/go", // Go env config dir (GOENV, macOS default)
        // Cargo registry + git caches. `cargo fetch`/`cargo build` unpack crate
        // sources into `registry/src`, download tarballs into `registry/cache`,
        // refresh the index under `registry/index`, and check out git deps under
        // `git/db` + `git/checkouts`; a build fails to unpack ("failed to create
        // directory .../registry/src/...: Operation not permitted") when these
        // are read-only. These hold build artifacts only — Cargo credentials and
        // config live at the CARGO_HOME root (`.cargo/credentials.toml`,
        // `.cargo/config.toml`), OUTSIDE `registry`/`git`, and stay read-only
        // (granted read via `.cargo` in `developer_toolchain_read_roots_for_home`
        // and re-denied write by the package-manager preset). `.package-cache` is
        // Cargo's advisory build lock at the CARGO_HOME root.
        ".cargo/registry",       // crate cache/index/src (CARGO_HOME default)
        ".cargo/git",            // git dependency db + checkouts
        ".cargo/.package-cache", // Cargo's advisory build lock file
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn package_manager_config_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".npmrc",
        ".gitconfig",
        ".netrc",
        ".yarnrc.yml",
        ".config",
        ".npm",
        ".cache",
        ".pip",
        ".pypirc",
        // Composer's macOS home (`COMPOSER_HOME` default). `config.json` and
        // the global `composer.json` are configuration; `auth.json` inside it
        // is on the credential denylist, which beats this grant.
        ".composer",
        // pnpm's global rc on macOS (`env-paths` config dir). A registry or
        // store-dir setting, never a token: those live in `.npmrc`.
        "Library/Preferences/pnpm",
        ".cargo/config",
        ".cargo/config.toml",
        ".cargo/credentials",
        ".cargo/credentials.toml",
        // NOTE: `.cargo/registry` and `.cargo/git` are deliberately NOT here.
        // They are build caches Cargo must WRITE, so they moved to
        // `developer_toolchain_cache_write_roots_for_home`. Listing them here
        // too would re-deny their writes: the macOS backend emits a
        // `(deny file-write*)` for every package-manager read root AFTER the
        // write-allow block, and last-match-wins would cancel the cache grant.
        // `.cargo` itself stays readable via `developer_toolchain_read_roots`.
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// One read-and-execute grant derived from a confined child's `PATH`, with the
/// entry that produced it so the grant can be reported against its source.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod path_entry_grant_tests {
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
