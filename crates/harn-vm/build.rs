//! Computes a fingerprint of the compiler front-end at build time so the
//! bytecode cache key tracks the *compiler's behavior*, not just the released
//! version.
//!
//! `harn run` caches compiled bytecode on disk keyed by source content and the
//! crate version. Within a single version the compiler still changes constantly
//! during development, so an entry produced by an older compiler would be
//! replayed silently — masking the new compiler's output. This is exactly what
//! hid the #2610 fix until `~/.cache/harn/bytecode` was manually cleared, and
//! is tracked as #2621.
//!
//! We close that gap by hashing the source of every crate that determines the
//! bytecode emitted for a given program — the lexer, parser, and IR
//! (source → typed AST) plus this crate's code generator and bytecode/`Chunk`
//! types — and baking the digest into the binary as `HARN_CODEGEN_FINGERPRINT`.
//! The `cargo:rerun-if-changed` lines recompute it whenever any of those files
//! change, so the cache invalidates itself with no manual wipe and no
//! hand-maintained version constant. Over-inclusion only costs an occasional
//! recompile; omitting a code-generation input would reintroduce the bug, so
//! the set is deliberately drawn wide around the front-end.

use std::path::PathBuf;

#[path = "build_support/codegen_fingerprint.rs"]
mod codegen_fingerprint;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let inputs = codegen_fingerprint::compiler_inputs(&manifest_dir);
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.disk_path.display());
    }
    // Watch the roots themselves so file additions and removals also retrigger
    // the build script, not just edits to already-tracked files.
    for root in codegen_fingerprint::watch_roots(&manifest_dir) {
        println!("cargo:rerun-if-changed={}", root.display());
    }

    let hex = codegen_fingerprint::fingerprint_inputs(&inputs);
    println!("cargo:rustc-env=HARN_CODEGEN_FINGERPRINT={hex}");
}
