use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use super::build_revision;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Harn fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(root: &Path) -> String {
    git(root, &["add", "--all"]);
    git(
        root,
        &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
    );
    git(root, &["rev-parse", "HEAD"])
}

fn dependency_version(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lock: toml::Value = toml::from_str(&fs::read_to_string(root).unwrap()).unwrap();
    let versions: Vec<_> = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| package["name"].as_str() == Some(name))
        .map(|package| package["version"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(versions.len(), 1, "one resolved {name} build dependency");
    versions[0].clone()
}

fn source_crate(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname='provenance-fixture'\nversion='0.0.0'\nedition='2021'\n\
         [build-dependencies]\nserde={{version='={}',features=['derive']}}\nserde_json='={}'\n",
            dependency_version("serde"),
            dependency_version("serde_json"),
        ),
    )
    .unwrap();
    let owner = Path::new(env!("CARGO_MANIFEST_DIR")).join("build_support/build_revision.rs");
    // The Rust path attribute needs a quoted, escaped literal rather than Display.
    #[allow(clippy::unnecessary_debug_formatting)]
    let build_script = format!(
        "#[path = {owner:?}]\n#[allow(dead_code)]\nmod owner;\nfn main() {{ owner::emit(); }}\n"
    );
    fs::write(root.join("build.rs"), build_script).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub const REVISION: &str = env!(\"HARN_BUILD_REVISION\");\n",
    )
    .unwrap();
}

fn consumer_manifest(root: &Path, dependency: &Path) {
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname='consumer-fixture'\nversion='0.0.0'\nedition='2021'\n\
         [workspace]\n[dependencies]\nprovenance-fixture={{path={:?}}}\n",
            dependency.to_str().unwrap(),
        ),
    )
    .unwrap();
}

fn cargo_run(consumer: &Path, target: &Path, override_revision: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(consumer)
        .args(["run", "--offline"])
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_JOBS", "2")
        .env("GIT_DIR", consumer.join(".git"))
        .env("GIT_WORK_TREE", consumer)
        .env_remove("HARN_BUILD_REVISION")
        .env_remove("HARN_BUILD_FRESHNESS_ID");
    if let Some(revision) = override_revision {
        command.env("HARN_BUILD_REVISION", revision);
    }
    let output = command.output().expect("run unconfigured downstream Cargo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
pub(super) fn unconfigured_cargo_build_attests_its_dependency_and_tracks_git_head() {
    let temp = tempfile::tempdir().unwrap();
    let source_repo = temp.path().join("source");
    let source = source_repo.join("crates/harn-cli");
    let consumer = temp.path().join("consumer");
    let target = temp.path().join("target");
    source_crate(&source);
    git(&source_repo, &["init", "--quiet"]);
    let source_head = commit(&source_repo);
    fs::create_dir_all(consumer.join("src")).unwrap();
    consumer_manifest(&consumer, &source);
    fs::write(
        consumer.join("src/main.rs"),
        "fn main() { println!(\"{}\", provenance_fixture::REVISION); }\n",
    )
    .unwrap();
    git(&consumer, &["init", "--quiet"]);
    let consumer_head = commit(&consumer);
    assert_ne!(source_head, consumer_head);
    let output = cargo_run(&consumer, &target, None);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        source_head
    );

    git(&source_repo, &["pack-refs", "--all"]);
    let next_head = commit(&source_repo);
    assert_ne!(next_head, source_head);
    let output = cargo_run(&consumer, &target, None);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        next_head,
        "a cached Cargo build must notice Git HEAD advancing without a source edit"
    );
    let explicit = "d".repeat(64);
    let output = cargo_run(&consumer, &target, Some(&explicit));
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), explicit);

    let archive = consumer.join("untracked-archive");
    source_crate(&archive);
    consumer_manifest(&consumer, &archive);
    let output = cargo_run(&consumer, &target, None);
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Harn source revision unavailable"));
}

#[test]
pub(super) fn packaged_source_identity_does_not_inherit_an_unrelated_git_checkout() {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "--quiet"]);
    fs::write(temp.path().join("README"), "outer checkout").unwrap();
    let outer_head = commit(temp.path());
    let package = temp.path().join("untracked-package");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname='harn-cli'\nversion='0.0.0'\n",
    )
    .unwrap();
    assert!(
        build_revision::derive(&package).is_err(),
        "an outer checkout is not crate provenance"
    );
    commit(temp.path());
    assert!(
        build_revision::derive(&package).is_err(),
        "a tracked vendored crate must not borrow an unrelated checkout's revision"
    );
    let revision = "a".repeat(40);
    let metadata = package.join(".cargo_vcs_info.json");
    fs::write(
        &metadata,
        serde_json::json!({"git":{"sha1":revision},"path_in_vcs":"crates/harn-cli"}).to_string(),
    )
    .unwrap();
    let derived = build_revision::derive(&package).unwrap();
    assert_ne!(derived.revision, outer_head);
    assert_eq!(derived.revision, revision);
    assert_eq!(derived.watched_paths, vec![metadata.clone()]);
    for value in [
        "{",
        "{}",
        r#"{"git":{"sha1":"short"}}"#,
        r#"{"git":{"sha1":"0000000000000000000000000000000000000000"}}"#,
    ] {
        fs::write(&metadata, value).unwrap();
        assert!(
            build_revision::derive(&package).is_err(),
            "malformed package provenance must be unavailable"
        );
    }
}
