//! Real Git subprocesses exercise checkout teardown before cache publication.

use crate::package::registry::git_source::HardenedGitEnv;
use crate::package::test_support::{create_git_package_repo, run_git, TestWorkspace};
use crate::package::*;
use std::os::unix::fs::PermissionsExt;

fn maintenance_trigger_repo() -> (tempfile::TempDir, PathBuf) {
    let (tmp, repo, _) = create_git_package_repo();
    // Git 2.55 samples shard 17: two loose objects estimate 512, exceeding
    // its default geometric-repack threshold even in this tiny repository.
    for (name, content) in [("sample-a", "32"), ("sample-b", "119")] {
        fs::write(repo.join(name), content).unwrap();
        assert!(run_git(&repo, &["hash-object", name]).starts_with("17"));
    }
    run_git(&repo, &["add", "."]);
    run_git(
        &repo,
        &[
            "-c",
            "maintenance.auto=false",
            "commit",
            "-m",
            "sampled objects",
        ],
    );
    (tmp, repo)
}

fn maintenance_started(trace: &Path) -> bool {
    fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .any(|event| {
            event["event"] == "child_start"
                && event["argv"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|arg| arg == "maintenance" || arg == "gc"))
        })
}

fn assert_maintenance_control(root: &Path, url: &str, commit: &str) {
    let dest = root.join("control");
    fs::create_dir(&dest).unwrap();
    assert!(git_output(["init", "--quiet"], Cwd::In(&dest))
        .unwrap()
        .status
        .success());
    let trace = root.join("control.json");
    let env = HardenedGitEnv::new().unwrap();
    let mut command = process::Command::new("git");
    env.apply_to(&mut command, Cwd::In(&dest));
    // Keep the positive control synchronous, including on Git 2.55.
    let output = command
        .env("GIT_TRACE2_EVENT", &trace)
        .args([
            "-c",
            "maintenance.autoDetach=false",
            "-c",
            "gc.autoDetach=false",
        ])
        .args(["fetch", "--depth", "1", url, commit])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        maintenance_started(&trace),
        "control must observe real maintenance"
    );
}

#[test]
fn git_dependency_checkout_lifecycle() {
    const CHILD_ROOT: &str = "PACKAGE_GIT_LIFECYCLE_TEST_CHILD_ROOT";
    const CHILD_MODE: &str = "PACKAGE_GIT_LIFECYCLE_TEST_CHILD_MODE";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let workspace = TestWorkspace::new(&root);
        let mode = std::env::var(CHILD_MODE).unwrap();
        let manifest = read_manifest_from_path(&root.join(MANIFEST)).unwrap();
        let dependency = &manifest.dependencies["acme-lib"];
        let git = dependency.git_url().unwrap();
        let commit = dependency.rev().unwrap();
        let source = format!("git+{git}");
        let cache = git_cache_dir_in(workspace.env(), &source, commit).unwrap();
        assert!(!cache.exists(), "the install must start with a cold cache");
        if mode == "hash-failure" {
            let error = ensure_git_cache_populated_in(
                workspace.env(),
                git,
                &source,
                commit,
                Some("invalid-hash"),
                false,
                false,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("content hash mismatch"),
                "{error}"
            );
            assert!(!cache.exists());
        } else {
            let result = install_packages_in(workspace.env(), false, None, false);
            if mode == "cleanup-failure" {
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains("failed to remove") && error.contains(".git"),
                    "{error}"
                );
                assert!(
                    !cache.exists(),
                    "failed teardown must refuse cache publication"
                );
                assert!(!root.join(LOCK_FILE).exists());
                assert!(
                    harn_modules::package_snapshot::PackageSnapshot::acquire(&root)
                        .unwrap()
                        .is_none()
                );
            } else {
                result.unwrap();
                let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
                let entry = lock.find("acme-lib").unwrap();
                assert_eq!(entry.commit.as_deref(), Some(commit));
                assert_eq!(
                    entry.content_hash.as_deref(),
                    Some(compute_content_hash(&cache).unwrap().as_str())
                );
                assert!(!cache.join(".git").exists());
                assert_eq!(
                    fs::read_to_string(cache.join("lib.harn")).unwrap(),
                    "pub fn value() -> string { return \"v1\" }\n"
                );
                assert!(
                    harn_modules::package_snapshot::PackageSnapshot::acquire(&root)
                        .unwrap()
                        .is_some()
                );
            }
        }
        assert!(!maintenance_started(&root.join("trace.json")));
        let trace = fs::read_to_string(root.join("trace.json")).unwrap();
        let cloned = trace.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["event"] == "cmd_name" && event["name"] == "clone"
        });
        assert_eq!(
            cloned,
            mode == "fallback",
            "the requested checkout path must run"
        );
        return;
    }

    let (_repo_tmp, repo) = maintenance_trigger_repo();
    let commit = run_git(&repo, &["rev-parse", "HEAD"]);
    let git = normalize_git_url(repo.to_str().unwrap()).unwrap();
    let real_git = which::which("git").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    assert_maintenance_control(tmp.path(), &git, &commit);
    for mode in ["shallow", "fallback", "cleanup-failure", "hash-failure"] {
        let root = tmp.path().join(mode);
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(MANIFEST), format!("[package]\nname = \"workspace\"\nversion = \"0.1.0\"\n[dependencies]\nacme-lib = {{ git = \"{git}\", rev = \"{commit}\" }}\n")).unwrap();
        let trace = root.join("trace.json");
        let before = if mode == "fallback" {
            "case \" $* \" in *' fetch '*) exit 1 ;; esac\n"
        } else {
            ""
        };
        let after = if mode == "cleanup-failure" {
            // A regular file at .git deterministically makes remove_dir_all
            // fail, even on hosts where root can bypass directory permissions.
            "case \" $* \" in *' checkout '*) rm -rf .git; printf blocked > .git ;; esac\n"
        } else {
            ""
        };
        let wrapper = bin.join("git");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nset -eu\n{before}GIT_TRACE2_EVENT={} {} \"$@\"\n{after}",
                shell_words::quote(trace.to_str().unwrap()),
                shell_words::quote(real_git.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "package::registry::tests::git_lifecycle::git_dependency_checkout_lifecycle",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .env(CHILD_MODE, mode)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child test did not run: {output:?}"
        );
    }
}
