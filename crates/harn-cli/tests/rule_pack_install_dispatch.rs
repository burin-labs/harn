//! End-to-end coverage for installed-package rule packs (#2846): a pack
//! fetched with `harn add` materializes in the current immutable generation
//! and is consumed by name via `harn scan/codemod --rule-pack <name>`, reading
//! the pack's own `[rules] ruleDirs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn create_package_generation(root: &Path, lock_body: &str) -> PathBuf {
    use harn_modules::package_snapshot::{
        generation_root, package_current_path, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };

    let generation = "generation-test";
    let generation_root = generation_root(root, generation);
    let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
    std::fs::create_dir_all(&packages_root).unwrap();
    std::fs::write(generation_root.join(GENERATION_LOCK_FILE), lock_body).unwrap();
    std::fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest = PackageGenerationManifest::new(
        generation,
        harn_modules::package_snapshot::package_lock_digest(lock_body.as_bytes()),
    )
    .unwrap();
    std::fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        package_current_path(root),
        toml::to_string_pretty(&PackageGenerationPointer::new(generation).unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::File::create(package_publication_lock_path(root)).unwrap();
    packages_root
}

fn project_with_installed_pack(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-pack-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lock_body = r#"version = 4

[[package]]
name = "my-rules"
source = "git+https://github.com/acme/my-rules"

[package.registry]
source = "index.toml"
name = "@acme/my-rules"
version = "0.1.0"
"#;
    let packages = create_package_generation(&dir, lock_body);
    let pack = packages.join("my-rules/rules");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // The consuming project.
    std::fs::write(dir.join("harn.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("harn.lock"), lock_body).unwrap();
    // The installed pack ships its own manifest declaring where its rules live.
    std::fs::write(
        packages.join("my-rules/harn.toml"),
        "[rules]\nruleDirs = [\"rules\"]\n",
    )
    .unwrap();
    std::fs::write(
        pack.join("no-foo.toml"),
        "id = \"no-foo\"\nlanguage = \"typescript\"\nmessage = \"no foo\"\n[rule]\npattern = \"foo()\"\n",
    )
    .unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn harn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn rule_pack_resolves_an_installed_package_by_name() {
    let dir = project_with_installed_pack("resolve");
    std::fs::write(dir.join("src/a.ts"), "foo();\nfoo();\n").unwrap();

    let (stdout, stderr, code) = run(&dir, &["scan", "--rule-pack", "my-rules", "src", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["summary"]["total"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_pack_resolves_an_installed_package_by_registry_name() {
    let dir = project_with_installed_pack("scoped");
    std::fs::write(dir.join("src/a.ts"), "foo();\nfoo();\n").unwrap();

    let (stdout, stderr, code) = run(
        &dir,
        &["scan", "--rule-pack", "@acme/my-rules", "src", "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["summary"]["total"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_pack_resolves_an_installed_package_by_registry_name_and_version() {
    let dir = project_with_installed_pack("scoped-version");
    std::fs::write(dir.join("src/a.ts"), "foo();\n").unwrap();

    let (stdout, stderr, code) = run(
        &dir,
        &[
            "scan",
            "--rule-pack",
            "@acme/my-rules@0.1.0",
            "src",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["summary"]["total"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_pack_name_suggests_harn_add() {
    let dir = project_with_installed_pack("unknown");
    std::fs::write(dir.join("src/a.ts"), "foo();\n").unwrap();

    let (_stdout, stderr, code) = run(&dir, &["scan", "--rule-pack", "not-installed", "src"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("harn add"), "stderr={stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}
