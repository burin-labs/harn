use std::path::Path;

use crate::test_util::process::harn_e2e_command;

fn run(root: &Path, cache: &Path) -> serde_json::Value {
    run_with_marker(root, cache, "shipped-interface-reached")
}

fn run_with_marker(root: &Path, cache: &Path, marker: &str) -> serde_json::Value {
    let home = tempfile::tempdir().unwrap();
    let output = harn_e2e_command()
        .args(["time", "run", "--json"])
        .arg(root.join("main.harn"))
        .env("HARN_CACHE_DIR", cache)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(marker),
        "the imported enum-dependent function must execute: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("time run receipt")
}

fn entries(cache: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_entry_hit(receipt: &serde_json::Value) {
    let compile = receipt["data"]["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["name"] == "bytecode_compile")
        .expect("compile phase reported");
    assert_eq!(compile["cache"], "hit", "{receipt}");
}

#[test]
fn relocated_precompiled_tree_writes_no_shared_cache_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let build = tmp.path().join("build");
    let package = tmp.path().join("release-package");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(
        build.join("leaf.harn"),
        "pub enum Color { Ready(message: string) Empty }\n",
    )
    .unwrap();
    std::fs::write(
        build.join("lib.harn"),
        r#"import "./leaf"
pub fn answer() -> string {
  match Color.Ready("shipped-interface-reached") {
    Color.Ready(message) -> { return message }
    _ -> { return "wrong" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        build.join("main.harn"),
        r#"import { answer } from "./lib"
fn main(harness: Harness) {
  harness.llm.mock_load_jsonl(json_stringify({text: answer(), model: "first-turn-fixture"}) + "\n")
  const reply = harness.llm.call("Say hello.", nil, {provider: "mock", model: "first-turn-fixture"})
  harness.stdio.println(reply.text)
}
"#,
    )
    .unwrap();
    let output = harn_e2e_command()
        .arg("precompile")
        .arg(&build)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["main", "lib", "leaf"] {
        for extension in ["harnbc", "harnmod", "harnifc"] {
            assert!(build.join(format!("{name}.{extension}")).is_file());
        }
    }
    std::fs::rename(&build, &package).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let receipt = run(&package, cache.path());
    assert_entry_hit(&receipt);
    assert!(
        entries(cache.path()).is_empty(),
        "cold shared cache received {:?}",
        entries(cache.path())
    );

    // Removing every interface prevents entry linking from supplying the
    // missing module's projection. A real runtime derivation must be visible.
    for name in ["main", "lib", "leaf"] {
        std::fs::remove_file(package.join(format!("{name}.harnifc"))).unwrap();
    }
    let missing_cache = tempfile::tempdir().unwrap();
    run(&package, missing_cache.path());
    assert!(
        entries(missing_cache.path())
            .iter()
            .any(|name| name.ends_with(".harnifc")),
        "missing interfaces must rederive into the shared cache: {:?}",
        entries(missing_cache.path())
    );
}

#[test]
fn dotted_source_names_keep_distinct_adjacent_artifacts_after_relocation() {
    let tmp = tempfile::tempdir().unwrap();
    let build = tmp.path().join("build");
    std::fs::create_dir(&build).unwrap();
    for (name, message) in [
        ("catalog", "wrong-undotted-module"),
        ("catalog.generated", "shipped-interface-reached"),
    ] {
        std::fs::write(
            build.join(format!("{name}.harn")),
            format!("pub fn answer() -> string {{ return \"{message}\" }}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        build.join("main.harn"),
        "import { answer } from \"./catalog.generated.harn\"\nfn main(harness: Harness) { harness.stdio.println(answer()) }\n",
    )
    .unwrap();
    let output = harn_e2e_command()
        .arg("precompile")
        .arg(&build)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["main", "catalog", "catalog.generated"] {
        for extension in ["harnbc", "harnmod", "harnifc"] {
            assert!(
                build.join(format!("{name}.{extension}")).is_file(),
                "missing adjacent {name}.{extension}"
            );
        }
    }
    let package = tmp.path().join("relocated");
    std::fs::rename(&build, &package).unwrap();
    let cache = tempfile::tempdir().unwrap();
    assert_entry_hit(&run(&package, cache.path()));
    assert!(
        entries(cache.path()).is_empty(),
        "shipped dotted module missed its artifacts: {:?}",
        entries(cache.path())
    );
}

#[test]
fn shipped_entry_preserves_package_authority_after_relocation() {
    let tmp = tempfile::tempdir().unwrap();
    let build = write_package_fixture(tmp.path(), true);
    let output = harn_e2e_command()
        .arg("precompile")
        .arg(&build)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let package = tmp.path().join("relocated");
    std::fs::rename(&build, &package).unwrap();
    let cache = tempfile::tempdir().unwrap();
    assert_entry_hit(&run(&package, cache.path()));

    // The source graph still resolves from the installed generation, but a
    // removed root declaration must refuse the otherwise valid shipped hit.
    std::fs::write(
        package.join("harn.toml"),
        "[package]\nname = \"consumer\"\n",
    )
    .unwrap();
    let rejected_cache = tempfile::tempdir().unwrap();
    let rejected = harn_e2e_command()
        .arg("run")
        .arg(package.join("main.harn"))
        .env("HARN_CACHE_DIR", rejected_cache.path())
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("not declared"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let reprepare = harn_e2e_command()
        .arg("precompile")
        .arg(package.join("main.harn"))
        .output()
        .unwrap();
    assert!(
        !reprepare.status.success(),
        "precompile must not erase missing declaration authority"
    );
}

#[test]
fn precompile_interface_comes_from_the_generation_its_setup_publishes() {
    let tmp = tempfile::tempdir().unwrap();
    let build = write_package_fixture(tmp.path(), false);
    let interface = build.join("main.harnifc");
    let precompile = || {
        let output = harn_e2e_command()
            .arg("precompile")
            .arg(build.join("main.harn"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read(&interface).unwrap()
    };
    let after_setup = precompile();
    std::fs::remove_file(&interface).unwrap();
    // The replacement generation is current now, so this run publishes
    // nothing. An interface walked before setup would still name the stale one.
    assert_eq!(after_setup, precompile());
}

/// `published: false` publishes a generation holding an older `acme` than the
/// lock pins, so setup must publish a replacement before keying from it.
fn write_package_fixture(root: &Path, published: bool) -> std::path::PathBuf {
    use crate::test_util::package_generation::{
        create_package_generation, package_content_hash, publish_package_generation,
    };

    let build = root.join("build");
    let dependency = root.join("dependency");
    std::fs::create_dir(&build).unwrap();
    std::fs::create_dir(&dependency).unwrap();
    let package_manifest = "[package]\nname = \"acme\"\nversion = \"0.1.0\"\n";
    let module = "pub fn answer() -> string { return \"shipped-interface-reached\" }\n";
    std::fs::write(dependency.join("harn.toml"), package_manifest).unwrap();
    std::fs::write(dependency.join("lib.harn"), module).unwrap();
    std::fs::write(
        build.join("harn.toml"),
        "[package]\nname = \"consumer\"\n[dependencies]\nacme = { path = \"../dependency\" }\n",
    )
    .unwrap();
    let lock = format!(
        "version = 4\n[[package]]\nname = \"acme\"\nsource = \"path+{}\"\ncontent_hash = \"{}\"\n",
        url::Url::from_file_path(dependency.canonicalize().unwrap()).unwrap(),
        package_content_hash(&dependency),
    );
    std::fs::write(build.join("harn.lock"), &lock).unwrap();
    let packages = create_package_generation(&build);
    std::fs::create_dir(packages.join("acme")).unwrap();
    std::fs::write(packages.join("acme/harn.toml"), package_manifest).unwrap();
    if published {
        std::fs::write(packages.join("acme/lib.harn"), module).unwrap();
        publish_package_generation(&build, &lock);
    } else {
        let stale = "pub fn answer() -> string { return \"stale\" }\n";
        std::fs::write(packages.join("acme/lib.harn"), stale).unwrap();
        let stale_lock = lock.replace(&package_content_hash(&dependency), "sha256:stale");
        publish_package_generation(&build, &stale_lock);
    }
    std::fs::write(
        build.join("main.harn"),
        "import { answer } from \"acme/lib\"\nfn main(harness: Harness) { harness.stdio.println(answer()) }\n",
    )
    .unwrap();
    build
}

#[test]
fn explicit_cache_preparation_uses_runtime_package_and_authority_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let build = write_package_fixture(tmp.path(), true);
    let manifest = std::fs::read_to_string(build.join("harn.toml")).unwrap();
    std::fs::write(
        build.join("harn.toml"),
        format!("{manifest}\n[check]\ntrusted_host_dispatch = true\n"),
    )
    .unwrap();
    let package = tmp.path().join("relocated");
    std::fs::rename(&build, &package).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let disabled = harn_e2e_command()
        .args(["precompile", "--warm-cache"])
        .arg(package.join("main.harn"))
        .env("HARN_CACHE_DIR", cache.path())
        .env("HARN_BYTECODE_CACHE", "0")
        .output()
        .unwrap();
    assert!(
        !disabled.status.success(),
        "disabled cache cannot count as prepared"
    );
    assert!(
        String::from_utf8_lossy(&disabled.stderr).contains("cache is disabled"),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    let output = harn_e2e_command()
        .args(["precompile", "--warm-cache"])
        .arg(package.join("main.harn"))
        .env("HARN_CACHE_DIR", cache.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("shipped-interface-reached"),
        "preparation executed source"
    );
    let prepared = entries(cache.path());
    for extension in ["harnbc", "harnmod", "harnifc"] {
        assert!(
            prepared.iter().any(|name| name.ends_with(extension)),
            "missing {extension}: {prepared:?}"
        );
    }
    assert!(
        !package.join("main.harnbc").exists(),
        "cache mode must support read-only sources"
    );
    assert_entry_hit(&run(&package, cache.path()));
    assert_eq!(
        entries(cache.path()),
        prepared,
        "execution created an unprepared cache identity"
    );

    // Withhold the prepared modules while retaining the entry and interfaces.
    // The same reached import must visibly compile instead of passing vacuously.
    for name in prepared.iter().filter(|name| name.ends_with(".harnmod")) {
        std::fs::remove_file(cache.path().join(name)).unwrap();
    }
    let missing = entries(cache.path());
    let receipt = run(&package, cache.path());
    assert!(
        entries(cache.path()).len() > missing.len(),
        "missing modules were not recompiled: {receipt}"
    );
}
