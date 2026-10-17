use super::*;
use std::process::Command;

struct GitFixture {
    temp: tempfile::TempDir,
    repository: PathBuf,
    lane: PathBuf,
}

impl GitFixture {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CONFIG_NOSYSTEM")
            .env(
                "GIT_CONFIG_SYSTEM",
                self.temp.path().join("empty.gitconfig"),
            )
            .env(
                "GIT_CONFIG_GLOBAL",
                self.temp.path().join("empty.gitconfig"),
            );
        command
    }

    fn bash_command(&self) -> Command {
        #[cfg(windows)]
        let bash = {
            // PATH may select Windows' WSL shim. Use the native shell shipped
            // with the Git installation that owns this fixture's authorities.
            let exec_path = self.git(&self.repository, &["--exec-path"]);
            Path::new(&exec_path)
                .ancestors()
                .flat_map(|root| [root.join("bin/bash.exe"), root.join("usr/bin/bash.exe")])
                .find(|candidate| candidate.is_file())
                .expect("the fixture's Git installation must provide native Git Bash")
        };
        #[cfg(not(windows))]
        let bash = PathBuf::from("bash");
        self.command(bash)
    }

    fn git(&self, directory: &Path, args: &[&str]) -> String {
        let output = self
            .command("git")
            .arg("-C")
            .arg(directory)
            .args([
                "-c",
                "user.name=Freshness fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("empty.gitconfig"), []).unwrap();
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let fixture = Self {
            lane: temp.path().join("lane"),
            repository,
            temp,
        };
        fixture.git(&fixture.repository, &["init", "--initial-branch=main"]);
        fs::write(fixture.repository.join("input.harn"), b"source").unwrap();
        fixture.git(&fixture.repository, &["add", "input.harn"]);
        fixture.git(&fixture.repository, &["commit", "-m", "baseline"]);
        fixture.git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "lane",
                fixture.lane.to_str().unwrap(),
            ],
        );
        fixture.git(&fixture.repository, &["pack-refs", "--all"]);
        fixture
    }
}

#[test]
fn linked_worktree_freshness_tracks_selected_revision_and_source() {
    let fixture = GitFixture::new();
    let baseline = fixture.git(&fixture.lane, &["rev-parse", "HEAD"]);
    assert_eq!(baseline.len(), 40);
    let authorities = fixture.temp.path().join("authorities");
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = fixture
        .bash_command()
        .arg(project.join("scripts/tests/fixtures/freshness-authorities.sh"))
        .arg(project.join("scripts/lib/harn_bin_freshness.sh"))
        .arg(&fixture.lane)
        .arg(&authorities)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "authority writer exited {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let dep_info = fixture.temp.path().join("harn.d");
    fs::write(&dep_info, b"target:\n").unwrap();
    let manifest = fixture.temp.path().join("manifest");
    write_manifest(
        &manifest,
        &fixture.lane,
        &BTreeSet::from([PathBuf::from("input.harn")]),
        &dep_info,
        &[],
        &authorities,
    )
    .unwrap();
    assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

    // The shared ref store really changes while this checkout's commit does not.
    let packed_refs = PathBuf::from(fixture.git(
        &fixture.repository,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "packed-refs",
        ],
    ));
    let packed_before = fs::read(&packed_refs).unwrap();
    fixture.git(
        &fixture.repository,
        &["commit", "--allow-empty", "-m", "unrelated checkout"],
    );
    let other = fixture.git(&fixture.repository, &["rev-parse", "HEAD"]);
    assert_ne!(baseline, other);
    fixture.git(&fixture.repository, &["pack-refs", "--all"]);
    assert_ne!(packed_before, fs::read(&packed_refs).unwrap());
    assert_eq!(fixture.git(&fixture.lane, &["rev-parse", "HEAD"]), baseline);
    assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

    fixture.git(&fixture.lane, &["update-ref", "refs/heads/lane", &other]);
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("content changed"));
    fixture.git(&fixture.lane, &["update-ref", "refs/heads/lane", &baseline]);
    fixture.git(&fixture.lane, &["checkout", "--detach", &baseline]);
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("content changed"));
    fixture.git(&fixture.lane, &["checkout", "lane"]);
    assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

    let input = fixture.lane.join("input.harn");
    fs::write(&input, b"mutated").unwrap();
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("content changed"));
    fs::write(input, b"source").unwrap();
    assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

    let config = PathBuf::from(fixture.git(
        &fixture.lane,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "config",
        ],
    ));
    let config_before = fs::read(&config).unwrap();
    fixture.git(
        &fixture.lane,
        &["config", "core.excludesfile", "/unrelated-inventory-ignore"],
    );
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("content changed"));
    fs::write(config, config_before).unwrap();
    assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

    let head = PathBuf::from(fixture.git(
        &fixture.lane,
        &["rev-parse", "--path-format=absolute", "--git-path", "HEAD"],
    ));
    fs::write(&head, b"invalid HEAD\n").unwrap();
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("cannot resolve"));
    fs::remove_file(head).unwrap();
    assert!(verify_manifest(&manifest)
        .unwrap_err()
        .contains("type changed"));
}
