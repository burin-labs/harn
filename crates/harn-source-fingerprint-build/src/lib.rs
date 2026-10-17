//! Build-script source collection, confined to the crate that owns the inputs.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use harn_source_fingerprint::{SourceContribution, SourceFingerprint, SourceIdentity};

/// Emit a standard crate source contribution, including its build script.
pub fn emit_crate_source() {
    emit(
        "HARN_SOURCE_FINGERPRINT",
        harn_source_fingerprint::CRATE_SOURCE_ROOTS,
    );
}

/// Recompute declared source contributions for an explicitly located checkout.
/// Generators use this to refuse a stale compiled compiler.
pub fn fingerprint_components(
    identity: SourceIdentity<'_>,
    locate_owner: impl Fn(&str) -> PathBuf,
) -> io::Result<SourceFingerprint> {
    let fingerprints = identity
        .components
        .iter()
        .map(|component| {
            fingerprint(
                component.name,
                &locate_owner(component.name),
                component.roots,
            )
        })
        .collect::<io::Result<Vec<_>>>()?;
    let observed: Vec<_> = identity
        .components
        .iter()
        .zip(&fingerprints)
        .map(|(component, fingerprint)| SourceContribution {
            name: component.name,
            fingerprint: fingerprint.as_str(),
            roots: component.roots,
        })
        .collect();
    Ok(harn_source_fingerprint::compose(identity.domain, &observed))
}

/// Hash selected crate-local files or trees. Every root must exist and at least
/// one input must be present. Rust source line endings are normalized.
pub fn fingerprint(
    crate_name: &str,
    manifest_dir: &Path,
    roots: &[&str],
) -> io::Result<SourceFingerprint> {
    if crate_name.is_empty() || roots.is_empty() {
        return Err(invalid("source owner and roots must be nonempty"));
    }
    let mut files = BTreeMap::new();
    for root in roots {
        if root.is_empty()
            || !Path::new(root)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(invalid("source roots must be plain crate-relative paths"));
        }
        collect(manifest_dir, &manifest_dir.join(root), &mut files)?;
    }
    if files.is_empty() {
        return Err(invalid("source contribution has no inputs"));
    }
    let mut hash = Sha256::new();
    hash.update(b"harn.source-contribution.v1\0");
    hash.update(crate_name.as_bytes());
    hash.update([0]);
    for (logical, bytes) in files {
        hash.update(logical.as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(SourceFingerprint::from_digest(hash.finalize().into()))
}

/// Emit a contribution and watch exactly its existing, crate-local roots.
/// Cargo recursively watches directory roots, including additions and removals.
pub fn emit(variable: &str, roots: &[&str]) {
    assert!(
        !variable.is_empty()
            && variable
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
        "fingerprint environment name must be uppercase ASCII"
    );
    let manifest_dir =
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let crate_name = std::env::var("CARGO_PKG_NAME").expect("Cargo sets CARGO_PKG_NAME");
    let fingerprint = fingerprint(&crate_name, Path::new(&manifest_dir), roots)
        .unwrap_or_else(|error| panic!("fingerprint {crate_name}: {error}"));
    for root in roots {
        println!("cargo:rerun-if-changed={root}");
    }
    println!("cargo:rustc-env={variable}={}", fingerprint.as_str());
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn collect(root: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid(
            "source inputs must be files or directories, not symlinks",
        ));
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect(root, &entry?.path(), files)?;
        }
    } else if metadata.is_file() {
        let logical = path
            .strip_prefix(root)
            .map_err(|_| invalid("source escaped its owner"))?
            .to_str()
            .ok_or_else(|| invalid("source path is not UTF-8"))?
            .replace('\\', "/");
        let bytes = std::fs::read(path)?;
        let bytes = if path.extension().is_some_and(|extension| extension == "rs") {
            canonical_rust_source(&bytes)
        } else {
            bytes
        };
        files.insert(logical, bytes);
    } else {
        return Err(invalid("source input is not a regular file or directory"));
    }
    Ok(())
}

fn canonical_rust_source(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                normalized.push(b'\n');
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            }
            byte => normalized.push(byte),
        }
        index += 1;
    }
    normalized
}
