//! Pack every catalog file into one zstd archive.
//!
//! The sources are about 5.9 MB as text and 1.1 MB compressed together, since
//! modules repeat each other's vocabulary. `lib.rs` decodes the archive once,
//! on the first catalog read, and slices it by the lengths written here.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

macro_rules! catalog_entries {
    (
        sources: [$($_module:literal => $module_path:literal),* $(,)?],
        prompt_assets: [$($_asset:literal => $asset_path:literal),* $(,)?],
        cli_scripts: [$($_script:literal => $script_path:literal),* $(,)?] $(,)?
    ) => {
        /// Every file in catalog order: sources, prompt assets, CLI scripts.
        const FILES: &[&str] = &[$($module_path,)* $($asset_path,)* $($script_path,)*];
    };
}

include!("src/catalog_entries.rs");

/// The archive is built once per stdlib edit; decoding costs the same at
/// any level.
const ZSTD_LEVEL: i32 = 19;

fn main() {
    let src =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it")).join("src");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    println!("cargo:rerun-if-changed=src/catalog_entries.rs");

    let mut text = String::new();
    let mut lengths = String::new();
    for file in FILES {
        println!("cargo:rerun-if-changed=src/{file}");
        let source = read(&src.join(file));
        write!(lengths, "{},", source.len()).expect("format length");
        text.push_str(&source);
    }
    let archive = zstd::bulk::compress(text.as_bytes(), ZSTD_LEVEL).expect("compress stdlib");
    let archive_path = out_dir.join("stdlib.zst");
    std::fs::write(&archive_path, archive).expect("write stdlib archive");
    std::fs::write(
        out_dir.join("stdlib_archive.rs"),
        format!(
            "const ARCHIVE: &[u8] = include_bytes!({:?});\n\
             const ARCHIVE_TEXT_LEN: usize = {};\n\
             const LENGTHS: &[usize] = &[{lengths}];\n",
            archive_path.display().to_string(),
            text.len(),
        ),
    )
    .expect("write archive bindings");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read stdlib file {}: {error}", path.display()))
}
