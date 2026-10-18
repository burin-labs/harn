//! Compile the embedded stdlib with this build's compiler and embed the result.
//!
//! The artifacts carry the Harn version and compiler fingerprint of the
//! harn-vm this build script links. The embed feature selects the same full
//! native builtin contract on this compiler and the host's harn-vm, so the
//! runtime accepts them. Anything that makes them
//! disagree is a header mismatch at load time, and the import falls back to
//! compiling, exactly as it did before this crate existed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rows = embed(&out_dir);
    let mut table = String::from(
        "/// Stdlib module artifacts compiled by this crate's build script, sorted by file name.\n\
         pub static ARTIFACTS: &[harn_vm::bytecode_cache::EmbeddedStdlibArtifact] = &[\n",
    );
    for (file_name, path) in rows {
        writeln!(
            table,
            "    harn_vm::bytecode_cache::EmbeddedStdlibArtifact {{ file_name: {file_name:?}, bytes: include_bytes!({:?}) }},",
            path.display().to_string(),
        )
        .expect("format table row");
    }
    table.push_str("];\n");
    std::fs::write(out_dir.join("stdlib_artifacts.rs"), table).expect("write artifact table");
}

#[cfg(not(feature = "embed"))]
fn embed(_out_dir: &Path) -> Vec<(String, PathBuf)> {
    Vec::new()
}

/// Compiler-option overrides a developer shell may carry. Either changes the
/// key every artifact is stored under, so a build that inherited one would
/// embed a table no default runtime ever reads.
#[cfg(feature = "embed")]
const COMPILER_OPTION_ENV: &[&str] = &[
    "HARN_DISABLE_OPTIMIZATIONS",
    "HARN_LEGACY_AMBIENT_CAPABILITIES",
];

#[cfg(feature = "embed")]
fn embed(out_dir: &Path) -> Vec<(String, PathBuf)> {
    for name in COMPILER_OPTION_ENV {
        println!("cargo:rerun-if-env-changed={name}");
        // The build script is still single-threaded here.
        std::env::remove_var(name);
    }
    let artifact_dir = out_dir.join("stdlib");
    if artifact_dir.exists() {
        std::fs::remove_dir_all(&artifact_dir).expect("clear previous stdlib artifacts");
    }
    std::fs::create_dir_all(&artifact_dir).expect("create stdlib artifact dir");

    harn_stdlib::install().expect("install the stdlib this script compiles");
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let build = harn_vm::compile_embedded_stdlib_artifacts(threads);
    for (module, error) in &build.failed {
        println!("cargo:warning=std/{module} is not embedded and compiles on import: {error}");
    }
    build
        .artifacts
        .into_iter()
        .map(|artifact| {
            let path = artifact_dir.join(&artifact.file_name);
            std::fs::write(&path, &artifact.bytes).expect("write stdlib artifact");
            (artifact.file_name, path)
        })
        .collect()
}
