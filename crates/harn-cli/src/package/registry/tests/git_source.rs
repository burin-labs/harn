//! Git spec handling and the hardened git environment.

use super::super::git_source::pick_ls_remote_commit;

#[cfg(unix)]
use super::super::git_source::HardenedGitEnv;
use crate::package::test_support::{create_git_package_repo, run_git};
use crate::package::*;

#[test]
fn pick_ls_remote_commit_prefers_peeled_tag_over_tag_object() {
    // Real-world example from notion-sdk-harn v0.1.0: the tag is
    // annotated, so ls-remote returns both the tag-object SHA and the
    // commit it points at.
    let output = "\
963b6e8acfdf030a9b922bc5a73e010758ff47da\trefs/tags/v0.1.0\n\
bad580c5fbe8ede612b2748ad98606642ce2fc02\trefs/tags/v0.1.0^{}\n";
    assert_eq!(
        pick_ls_remote_commit(output),
        Some("bad580c5fbe8ede612b2748ad98606642ce2fc02"),
    );
}

#[test]
fn pick_ls_remote_commit_falls_back_to_first_match_for_lightweight_tags() {
    let output = "\
abc123abc123abc123abc123abc123abc1234567\trefs/tags/v0.0.1\n";
    assert_eq!(
        pick_ls_remote_commit(output),
        Some("abc123abc123abc123abc123abc123abc1234567"),
    );
}

#[test]
fn pick_ls_remote_commit_returns_none_on_empty_output() {
    assert_eq!(pick_ls_remote_commit(""), None);
}

#[cfg(unix)]
#[test]
fn hardened_git_env_scrubs_ambient_git_credentials_and_config() {
    let git_env = HardenedGitEnv::new().unwrap();
    let mut command = process::Command::new("/usr/bin/env");
    command
        .env("HOME", "/sensitive/home")
        .env("XDG_CONFIG_HOME", "/sensitive/config")
        .env("GIT_ASKPASS", "/sensitive/askpass")
        .env("GIT_SSH_COMMAND", "ssh -i /sensitive/key")
        .env("SSH_AUTH_SOCK", "/sensitive/agent.sock")
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            "http.https://attacker.example/.extraheader",
        )
        .env("GIT_CONFIG_VALUE_0", "Authorization: bearer secret");
    git_env.apply_to(&mut command, Cwd::Detached);

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "env probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let vars: std::collections::BTreeMap<_, _> = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();

    assert_eq!(Path::new(&vars["HOME"]), git_env.home);
    assert_eq!(Path::new(&vars["XDG_CONFIG_HOME"]), git_env.config_home);
    assert_eq!(Path::new(&vars["GIT_CONFIG_GLOBAL"]), git_env.global_config);
    assert_eq!(Path::new(&vars["GIT_CONFIG_SYSTEM"]), git_env.system_config);
    assert_eq!(vars["GIT_CONFIG_NOSYSTEM"], "1");
    assert_eq!(vars["GIT_TERMINAL_PROMPT"], "0");
    assert!(!vars.contains_key("GIT_ASKPASS"));
    assert!(!vars.contains_key("GIT_SSH_COMMAND"));
    assert!(!vars.contains_key("SSH_AUTH_SOCK"));
    assert!(!vars.contains_key("GIT_CONFIG_COUNT"));
    assert!(!vars.contains_key("GIT_CONFIG_KEY_0"));
    assert!(!vars.contains_key("GIT_CONFIG_VALUE_0"));
}

#[test]
fn clone_normalizes_git_symlink_materialization() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    fs::write(repo.join("alias.harn"), "lib.harn").unwrap();
    let blob = run_git(&repo, &["hash-object", "-w", "alias.harn"]);
    let cache_info = format!("120000,{blob},alias.harn");
    run_git(
        &repo,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );
    run_git(&repo, &["commit", "-m", "add symlink"]);
    let commit = run_git(&repo, &["rev-parse", "HEAD"]);
    let clone_tmp = tempfile::tempdir().unwrap();
    let dest = clone_tmp.path().join("clone");
    let url = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();

    clone_git_commit_to(&url, &commit, &dest).unwrap();

    assert!(fs::symlink_metadata(dest.join("alias.harn"))
        .unwrap()
        .file_type()
        .is_file());
    assert_eq!(
        fs::read_to_string(dest.join("alias.harn")).unwrap(),
        "lib.harn"
    );
}
