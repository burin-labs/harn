#[path = "../build_support/codegen_fingerprint.rs"]
mod codegen_fingerprint;

use std::fs;
use std::path::Path;

use codegen_fingerprint::{compiler_inputs, fingerprint_inputs, watch_roots};

#[test]
fn codegen_fingerprint_is_checkout_path_stable() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    seed_compiler_sources(left.path(), "return 1;");
    seed_compiler_sources(right.path(), "return 1;");
    assert_eq!(watch_roots(&left.path().join("harn-vm")).len(), 4);

    let left_hash = fingerprint_inputs(&compiler_inputs(&left.path().join("harn-vm")));
    let right_hash = fingerprint_inputs(&compiler_inputs(&right.path().join("harn-vm")));

    assert_eq!(
        left_hash, right_hash,
        "same compiler sources in different checkout roots must produce the same fingerprint"
    );
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

fn seed_compiler_sources(root: &Path, compiler_body: &str) {
    let files = [
        ("harn-lexer/src/lib.rs", "pub fn lex() {}"),
        ("harn-parser/src/lib.rs", "pub fn parse() {}"),
        ("harn-ir/src/lib.rs", "pub fn lower() {}"),
        ("harn-vm/src/compiler/state.rs", compiler_body),
        ("harn-vm/src/chunk.rs", "pub struct Chunk;"),
    ];
    for (relative, contents) in files {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}
