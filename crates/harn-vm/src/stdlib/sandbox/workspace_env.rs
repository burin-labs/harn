use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

use super::{
    base_workspace_roots, normalize_for_policy, normalized_workspace_roots, path_is_within,
    warn_once,
};

pub(crate) const WORKSPACE_TMPDIR_NAME: &str = ".harn-tmp";
pub(crate) const TMPDIR_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];
pub(crate) const WORKSPACE_TOOLCHAIN_CACHE_NAME: &str = ".harn-toolchain-cache";

fn create_self_ignored_dir(
    policy: &CapabilityPolicy,
    name: &str,
    warning_key: &str,
    label: &str,
) -> Option<PathBuf> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return None;
    }
    let root = normalized_workspace_roots(policy).into_iter().next()?;
    let path = root.join(name);
    if let Err(error) = std::fs::create_dir_all(&path) {
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
    let ignore = path.join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(
            &ignore,
            "# Created by the Harn sandbox; safe to delete.\n*\n",
        );
    }
    Some(path)
}

pub(crate) fn workspace_local_tmpdir(policy: &CapabilityPolicy) -> Option<PathBuf> {
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

fn workspace_toolchain_env_with_package_cache(
    policy: &CapabilityPolicy,
    package_cache: Option<PathBuf>,
) -> Vec<(String, String)> {
    let Some(root) = workspace_local_toolchain_cache(policy) else {
        return Vec::new();
    };
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
            root.join("xdg-cache").display().to_string(),
        ),
        ("GOCACHE".to_string(), path("GOCACHE", "go-build")),
        ("GOMODCACHE".to_string(), path("GOMODCACHE", "go-mod")),
        ("GOPATH".to_string(), path("GOPATH", "go")),
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
        ("PNPM_HOME".to_string(), path("PNPM_HOME", "pnpm/home")),
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
    if let Some(home) = crate::user_dirs::home_dir().filter(|home| home.is_absolute()) {
        env.push((
            "CARGO_HOME".to_string(),
            home.join(".cargo").display().to_string(),
        ));
        env.push((
            "RUSTUP_HOME".to_string(),
            home.join(".rustup").display().to_string(),
        ));
        for (key, candidate) in [
            ("GIT_CONFIG_GLOBAL", home.join(".gitconfig")),
            ("PIP_CONFIG_FILE", home.join(".config/pip/pip.conf")),
        ] {
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
            seed_sanitized_npmrc(&home.join(".npmrc"), &root.join("npm/npmrc"))
                .display()
                .to_string(),
        ));
        // Composer treats an unreadable `auth.json` as fatal, and that file
        // is on the credential denylist. Pointing COMPOSER_HOME at a
        // workspace stand-in that carries `config.json` but never `auth.json`
        // keeps repositories and preferred-install settings and drops tokens.
        env.push((
            "COMPOSER_HOME".to_string(),
            seed_composer_home(&home, &root.join("composer/home"))
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
/// Workspace-local Composer home: copy non-credential files, never `auth.json`.
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
    let Some(tmpdir) = workspace_local_tmpdir(policy) else {
        return;
    };
    let tmpdir = tmpdir.display().to_string();
    for key in TMPDIR_ENV_KEYS {
        if !env.iter().any(|(existing, _)| existing == key) {
            env.push((key.to_string(), tmpdir.clone()));
        }
    }
}

pub(crate) fn inject_workspace_process_env(
    env: &mut Vec<(String, String)>,
    policy: &CapabilityPolicy,
) {
    inject_workspace_tmpdir(env, policy);
    for (key, value) in workspace_toolchain_env(policy) {
        if !env.iter().any(|(existing, _)| existing == &key) {
            env.push((key, value));
        }
    }
    inject_jvm_loopback_env(env, policy);
}

/// The JVM option that makes a loopback-only grant hold for Java children.
const JVM_PREFER_IPV4_STACK: &str = "-Djava.net.preferIPv4Stack=true";

/// Make `allow_tcp_loopback` mean what it says for JVM children on macOS.
///
/// The seatbelt matches loopback endpoints by the literal `localhost` filter,
/// and that filter does not match an IPv4-mapped IPv6 address. A dual-stack
/// JVM — the default — binds and connects `127.0.0.1` as `::ffff:127.0.0.1`,
/// so a Gradle daemon, an sbt server, or any Java test server on a granted
/// loopback dies with a bare `SocketException: Operation not permitted`
/// while a Python or Node server beside it works. The seatbelt grammar
/// offers no spelling for the mapped form (measured: numeric hosts are
/// rejected at profile parse), so the fix is on the JVM side: prefer the
/// IPv4 stack, which makes the JVM emit the addresses the filter matches.
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
        JVM_PREFER_IPV4_STACK.to_string()
    } else {
        format!("{} {JVM_PREFER_IPV4_STACK}", existing.trim_end())
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
            "CARGO_TARGET_DIR",
            "PIP_CACHE_DIR",
            "UV_CACHE_DIR",
            "NPM_CONFIG_CACHE",
            "YARN_CACHE_FOLDER",
            "PNPM_HOME",
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
        for key in ["HOME", "PYTHONUSERBASE"] {
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
    }

    #[test]
    fn a_loopback_grant_pins_the_jvm_to_the_ipv4_stack_on_macos() {
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
            assert_eq!(value, format!("-Xmx1g {JVM_PREFER_IPV4_STACK}"));
        } else {
            assert_eq!(value, "-Xmx1g", "only the macOS seatbelt needs the pin");
        }

        // An operator who already chose a stack keeps their choice.
        let mut env = vec![(
            "JAVA_TOOL_OPTIONS".to_string(),
            "-Djava.net.preferIPv6Addresses=true".to_string(),
        )];
        inject_workspace_process_env(&mut env, &policy);
        assert_eq!(
            env.iter()
                .filter(|(key, _)| key == "JAVA_TOOL_OPTIONS")
                .count(),
            1
        );
        assert_eq!(env[0].1, "-Djava.net.preferIPv6Addresses=true");
    }
}
