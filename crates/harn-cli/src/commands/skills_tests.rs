use super::*;
use crate::env_guard::ScopedEnvVar;
use crate::tests::common::harn_state_lock::lock_harn_state;

#[test]
fn derive_name_extracts_repo_segment() {
    assert_eq!(
        derive_name_from_spec("https://github.com/acme/harn-skills.git"),
        Some("harn-skills".to_string())
    );
    assert_eq!(
        derive_name_from_spec("./local/path/deploy"),
        Some("deploy".to_string())
    );
    assert_eq!(derive_name_from_spec("acme/ops"), Some("ops".to_string()));
}

#[test]
fn resolve_git_url_expands_shorthand() {
    assert_eq!(
        resolve_git_url("acme/ops"),
        "https://github.com/acme/ops.git"
    );
    assert_eq!(
        resolve_git_url("https://example.com/x.git"),
        "https://example.com/x.git"
    );
}

#[test]
fn glob_matches_expected() {
    assert!(glob_match("src/*.rs", "src/main.rs"));
    assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
    assert!(glob_match("infra/**", "infra/terraform/main.tf"));
}

#[test]
fn command_discovery_owns_the_package_generation_it_reads() {
    let _env = lock_harn_state();
    let temp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let _home = ScopedEnvVar::set("HOME", home.path().to_str().unwrap());
    let _skills_path = ScopedEnvVar::unset("HARN_SKILLS_PATH");
    fs::write(temp.path().join("harn.toml"), "[package]\nname = \"app\"\n").unwrap();
    let anchor = temp.path().join("main.harn");
    fs::write(&anchor, "fn main() {}\n").unwrap();
    let packages_root = crate::package::test_support::create_test_package_generation(temp.path());
    crate::package::test_support::write_test_generation_lock(
        temp.path(),
        "version = 4\n\n[[package]]\nname = \"pack\"\nsource = \"path+fixture\"\n",
    );
    let skill_dir = packages_root.join("pack/skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\nshort: review code\n---\nReview the project",
    )
    .unwrap();

    let discovery = build_discovery(&[], anchor.to_str());
    let lease = fs::File::open(
        packages_root
            .parent()
            .unwrap()
            .join(harn_modules::package_snapshot::GENERATION_LEASE_FILE),
    )
    .unwrap();
    assert!(
        lease.try_lock().is_err(),
        "command discovery must retain the generation backing its paths"
    );
    assert_eq!(
        discovery.fetch("review").unwrap().body,
        "Review the project"
    );

    drop(discovery);
    harn_flock::assert_released(&lease);
}
