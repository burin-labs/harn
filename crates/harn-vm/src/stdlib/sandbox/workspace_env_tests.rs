//! Sandbox temp dir, toolchain cache and process environment behavior.

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
/// the next child, or Harn's own writes, a directory somewhere else. It gets
/// the workspace's own temp dir instead.
#[cfg(unix)]
#[test]
fn a_symlinked_runtime_child_is_refused_not_followed() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = policy(workspace.path());
    let tmp = workspace_local_tmpdir(&policy).expect("ordinary temp dir");
    std::fs::remove_dir(&tmp).unwrap();
    std::os::unix::fs::symlink(outside.path(), &tmp).unwrap();

    let redirected = workspace_local_tmpdir(&policy).expect("the workspace fallback");
    std::fs::remove_dir_all(tmp.parent().unwrap()).unwrap();
    assert!(redirected.starts_with(workspace.path().canonicalize().unwrap()));
    assert!(redirected.ends_with(WORKSPACE_TMPDIR_NAME));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

/// A fallback dir swapped for a symlink is refused too. With both locations
/// refused there is no temp dir to hand out.
#[cfg(unix)]
#[test]
fn a_symlinked_workspace_fallback_is_refused_not_followed() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = policy(workspace.path());
    let tmp = workspace_local_tmpdir(&policy).expect("ordinary temp dir");
    std::fs::remove_dir(&tmp).unwrap();
    std::os::unix::fs::symlink(outside.path(), &tmp).unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join(WORKSPACE_TMPDIR_NAME))
        .unwrap();

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

    let base_fd = super::super::open_dir_absolute(&base.path().canonicalize().unwrap()).unwrap();
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
    let env: BTreeMap<_, _> =
        workspace_toolchain_env_with_package_cache(&policy(workspace.path()), Some(cache.clone()))
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
