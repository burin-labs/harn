use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

use super::{base_workspace_roots, normalize_for_policy, path_is_within, warn_once};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "pinned_pnpm.rs"]
mod pinned_pnpm;

pub(crate) const WORKSPACE_TMPDIR_NAME: &str = ".harn-tmp";
pub(crate) const TMPDIR_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];
pub(crate) const WORKSPACE_TOOLCHAIN_CACHE_NAME: &str = ".harn-toolchain-cache";

/// Reserved workspace directory names owned by the runtime rather than the application.
pub fn is_workspace_runtime_directory(name: &OsStr) -> bool {
    name == OsStr::new(WORKSPACE_TMPDIR_NAME) || name == OsStr::new(WORKSPACE_TOOLCHAIN_CACHE_NAME)
}

/// Where a workspace-local runtime directory lives, without creating it. The
/// first normalized root is always the first base root, so this skips the git
/// topology walk.
fn planned_workspace_dir(policy: &CapabilityPolicy, name: &str) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    Some(base_workspace_roots(policy).into_iter().next()?.join(name))
}

fn create_self_ignored_dir(
    policy: &CapabilityPolicy,
    name: &str,
    warning_key: &str,
    label: &str,
) -> Option<PathBuf> {
    let path = planned_workspace_dir(policy, name)?;
    let root = path.parent()?;
    if let Err(error) = initialize_self_ignored_dir(root, name) {
        warn_once(
            warning_key,
            &format!(
                "could not create workspace-local {label} '{}': {error}; \
                 leaving the child's inherited environment in place",
                path.display()
            ),
        );
        return None;
    }
    Some(path)
}

#[cfg(unix)]
fn initialize_self_ignored_dir(root: &Path, name: &str) -> std::io::Result<()> {
    let (parent, leaf) = super::ensure_parent_dirs_scoped(&super::ScopedMutationTarget {
        root: root.to_path_buf(),
        relative: Path::new(name).join(".gitignore"),
    })?;
    let _ = write_scoped_self_ignore(&parent, &leaf);
    Ok(())
}

#[cfg(unix)]
fn write_scoped_self_ignore(parent: &std::os::fd::OwnedFd, name: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    // Retain the directory descriptor through publication: replacing the
    // workspace path after mkdir-p must never create a host ignore file.
    match super::openat_file(
        parent.as_raw_fd(),
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    ) {
        Ok(mut file) => file.write_all(crate::runtime_paths::SELF_IGNORE_CONTENTS.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn initialize_self_ignored_dir(root: &Path, name: &str) -> std::io::Result<()> {
    super::create_dir_all_scoped_target(&super::ScopedMutationTarget {
        root: root.to_path_buf(),
        relative: PathBuf::from(name),
    })?;
    let _ = crate::runtime_paths::ensure_self_ignored_dir(&root.join(name));
    Ok(())
}

/// The session's own temp dir, `.harn-tmp` in the first writable workspace
/// root, created on first use. A confined child's `TMPDIR`, `TMP`, and `TEMP`
/// point here. `None` when the profile does not scope paths.
pub fn workspace_local_tmpdir(policy: &CapabilityPolicy) -> Option<PathBuf> {
    create_self_ignored_dir(
        policy,
        WORKSPACE_TMPDIR_NAME,
        "handler_sandbox_workspace_tmpdir",
        "temp dir",
    )
}

fn workspace_local_toolchain_cache(policy: &CapabilityPolicy) -> Option<PathBuf> {
    create_self_ignored_dir(
        policy,
        WORKSPACE_TOOLCHAIN_CACHE_NAME,
        "handler_sandbox_workspace_toolchain_cache",
        "toolchain cache",
    )
}

/// Preserve a caller-selected toolchain path only when it resolves inside a
/// writable workspace root. This lets an outer harness prewarm one cache for
/// the whole run without allowing an inherited global path to widen the jail.
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
        .iter()
        .any(|root| path_is_within(&resolved, root))
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
        // and never carries a token into the workspace.
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
        // workspace stand-in that carries `config.json` but never `auth.json`
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
fn gh_config_source(operator_home: &Path) -> PathBuf {
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
/// left from an earlier spawn is removed.
fn seed_gh_config_dir(source: &Path, cache: &Path, relative: &str) -> PathBuf {
    let raw = std::fs::read_to_string(source.join("config.yml"))
        .ok()
        .filter(|raw| !raw.contains("oauth_token"));
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

/// Workspace-local Composer home: copy non-credential files, never `auth.json`.
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
/// temporary storage inside the workspace.
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
    if let Some(tmpdir) = planned_workspace_dir(policy, WORKSPACE_TMPDIR_NAME) {
        push_tmpdir(&mut env, &tmpdir);
    }
    if let Some(root) = planned_workspace_dir(policy, WORKSPACE_TOOLCHAIN_CACHE_NAME) {
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

/// Workspace-local temp and mutable toolchain-state defaults for the active
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

    #[cfg(unix)]
    #[test]
    fn runtime_ignore_uses_the_opened_directory_after_a_workspace_path_swap() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("marker"), "host-unchanged").unwrap();
        let (parent, leaf) =
            super::super::ensure_parent_dirs_scoped(&super::super::ScopedMutationTarget {
                root: workspace.path().to_path_buf(),
                relative: PathBuf::from(".harn-tmp/.gitignore"),
            })
            .unwrap();
        let original = workspace.path().join("original-temp");
        std::fs::rename(workspace.path().join(".harn-tmp"), &original).unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join(".harn-tmp")).unwrap();

        write_scoped_self_ignore(&parent, &leaf).unwrap();
        assert_eq!(
            std::fs::read_to_string(original.join(".gitignore")).unwrap(),
            crate::runtime_paths::SELF_IGNORE_CONTENTS
        );
        assert!(!outside.path().join(".gitignore").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("marker")).unwrap(),
            "host-unchanged"
        );
    }

    #[test]
    fn process_env_is_workspace_local_and_self_ignored() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = policy(workspace.path());
        let mut env = Vec::new();
        inject_workspace_process_env(&mut env, &policy);
        let env: BTreeMap<_, _> = env.into_iter().collect();

        let workspace = workspace.path().canonicalize().unwrap();
        let cache = workspace.join(WORKSPACE_TOOLCHAIN_CACHE_NAME);
        let tmp = workspace.join(WORKSPACE_TMPDIR_NAME);
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
        for root in [cache, tmp] {
            let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(ignore.lines().any(|line| line.trim() == "*"));
        }
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
        let cache = dir.path().join("cache");
        let stand_in = cache.join("npm/npmrc");
        assert_eq!(seed_sanitized_npmrc(&source, &cache, "npm/npmrc"), stand_in);
        assert!(
            !stand_in.exists(),
            "no source, no file: npm treats absent as empty"
        );
        std::fs::write(&source, raw).unwrap();
        seed_sanitized_npmrc(&source, &cache, "npm/npmrc");
        assert_eq!(std::fs::read_to_string(&stand_in).unwrap(), sanitized);
        let written = std::fs::metadata(&stand_in).unwrap().modified().unwrap();
        seed_sanitized_npmrc(&source, &cache, "npm/npmrc");
        assert_eq!(
            std::fs::metadata(&stand_in).unwrap().modified().unwrap(),
            written,
            "an unchanged stand-in is not rewritten"
        );
        std::fs::remove_file(&source).unwrap();
        seed_sanitized_npmrc(&source, &cache, "npm/npmrc");
        assert!(!stand_in.exists(), "a stand-in never outlives its source");
    }

    /// Confined children can write the cache, and the seed runs unconfined. A
    /// link a child plants at the stand-in's name must be replaced, never
    /// written through, and must not leak the target's content.
    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_never_turns_a_stand_in_seed_into_a_host_write() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("hosts.yml");
        std::fs::write(&victim, "github.com:\n  oauth_token: gho_victim\n").unwrap();
        let source = dir.path().join("gh");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("config.yml"), "git_protocol: ssh\n").unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(cache.join("gh")).unwrap();
        std::os::unix::fs::symlink(&victim, cache.join("gh/config.yml")).unwrap();

        seed_gh_config_dir(&source, &cache, "gh");

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "github.com:\n  oauth_token: gho_victim\n",
            "the link's target must be untouched"
        );
        let planted = std::fs::symlink_metadata(cache.join("gh/config.yml")).unwrap();
        assert!(planted.file_type().is_file(), "the link is replaced");
        assert_eq!(
            std::fs::read_to_string(cache.join("gh/config.yml")).unwrap(),
            "git_protocol: ssh\n"
        );

        // A linked directory component is refused rather than followed.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::remove_dir_all(cache.join("gh")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, cache.join("gh")).unwrap();
        seed_gh_config_dir(&source, &cache, "gh");
        assert!(!elsewhere.join("config.yml").exists());
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
        let cache = workspace
            .path()
            .canonicalize()
            .unwrap()
            .join(WORKSPACE_TOOLCHAIN_CACHE_NAME);
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
        let gh_config = PathBuf::from(env.get("GH_CONFIG_DIR").unwrap());
        assert!(
            gh_config.starts_with(&cache),
            "GH_CONFIG_DIR must be the workspace stand-in: {gh_config:?}"
        );
    }

    #[test]
    fn the_gh_stand_in_carries_config_but_never_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("gh");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("config.yml"), "git_protocol: ssh\n").unwrap();
        std::fs::write(
            source.join("hosts.yml"),
            "github.com:\n  oauth_token: gho_x\n",
        )
        .unwrap();
        let cache = dir.path().join("cache");
        let stand_in = seed_gh_config_dir(&source, &cache, "gh");
        assert_eq!(
            std::fs::read_to_string(stand_in.join("config.yml")).unwrap(),
            "git_protocol: ssh\n"
        );
        assert!(!stand_in.join("hosts.yml").exists());

        // A pre-1.0 config.yml that still holds a token is not copied, and the
        // copy the earlier seed left is removed rather than kept.
        let legacy = tempfile::tempdir().unwrap();
        std::fs::write(
            legacy.path().join("config.yml"),
            "hosts:\n  github.com:\n    oauth_token: gho_x\n",
        )
        .unwrap();
        let stand_in = seed_gh_config_dir(legacy.path(), &cache, "gh");
        assert!(stand_in.is_dir());
        assert!(!stand_in.join("config.yml").exists());
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
