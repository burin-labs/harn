use harn_modules::package_snapshot::{
    generation_root, package_current_path, package_lock_digest, package_publication_lock_path,
    PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
    GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
};

use super::tests_runtime::run_harn_at;

#[test]
fn package_export_import_executes_through_manifest_alias() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let generation = "generation-test";
    let generation_root = generation_root(root, generation);
    let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
    std::fs::create_dir_all(packages_root.join("acme/runtime")).unwrap();
    let lock = "version = 4\n\n[[package]]\nname = \"acme\"\n";
    std::fs::write(generation_root.join(GENERATION_LOCK_FILE), lock).unwrap();
    std::fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest =
        PackageGenerationManifest::new(generation, package_lock_digest(lock.as_bytes())).unwrap();
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
    std::fs::write(
        packages_root.join("acme/harn.toml"),
        "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
    )
    .unwrap();
    std::fs::write(
        packages_root.join("acme/runtime/capabilities.harn"),
        "pub fn exported_capability() { return 41 + 1 }\n",
    )
    .unwrap();
    let entry = root.join("main.harn");
    let source = r#"
import "acme/capabilities"

pipeline main(task) {
  __io_println(exported_capability())
}
"#;

    let (out, _) = run_harn_at(&entry, source).unwrap();
    assert_eq!(out.trim(), "42");
}
