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

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let crates_dir = manifest_dir.parent().expect("crate dir has a parent");

    // Directory roots whose `.rs` content determines emitted bytecode.
    let roots = [
        crates_dir.join("harn-lexer").join("src"),
        crates_dir.join("harn-parser").join("src"),
        crates_dir.join("harn-ir").join("src"),
        manifest_dir.join("src").join("compiler"),
    ];
    // Single files outside those roots that still shape the bytecode/`Chunk`
    // format written to and read from the cache.
    let single_files = [manifest_dir.join("src").join("chunk.rs")];

    let mut files = Vec::new();
    for root in &roots {
        collect_rs_files(root, &mut files);
    }
    for file in &single_files {
        if file.is_file() {
            files.push(file.clone());
        }
    }
    // Stable order → reproducible digest across builds.
    files.sort();

    let mut hasher = Sha256::new();
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        // Mix the path in too, so adding/removing/renaming a file shifts the
        // digest even when total content is unchanged.
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update(&bytes);
        }
    }
    // Watch the roots themselves so file additions and removals also retrigger
    // the build script, not just edits to already-tracked files.
    for root in &roots {
        println!("cargo:rerun-if-changed={}", root.display());
    }

    // `sha2`'s digest is a byte `Array` that does not implement `LowerHex`, so
    // hex-encode it ourselves.
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("cargo:rustc-env=HARN_CODEGEN_FINGERPRINT={hex}");
}

/// Recursively collect every `.rs` file under `dir`. A missing directory is
/// skipped so the build still succeeds outside the workspace (e.g. a packaged
/// crate), degrading to whatever sources are present.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
