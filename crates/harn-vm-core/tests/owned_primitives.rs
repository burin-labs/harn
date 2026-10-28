//! Structural checks that keep one owner for primitives the workspace used to
//! hand-roll in many places. Each rule scans production Rust sources, so a new
//! copy fails here and points at the owner to call instead.

use std::path::{Path, PathBuf};

/// Production source of every workspace crate: `crates/*/src/**/*.rs`, minus
/// test-only files and each file's trailing `#[cfg(test)]` section.
fn production_sources() -> Vec<(PathBuf, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory");
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(crates)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let path = entry.path();
        let relative = path.strip_prefix(crates).expect("under crates/");
        let parts: Vec<_> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let file = parts.last().cloned().unwrap_or_default();
        if parts.get(1).map(String::as_str) != Some("src")
            || parts.iter().any(|part| part == "tests" || part == "target")
            || file == "tests.rs"
            || file.ends_with("_tests.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("readable source");
        let production = match text.split_once("\n#[cfg(test)]") {
            Some((production, _)) => production.to_string(),
            None => text,
        };
        out.push((relative.to_path_buf(), production));
    }
    out
}

fn assert_no_matches(rule: &str, owner: &str, offenders: Vec<String>) {
    assert!(
        offenders.is_empty(),
        "{rule}: call {owner} instead of hand-rolling it in:\n  {}",
        offenders.join("\n  ")
    );
}

/// `atomic_io` owns temp-file-then-rename. A source that renames over a
/// destination and also names a temp file is a second copy of it.
#[test]
fn atomic_replace_has_one_owner() {
    const ALLOWED: &[&str] = &[
        "harn-vm-core/src/atomic_io.rs",
        // Publishes a staged directory tree, which `atomic_io` (files) cannot.
        "harn-package/src/package/registry/populate.rs",
    ];
    let offenders = production_sources()
        .into_iter()
        .filter(|(path, _)| !ALLOWED.iter().any(|allowed| path.ends_with(allowed)))
        .filter(|(_, text)| {
            let renames = text.contains("fs::rename(");
            let names_temp = text.contains(".tmp")
                || text.contains("tmp_path")
                || text.contains("temp_path")
                || text.contains("\"tmp");
            renames && names_temp
        })
        .map(|(path, _)| path.display().to_string())
        .collect();
    assert_no_matches(
        "atomic replace",
        "harn_vm_core::atomic_io (or tempfile::NamedTempFile::persist)",
        offenders,
    );
}
