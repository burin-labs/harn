//! A real confined Git command must honor paths from the user's global config
//! without giving the child write authority over those paths.

#![cfg(target_os = "macos")]

use crate::test_util::process::harn_e2e_command;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_CONFIG_GLOBAL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run setup git");
    assert!(
        output.status.success(),
        "setup git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn global_config_includes_external_hooks_for_commit_but_never_grants_write() {
    let temp = tempfile::tempdir().expect("isolated HOME and repository");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);

    // Outside the default temp grants and outside the repo write jail. Keep
    // this under the owning checkout so TempDir removes it even on a failure.
    let external = tempfile::Builder::new()
        .prefix("git-config-roots-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("external Git config root");
    let hooks = external.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("pre-commit"),
        "#!/bin/sh\nprintf 'hook-fired\\n' > .git-hook-fired\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        hooks.join("pre-commit"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let canary = hooks.join("canary");
    std::fs::write(&canary, "original\n").unwrap();
    let included = external.path().join("included.gitconfig");
    std::fs::write(
        &included,
        format!("[core]\n\thooksPath = {}\n", hooks.display()),
    )
    .unwrap();
    let selected_global = home.join("selected.gitconfig");
    let git_dir = repo.join(".git").canonicalize().unwrap();
    let global_config = format!(
        "[includeIf \"gitdir:{}\"]\n\tpath = {}\n",
        git_dir.display(),
        included.display()
    );
    std::fs::write(home.join(".gitconfig"), &global_config).unwrap();
    std::fs::write(&selected_global, &global_config).unwrap();

    git(&repo, &["config", "user.name", "Harn Git Probe"]);
    git(&repo, &["config", "user.email", "probe@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README"), "probe\n").unwrap();
    git(&repo, &["add", "README"]);

    let canary_arg = serde_json::to_string(&canary.display().to_string()).unwrap();
    std::fs::write(
        repo.join("probe.harn"),
        format!(
            concat!(
                "fn main(harness: Harness) {{\n",
                "  const commit = harness.process.run({{program: \"git\", args: [\"commit\", \"-q\", \"-m\", \"probe\"], cwd: \".\"}})\n",
                "  harness.stdio.println(\"commit=\" + to_string(commit.success))\n",
                "  const read = harness.process.run({{program: \"cat\", args: [{}], cwd: \".\"}})\n",
                "  harness.stdio.println(\"read=\" + to_string(read.success))\n",
                "  const write = harness.process.run({{program: \"sh\", args: [\"-c\", \"printf changed > \\\"$1\\\"\", \"sh\", {}], cwd: \".\"}})\n",
                "  harness.stdio.println(\"write=\" + to_string(write.success))\n",
                "  harness.stdio.println(write.stderr)\n",
                "}}\n"
            ),
            canary_arg, canary_arg
        ),
    )
    .unwrap();

    let run = |selected: Option<&Path>, system: Option<&Path>| {
        let mut command = harn_e2e_command();
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .args(["run", "--standalone", "probe.harn"]);
        if let Some(selected) = selected {
            command.env("GIT_CONFIG_GLOBAL", selected);
        } else {
            command.env_remove("GIT_CONFIG_GLOBAL");
        }
        if let Some(system) = system {
            command
                .env("GIT_CONFIG_SYSTEM", system)
                .env_remove("GIT_CONFIG_NOSYSTEM");
        } else {
            command
                .env_remove("GIT_CONFIG_SYSTEM")
                .env("GIT_CONFIG_NOSYSTEM", "1");
        }
        let output = command.output().expect("run canonical Harn CLI");
        assert!(
            output.status.success(),
            "Harn run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("commit=true"), "commit failed: {stdout}");
        assert!(stdout.contains("read=true"), "read did not fire: {stdout}");
        assert!(stdout.contains("write=false"), "write escaped: {stdout}");
        assert!(
            stdout.contains("Operation not permitted") || stdout.contains("Permission denied"),
            "write failure did not name a sandbox refusal: {stdout}"
        );
        assert_eq!(std::fs::read_to_string(&canary).unwrap(), "original\n");
        assert_eq!(
            std::fs::read_to_string(repo.join(".git-hook-fired")).unwrap(),
            "hook-fired\n"
        );
    };

    // First, the ordinary ~/.gitconfig path reproduces the original failure.
    run(None, None);
    std::fs::remove_file(repo.join(".git-hook-fired")).unwrap();
    std::fs::write(repo.join("SECOND"), "second\n").unwrap();
    git(&repo, &["add", "SECOND"]);
    // Then a selected global config proves the child's config and the host's
    // root discovery agree even while ~/.gitconfig names different settings.
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = Default\n").unwrap();
    run(Some(&selected_global), None);

    std::fs::remove_file(repo.join(".git-hook-fired")).unwrap();
    std::fs::write(repo.join("THIRD"), "third\n").unwrap();
    git(&repo, &["add", "THIRD"]);
    let selected_system = external.path().join("system.gitconfig");
    std::fs::write(
        &selected_system,
        format!("[include]\n\tpath = {}\n", included.display()),
    )
    .unwrap();
    // The system config is also outside HOME and the workspace. The host
    // resolves its include chain without granting the repo local config.
    run(None, Some(&selected_system));
    git(&repo, &["log", "-1", "--format=%s"]);
}
