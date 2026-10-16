//! Graph-walk output selection for [`super::walk_import_graph_fingerprinted`].

use super::*;

#[test]
fn hash_only_walk_matches_the_manifest_walk_without_building_a_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(
        tmp.path().join("lib.harn"),
        "pub fn helper() { return 1 }\n",
    )
    .unwrap();
    std::fs::write(&entry, "import { helper } from \"./lib\"\nhelper()\n").unwrap();
    let source = std::fs::read_to_string(&entry).unwrap();

    let hashes =
        walk_import_graph_fingerprinted(&entry, &source, CODEGEN_FINGERPRINT, GraphOutputs::Hashes);
    let with_manifest = walk_import_graph_fingerprinted(
        &entry,
        &source,
        CODEGEN_FINGERPRINT,
        GraphOutputs::Manifest,
    );

    assert_eq!(hashes.canonical, with_manifest.canonical);
    assert_eq!(hashes.relocatable, with_manifest.relocatable);
    assert!(
        hashes.manifest.is_none(),
        "a key-only walk builds no manifest"
    );
    assert!(
        with_manifest.manifest.is_some(),
        "the comparison walk must build the manifest it is compared against"
    );
}
