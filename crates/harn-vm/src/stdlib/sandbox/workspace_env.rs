use std::path::PathBuf;

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
    let mut env = vec![
        ("HOME".to_string(), root.display().to_string()),
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
            "PYTHONUSERBASE".to_string(),
            root.join("python-user").display().to_string(),
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

    // HOME is intentionally relocated, but immutable user toolchains and
    // package-manager configuration still resolve through their existing
    // process-only preset roots. Cargo's mutable registry/git cache already
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
            ("NPM_CONFIG_USERCONFIG", home.join(".npmrc")),
            ("PIP_CONFIG_FILE", home.join(".config/pip/pip.conf")),
        ] {
            if candidate.is_file() {
                env.push((key.to_string(), candidate.display().to_string()));
            }
        }
    }
    env
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
        for key in [
            "HOME",
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
            "PYTHONUSERBASE",
        ] {
            let value = PathBuf::from(env.get(key).unwrap());
            assert!(value.starts_with(&cache), "{key} escaped cache: {value:?}");
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
}
