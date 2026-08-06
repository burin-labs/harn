#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const GENERATION: &str = "generation-test";

pub fn create_package_generation(root: &Path) -> PathBuf {
    use harn_modules::package_snapshot::{generation_root, GENERATION_PACKAGES_DIR};

    let packages = generation_root(root, GENERATION).join(GENERATION_PACKAGES_DIR);
    std::fs::create_dir_all(&packages).unwrap();
    packages
}

pub fn publish_package_generation(root: &Path, lock_body: &str) {
    use harn_modules::package_snapshot::{
        generation_root, package_current_path, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE,
    };

    let generation_root = generation_root(root, GENERATION);
    std::fs::write(generation_root.join(GENERATION_LOCK_FILE), lock_body).unwrap();
    std::fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest = PackageGenerationManifest::new(
        GENERATION,
        harn_modules::package_snapshot::package_lock_digest(lock_body.as_bytes()),
    )
    .unwrap();
    std::fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        package_current_path(root),
        toml::to_string_pretty(&PackageGenerationPointer::new(GENERATION).unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::File::create(package_publication_lock_path(root)).unwrap();
}

pub fn package_content_hash(root: &Path) -> String {
    let mut files = Vec::new();
    collect_hashable_files(root, root, &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let contents = std::fs::read(root.join(&relative)).unwrap();
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        hasher.update(hex::encode(Sha256::digest(contents)).as_bytes());
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn collect_hashable_files(root: &Path, cursor: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(cursor).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        let name = entry.file_name();
        if [
            OsStr::new(".git"),
            OsStr::new(".gitignore"),
            OsStr::new(".harn-content-hash"),
            OsStr::new(".harn-package-cache.toml"),
        ]
        .contains(&name.as_os_str())
        {
            continue;
        }
        if file_type.is_dir() {
            collect_hashable_files(root, &path, out);
        } else if file_type.is_file() {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}
