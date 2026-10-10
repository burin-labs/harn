use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerInput {
    pub logical_path: String,
    pub disk_path: PathBuf,
}

// These owners determine both emitted bytecode and the derived import interface
// reused before compilation. Cargo watches the same roots that we fingerprint.
const SOURCE_ROOTS: &[&str] = &[
    "harn-lexer/src",
    "harn-parser/src",
    "harn-ir/src",
    "harn-kernel/src",
    "harn-modules/src",
    "harn-vm/src/chunk",
    "harn-vm/src/bytecode_cache",
];

const SOURCE_FILES: &[&str] = &[
    "bytecode_cache.rs",
    "chunk.rs",
    "compiler.rs",
    "context_manifest.rs",
    "module_artifact.rs",
    "module_source.rs",
];

pub fn compiler_inputs(manifest_dir: &Path) -> Vec<CompilerInput> {
    let crates_dir = manifest_dir.parent().expect("crate dir has a parent");
    let mut inputs = Vec::new();

    for logical_prefix in SOURCE_ROOTS {
        collect_rs_files(
            &crates_dir.join(logical_prefix),
            logical_prefix,
            &mut inputs,
        );
    }

    for file_name in SOURCE_FILES {
        let disk_path = manifest_dir.join("src").join(file_name);
        if disk_path.is_file() {
            inputs.push(CompilerInput {
                logical_path: format!("harn-vm/src/{file_name}"),
                disk_path,
            });
        }
    }

    inputs.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    inputs
}

pub fn watch_roots(manifest_dir: &Path) -> Vec<PathBuf> {
    let crates_dir = manifest_dir.parent().expect("crate dir has a parent");
    SOURCE_ROOTS
        .iter()
        .map(|root| crates_dir.join(root))
        .collect()
}

pub fn fingerprint_inputs(inputs: &[CompilerInput]) -> String {
    let mut hasher = Sha256::new();
    for input in inputs {
        hasher.update(input.logical_path.as_bytes());
        hasher.update([0u8]);
        if let Ok(bytes) = std::fs::read(&input.disk_path) {
            hasher.update(canonical_source_bytes(&bytes));
        }
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn canonical_source_bytes(bytes: &[u8]) -> Vec<u8> {
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

fn collect_rs_files(root: &Path, logical_prefix: &str, out: &mut Vec<CompilerInput>) {
    collect_rs_files_under(root, root, logical_prefix, out);
}

fn collect_rs_files_under(
    root: &Path,
    dir: &Path,
    logical_prefix: &str,
    out: &mut Vec<CompilerInput>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_under(root, &path, logical_prefix, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).unwrap_or(path.as_path());
            out.push(CompilerInput {
                logical_path: format!("{logical_prefix}/{}", normalize_path(relative)),
                disk_path: path,
            });
        }
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}
