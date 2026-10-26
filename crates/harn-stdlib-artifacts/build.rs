//! Compile the embedded stdlib with this build's compiler and embed the result.
//!
//! The artifacts carry the Harn version and compiler fingerprint of the
//! harn-vm this build script links, with the builtin families the host
//! selected on this crate. `register` refuses a table whose build identity
//! differs from the running binary; a per-module key mismatch, such as other
//! compiler options, is a miss and that import compiles.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rows = embed(&out_dir);
    let mut table = String::from(
        "/// Stdlib module artifacts compiled by this crate's build script, sorted by file name.\n\
         pub static ARTIFACTS: &[harn_vm::bytecode_cache::EmbeddedStdlibArtifact] = &[\n",
    );
    for Row {
        file_name,
        len,
        path,
    } in rows
    {
        writeln!(
            table,
            "    harn_vm::bytecode_cache::EmbeddedStdlibArtifact {{ file_name: {file_name:?}, len: {len}, zstd: include_bytes!({:?}) }},",
            path.display().to_string(),
        )
        .expect("format table row");
    }
    table.push_str("];\n");
    std::fs::write(out_dir.join("stdlib_artifacts.rs"), table).expect("write artifact table");
}

/// One embedded artifact: its cache file name, decoded size, and the file
/// holding it zstd-compressed.
struct Row {
    file_name: String,
    len: usize,
    path: PathBuf,
}

#[cfg(not(feature = "embed"))]
fn embed(_out_dir: &Path) -> Vec<Row> {
    Vec::new()
}

/// The artifacts are about 10 MB raw and 2.4 MB at this level; level 3 would
/// leave 2.6 MB. Decoding costs the same at any level.
#[cfg(feature = "embed")]
const ZSTD_LEVEL: i32 = 19;

/// Compiler-option overrides a developer shell may carry. Either changes the
/// key every artifact is stored under, so a build that inherited one would
/// embed a table no default runtime ever reads.
#[cfg(feature = "embed")]
const COMPILER_OPTION_ENV: &[&str] = &[
    "HARN_DISABLE_OPTIMIZATIONS",
    "HARN_LEGACY_AMBIENT_CAPABILITIES",
];

#[cfg(feature = "embed")]
fn embed(out_dir: &Path) -> Vec<Row> {
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

    harn_runtime::force_link();
    harn_stdlib::install().expect("install the stdlib this script compiles");
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let build = harn_vm::compile_embedded_stdlib_artifacts(threads);
    for (module, error) in &build.failed {
        println!("cargo:warning=std/{module} is not embedded and compiles on import: {error}");
    }
    // A partial selection legitimately skips modules that call an unselected
    // family. The full product surface has every builtin, so a skip there
    // means the build script's compiler lost a family and the shipped binary
    // would silently compile those modules on every import.
    assert!(
        std::env::var_os("CARGO_FEATURE_FULL").is_none() || build.failed.is_empty(),
        "the full product surface must embed every stdlib module; {} failed",
        build.failed.len()
    );
    let artifacts = build.artifacts;
    let chunk = artifacts.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers = artifacts
            .chunks(chunk)
            .map(|chunk| {
                let artifact_dir = &artifact_dir;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|artifact| {
                            let packed = zstd::bulk::compress(&artifact.bytes, ZSTD_LEVEL)
                                .expect("compress stdlib artifact");
                            let path = artifact_dir.join(format!("{}.zst", artifact.file_name));
                            std::fs::write(&path, packed).expect("write stdlib artifact");
                            Row {
                                file_name: artifact.file_name.clone(),
                                len: artifact.bytes.len(),
                                path,
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("compression worker"))
            .collect()
    })
}
