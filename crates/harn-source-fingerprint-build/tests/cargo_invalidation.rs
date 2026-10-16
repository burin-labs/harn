use std::fs;
use std::path::Path;
use std::process::Command;

fn build(root: &Path) -> (String, Vec<serde_json::Value>) {
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--offline",
            "--message-format=json",
            "-p",
            "fingerprint-consumer",
        ])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("CARGO_BUILD_BUILD_DIR", root.join("build"))
        .env("CARGO_INCREMENTAL", "0")
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_BUILD_RUSTC_WRAPPER", "")
        .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "")
        .output()
        .expect("run the fixture's owning Cargo build");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records
        .iter()
        .any(|row| row["reason"] == "build-finished" && row["success"] == true));
    let artifacts: Vec<_> = records
        .into_iter()
        .filter(|row| row["reason"] == "compiler-artifact")
        .collect();
    assert!(
        !artifacts.is_empty(),
        "Cargo must report the artifacts it judged"
    );
    assert!(artifacts
        .iter()
        .any(|row| row["target"]["name"] == "fingerprint_component"));
    assert!(artifacts.iter().all(|row| row["fresh"].is_boolean()));
    let executable = root.join("target/debug").join(format!(
        "fingerprint-consumer{}",
        std::env::consts::EXE_SUFFIX
    ));
    let executed = Command::new(executable).output().unwrap();
    assert!(executed.status.success());
    let fingerprint = String::from_utf8(executed.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(fingerprint, "0".repeat(64));
    (fingerprint, artifacts)
}

#[test]
fn cargo_reuses_unchanged_inputs_and_invalidates_a_changed_dependency() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir_all(root.join("component/src")).unwrap();
    fs::create_dir_all(root.join("consumer/src")).unwrap();
    let build_helper = Path::new(env!("CARGO_MANIFEST_DIR"));
    let helper = build_helper
        .parent()
        .unwrap()
        .join("harn-source-fingerprint");
    let helper = serde_json::to_string(&helper.to_string_lossy()).unwrap();
    let build_helper = serde_json::to_string(&build_helper.to_string_lossy()).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=['component','consumer']\nresolver='2'\n",
    )
    .unwrap();
    fs::write(root.join("component/Cargo.toml"), format!(
        "[package]\nname='fingerprint-component'\nversion='0.1.0'\nedition='2021'\n\n[build-dependencies]\nharn-source-fingerprint-build={{path={build_helper}}}\n"
    )).unwrap();
    let build_script = root.join("component/build.rs");
    let build_source = "fn main() { harn_source_fingerprint_build::emit_crate_source(); }\n";
    fs::write(&build_script, build_source).unwrap();
    let component = root.join("component/src/lib.rs");
    let original = "pub const SOURCE_FINGERPRINT: &str = env!(\"HARN_SOURCE_FINGERPRINT\");\npub fn value() -> u32 { 1 }\n";
    fs::write(&component, original).unwrap();
    fs::write(root.join("consumer/Cargo.toml"), format!(
        "[package]\nname='fingerprint-consumer'\nversion='0.1.0'\nedition='2021'\n\n[dependencies]\nfingerprint-component={{path='../component'}}\nharn-source-fingerprint={{path={helper}}}\n"
    )).unwrap();
    let consumer = root.join("consumer/src/main.rs");
    let consumer_source = "const FINGERPRINT: &str = harn_source_fingerprint::compose(\"compiler-probe\", &[harn_source_fingerprint::SourceContribution::crate_sources(\"component\", fingerprint_component::SOURCE_FINGERPRINT)]).as_str();\nfn main() { println!(\"{FINGERPRINT}\"); }\n";
    fs::write(&consumer, consumer_source).unwrap();

    let (first, built) = build(root);
    assert!(built
        .iter()
        .any(|row| row["target"]["name"] == "fingerprint_component" && row["fresh"] == false));
    let (unchanged, fresh) = build(root);
    assert_eq!(first, unchanged);
    assert!(
        fresh.iter().all(|row| row["fresh"] == true),
        "unchanged source must not rebuild"
    );

    fs::write(&component, original.replace("{ 1 }", "{ 2 }")).unwrap();
    let (changed, rebuilt) = build(root);
    assert_ne!(
        first, changed,
        "the consumer must receive the dependency's changed contribution"
    );
    assert!(rebuilt
        .iter()
        .any(|row| row["target"]["name"] == "fingerprint_component" && row["fresh"] == false));
    assert_eq!(fs::read_to_string(&consumer).unwrap(), consumer_source);

    let added = root.join("component/src/added.rs");
    fs::write(&added, "pub fn added_compiler_input() {}\n").unwrap();
    assert_ne!(
        changed,
        build(root).0,
        "new input must retrigger the owning build script"
    );
    fs::remove_file(added).unwrap();
    assert_eq!(
        changed,
        build(root).0,
        "removing that input restores its contribution"
    );
    fs::write(
        &build_script,
        format!("{build_source}// changed compiler build policy\n"),
    )
    .unwrap();
    assert_ne!(
        changed,
        build(root).0,
        "build-script source also contributes to the identity"
    );
}
