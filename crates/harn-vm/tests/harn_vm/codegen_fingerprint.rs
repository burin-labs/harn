#[path = "../../build_support/codegen_fingerprint.rs"]
mod codegen_fingerprint_impl;

use std::fs;
use std::path::Path;

use codegen_fingerprint_impl::{compiler_inputs, fingerprint_inputs, watch_roots};

#[test]
fn codegen_fingerprint_is_checkout_path_stable() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    seed_compiler_sources(left.path(), "return 1;");
    seed_compiler_sources(right.path(), "return 1;");

    let left_hash = fingerprint_inputs(&compiler_inputs(&left.path().join("harn-vm")));
    let right_hash = fingerprint_inputs(&compiler_inputs(&right.path().join("harn-vm")));

    assert_eq!(
        left_hash, right_hash,
        "same compiler sources in different checkout roots must produce the same fingerprint"
    );
}

#[test]
fn codegen_fingerprint_tracks_derived_interface_owners() {
    let root = tempfile::tempdir().unwrap();
    seed_compiler_sources(root.path(), "return 1;");
    let manifest = root.path().join("harn-vm");
    let baseline = fingerprint_inputs(&compiler_inputs(&manifest));

    for relative in [
        "harn-modules/src/package_imports.rs",
        "harn-vm/src/bytecode_cache/graph.rs",
        "harn-vm/src/bytecode_cache/interface.rs",
        "harn-vm/src/context_manifest.rs",
        "harn-vm/src/module_source.rs",
    ] {
        let path = root.path().join(relative);
        let original = fs::read(&path).unwrap();
        fs::write(&path, "pub fn changed_interface_behavior() {}").unwrap();
        assert_ne!(
            fingerprint_inputs(&compiler_inputs(&manifest)),
            baseline,
            "{relative} must invalidate interfaces produced by an older build"
        );
        fs::write(path, original).unwrap();
    }

    // New and deleted resolver modules must change the key and retrigger Cargo,
    // even though the previous build never emitted a watch for the new file.
    let new_module = root.path().join("harn-modules/src/new_resolver.rs");
    fs::write(&new_module, "pub fn resolve() {}").unwrap();
    assert_ne!(fingerprint_inputs(&compiler_inputs(&manifest)), baseline);
    assert!(watch_roots(&manifest).contains(&root.path().join("harn-modules/src")));
    assert!(watch_roots(&manifest).contains(&manifest.join("src/bytecode_cache")));
    fs::remove_file(new_module).unwrap();
    assert_eq!(fingerprint_inputs(&compiler_inputs(&manifest)), baseline);

    // A cache key must not depend on unrelated product presentation.
    let unrelated = manifest.join("src/unrelated_ui.rs");
    fs::write(unrelated, "pub fn render() {}").unwrap();
    assert_eq!(fingerprint_inputs(&compiler_inputs(&manifest)), baseline);
}

#[test]
fn codegen_fingerprint_changes_when_input_content_changes() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    seed_compiler_sources(left.path(), "return 1;");
    seed_compiler_sources(right.path(), "return 2;");

    let left_hash = fingerprint_inputs(&compiler_inputs(&left.path().join("harn-vm")));
    let right_hash = fingerprint_inputs(&compiler_inputs(&right.path().join("harn-vm")));

    assert_ne!(
        left_hash, right_hash,
        "compiler source edits must still invalidate bytecode cache keys"
    );
}

#[test]
fn codegen_fingerprint_is_line_ending_stable() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    seed_compiler_sources(left.path(), "fn compile() {\n    return;\n}\n");
    seed_compiler_sources(right.path(), "fn compile() {\r\n    return;\r\n}\r\n");

    let left_hash = fingerprint_inputs(&compiler_inputs(&left.path().join("harn-vm")));
    let right_hash = fingerprint_inputs(&compiler_inputs(&right.path().join("harn-vm")));

    assert_eq!(
        left_hash, right_hash,
        "LF and CRLF checkouts must produce the same compiler fingerprint"
    );
}

#[test]
fn codegen_fingerprint_tracks_module_artifact_compilation() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    seed_compiler_sources(left.path(), "return 1;");
    seed_compiler_sources(right.path(), "return 1;");
    fs::write(
        right.path().join("harn-vm/src/module_artifact.rs"),
        "pub fn changed_artifact_compiler() {}",
    )
    .unwrap();

    let left_hash = fingerprint_inputs(&compiler_inputs(&left.path().join("harn-vm")));
    let right_hash = fingerprint_inputs(&compiler_inputs(&right.path().join("harn-vm")));

    assert_ne!(
        left_hash, right_hash,
        "module artifact compiler edits must invalidate bytecode cache keys"
    );
}

fn seed_compiler_sources(root: &Path, compiler_body: &str) {
    let files = [
        ("harn-lexer/src/lib.rs", "pub fn lex() {}"),
        ("harn-parser/src/lib.rs", "pub fn parse() {}"),
        ("harn-ir/src/lib.rs", "pub fn lower() {}"),
        ("harn-kernel/src/compiler/state.rs", compiler_body),
        ("harn-vm/src/compiler.rs", "pub struct Compiler;"),
        ("harn-vm/src/chunk.rs", "pub struct Chunk;"),
        ("harn-vm/src/chunk/portable.rs", "pub fn adapt() {}"),
        ("harn-modules/src/package_imports.rs", "pub fn resolve() {}"),
        ("harn-vm/src/bytecode_cache/graph.rs", "pub fn derive() {}"),
        (
            "harn-vm/src/bytecode_cache/interface.rs",
            "pub fn recall() {}",
        ),
        ("harn-vm/src/context_manifest.rs", "pub fn validate() {}"),
        ("harn-vm/src/module_source.rs", "pub fn identity() {}"),
        (
            "harn-vm/src/module_artifact.rs",
            "pub fn compile_artifact() {}",
        ),
    ];
    for (relative, contents) in files {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}
