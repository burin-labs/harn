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

#[test]
fn walks_parse_each_imported_file_version_once() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = tmp.path().join("entry.harn");
    let lib = tmp.path().join("lib.harn");
    std::fs::write(
        tmp.path().join("colors.harn"),
        "pub enum Color { Ready(message: string) Empty }\n",
    )
    .unwrap();
    std::fs::write(&lib, "pub fn helper() { return 1 }\n").unwrap();
    std::fs::write(&entry, "import { helper } from \"./lib\"\nhelper()\n").unwrap();
    let source = std::fs::read_to_string(&entry).unwrap();
    let parses = || crate::module_artifact::MODULE_PARSES.with(std::cell::Cell::get);
    let lib_context = || {
        let walk = walk_import_graph_fingerprinted(
            &entry,
            &source,
            CODEGEN_FINGERPRINT,
            GraphOutputs::Manifest,
        );
        let manifest = walk.manifest.expect("a readable graph builds a manifest");
        let lib_identity = crate::module_source::canonical_identity(&lib);
        manifest
            .files
            .into_iter()
            .find(|file| file.path == lib_identity)
            .expect("the manifest records the imported file")
            .compilation_context
    };

    let first = parses();
    assert_eq!(lib_context(), ModuleCompilationContext::default());
    assert_eq!(
        parses() - first,
        1,
        "the first walk parses the imported file"
    );
    let second = parses();
    lib_context();
    assert_eq!(
        parses(),
        second,
        "a later walk reuses the parsed file version"
    );

    std::fs::write(
        &lib,
        "import \"./colors\"\npub fn helper(value) {\n  match value {\n    Color.Ready(message) -> { return message }\n    _ -> { return \"other\" }\n  }\n}\n",
    )
    .unwrap();
    let edited = parses();
    assert_ne!(
        lib_context(),
        ModuleCompilationContext::default(),
        "an edited file is projected from its new text"
    );
    assert_eq!(
        parses() - edited,
        2,
        "the edit and its new import parse once each"
    );
}
