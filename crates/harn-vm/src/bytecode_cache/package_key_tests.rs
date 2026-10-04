//! A reinstall of identical packages must keep relocatable entry keys.

use std::path::Path;

use crate::bytecode_cache::CacheKey;

/// Publish one `acme` package generation under `root` and point the project at
/// it, the layout `harn install` writes.
pub(super) fn publish_acme_generation(root: &Path, generation: &str, capability_body: &str) {
    use harn_modules::package_snapshot::{
        generation_root, package_current_path, package_lock_digest, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };

    std::fs::create_dir_all(root.join(".git")).unwrap();
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
        capability_body,
    )
    .unwrap();
}

#[test]
fn relocatable_entry_key_survives_reinstalling_identical_packages() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let edited = tempfile::tempdir().unwrap();
    let entry_source = "import \"acme/capabilities\"\nfn main() { return exported_capability() }\n";
    let capability = "pub fn exported_capability() { return 42 }\n";

    // Every install mints a fresh generation id for the same package bytes.
    publish_acme_generation(first.path(), "generation-0001", capability);
    publish_acme_generation(second.path(), "generation-0002", capability);
    publish_acme_generation(
        edited.path(),
        "generation-0003",
        "pub fn exported_capability() { return 43 }\n",
    );
    for root in [first.path(), second.path(), edited.path()] {
        std::fs::write(root.join("entry.harn"), entry_source).unwrap();
    }

    let key =
        |root: &Path| CacheKey::from_relocatable_source(&root.join("entry.harn"), entry_source);
    assert_eq!(
        key(first.path()),
        key(second.path()),
        "a reinstall of identical package bytes must keep the packaged key"
    );
    assert_ne!(
        key(first.path()),
        key(edited.path()),
        "changed package content must still invalidate the packaged key"
    );
}
