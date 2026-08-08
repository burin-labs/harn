use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::{PackageExecutionError, CACHE_METADATA_FILE, CONTENT_HASH_FILE};

pub const CANONICAL_CONTENT_HASH_PREFIX: &str = "sha256-v2:";
const ARCHIVE_CONTENT_HASH_PREFIX: &str = "sha256:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageContentHashAlgorithm {
    CanonicalV2,
    ArchiveV1,
}

impl PackageContentHashAlgorithm {
    fn parse(hash: &str) -> Result<Self, PackageExecutionError> {
        let (algorithm, hex) = if let Some(hex) = hash.strip_prefix(CANONICAL_CONTENT_HASH_PREFIX) {
            (Self::CanonicalV2, hex)
        } else if let Some(hex) = hash.strip_prefix(ARCHIVE_CONTENT_HASH_PREFIX) {
            (Self::ArchiveV1, hex)
        } else {
            return Err(PackageExecutionError::Invalid(format!(
                "package content hash must use sha256-v2:<64 hex> or archive sha256:<64 hex>, got {hash}"
            )));
        };
        if !is_sha256_hex(hex) {
            return Err(PackageExecutionError::Invalid(format!(
                "package content hash must use sha256-v2:<64 hex> or archive sha256:<64 hex>, got {hash}"
            )));
        }
        Ok(algorithm)
    }
}

pub fn compute_package_content_hash(dir: &Path) -> Result<String, PackageExecutionError> {
    compute_canonical_package_content_hash_capturing(dir, None).map(|(hash, _)| hash)
}

pub fn compute_archive_content_hash(dir: &Path) -> Result<String, PackageExecutionError> {
    compute_archive_content_hash_capturing(dir, None).map(|(hash, _)| hash)
}

pub fn is_canonical_package_content_hash(hash: &str) -> bool {
    hash.strip_prefix(CANONICAL_CONTENT_HASH_PREFIX)
        .is_some_and(is_sha256_hex)
}

pub fn verify_package_content_hash(
    dir: &Path,
    expected: &str,
) -> Result<String, PackageExecutionError> {
    compute_package_content_hash_capturing(dir, None, expected).map(|(hash, _)| hash)
}

pub(super) fn compute_package_content_hash_capturing(
    dir: &Path,
    capture: Option<&Path>,
    expected: &str,
) -> Result<(String, Option<Vec<u8>>), PackageExecutionError> {
    match PackageContentHashAlgorithm::parse(expected)? {
        PackageContentHashAlgorithm::CanonicalV2 => {
            compute_canonical_package_content_hash_capturing(dir, capture)
        }
        PackageContentHashAlgorithm::ArchiveV1 => {
            compute_archive_content_hash_capturing(dir, capture)
        }
    }
}

fn compute_archive_content_hash_capturing(
    dir: &Path,
    capture: Option<&Path>,
) -> Result<(String, Option<Vec<u8>>), PackageExecutionError> {
    let mut files = Vec::new();
    collect_hashable_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    let mut captured = None;
    for relative in files {
        let normalized = normalized_package_relative_path(&relative);
        let path = dir.join(&relative);
        let contents = read_regular_file(&path)?;
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        hasher.update(encode_hex(&Sha256::digest(&contents)).as_bytes());
        if capture == Some(relative.as_path()) {
            captured = Some(contents);
        }
    }
    Ok((
        format!(
            "{ARCHIVE_CONTENT_HASH_PREFIX}{}",
            encode_hex(&hasher.finalize())
        ),
        captured,
    ))
}

fn compute_canonical_package_content_hash_capturing(
    dir: &Path,
    capture: Option<&Path>,
) -> Result<(String, Option<Vec<u8>>), PackageExecutionError> {
    let mut paths = Vec::new();
    collect_hashable_files(dir, dir, &mut paths)?;
    let mut files = paths
        .into_iter()
        .map(|relative| {
            canonical_package_relative_path(&relative).map(|normalized| (normalized, relative))
        })
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for adjacent in files.windows(2) {
        if adjacent[0].0 == adjacent[1].0 {
            return Err(PackageExecutionError::Invalid(format!(
                "package paths {} and {} have the same canonical identity '{}'",
                adjacent[0].1.display(),
                adjacent[1].1.display(),
                adjacent[0].0
            )));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"harn-package-content-v2\0");
    let mut captured = None;
    for (normalized_path, relative) in files {
        let path = dir.join(&relative);
        let contents = read_regular_file(&path)?;
        let canonical_contents = canonical_file_contents(&contents);
        hash_framed(&mut hasher, normalized_path.as_bytes());
        hash_framed(&mut hasher, &Sha256::digest(canonical_contents.as_ref()));
        if capture == Some(relative.as_path()) {
            captured = Some(contents);
        }
    }
    Ok((
        format!(
            "{CANONICAL_CONTENT_HASH_PREFIX}{}",
            encode_hex(&hasher.finalize())
        ),
        captured,
    ))
}

fn hash_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn canonical_file_contents(contents: &[u8]) -> Cow<'_, [u8]> {
    if contents.contains(&0) || std::str::from_utf8(contents).is_err() {
        return Cow::Borrowed(contents);
    }
    if !contents.contains(&b'\r') {
        return Cow::Borrowed(contents);
    }
    let mut normalized = Vec::with_capacity(contents.len());
    let mut index = 0;
    while index < contents.len() {
        if contents[index] == b'\r' {
            normalized.push(b'\n');
            index += usize::from(contents.get(index + 1) == Some(&b'\n')) + 1;
        } else {
            normalized.push(contents[index]);
            index += 1;
        }
    }
    Cow::Owned(normalized)
}

fn collect_hashable_files(
    root: &Path,
    cursor: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), PackageExecutionError> {
    let entries = fs::read_dir(cursor).map_err(|error| {
        PackageExecutionError::io("read directory", cursor.to_path_buf(), error)
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            PackageExecutionError::io("read directory entry", cursor.to_path_buf(), error)
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PackageExecutionError::io("stat", path.clone(), error))?;
        let name = entry.file_name();
        if excluded_package_name(&name) {
            continue;
        }
        if file_type.is_symlink() {
            return Err(PackageExecutionError::Invalid(format!(
                "package content contains unsupported symlink: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_hashable_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                PackageExecutionError::Invalid(format!(
                    "failed to relativize {}: {error}",
                    path.display()
                ))
            })?;
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, PackageExecutionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| PackageExecutionError::io("stat", path.to_path_buf(), error))?;
    if !metadata.file_type().is_file() {
        return Err(PackageExecutionError::Invalid(format!(
            "package content is not a regular file: {}",
            path.display()
        )));
    }
    fs::read(path).map_err(|error| PackageExecutionError::io("read", path.to_path_buf(), error))
}

pub(super) fn excluded_package_name(name: &OsStr) -> bool {
    name == OsStr::new(".git")
        || name == OsStr::new(".gitignore")
        || name == OsStr::new("CLAUDE.md")
        || name == OsStr::new(CONTENT_HASH_FILE)
        || name == OsStr::new(CACHE_METADATA_FILE)
}

pub fn normalized_package_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn canonical_package_relative_path(path: &Path) -> Result<String, PackageExecutionError> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(PackageExecutionError::Invalid(format!(
                "package content path is not relative and normalized: {}",
                path.display()
            )));
        };
        let value = value.to_str().ok_or_else(|| {
            PackageExecutionError::Invalid(format!(
                "package content path is not valid UTF-8: {}",
                path.display()
            ))
        })?;
        components.push(value.nfc().collect::<String>());
    }
    Ok(components.join("/"))
}

pub(super) fn validate_content_hash(hash: &str) -> Result<(), PackageExecutionError> {
    PackageContentHashAlgorithm::parse(hash).map(|_| ())
}

fn is_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
