use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerInput {
    pub logical_path: String,
    pub disk_path: PathBuf,
}

pub fn compiler_inputs(manifest_dir: &Path) -> Vec<CompilerInput> {
    let crates_dir = manifest_dir.parent().expect("crate dir has a parent");
    let mut inputs = Vec::new();

    let roots = [
        (crates_dir.join("harn-lexer").join("src"), "harn-lexer/src"),
        (
            crates_dir.join("harn-parser").join("src"),
            "harn-parser/src",
        ),
        (crates_dir.join("harn-ir").join("src"), "harn-ir/src"),
        (
            manifest_dir.join("src").join("compiler"),
            "harn-vm/src/compiler",
        ),
    ];
    for (root, logical_prefix) in roots {
        collect_rs_files(&root, logical_prefix, &mut inputs);
    }

    let chunk = manifest_dir.join("src").join("chunk.rs");
    if chunk.is_file() {
        inputs.push(CompilerInput {
            logical_path: "harn-vm/src/chunk.rs".to_string(),
            disk_path: chunk,
        });
    }

    inputs.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    inputs
}

pub fn watch_roots(manifest_dir: &Path) -> Vec<PathBuf> {
    let crates_dir = manifest_dir.parent().expect("crate dir has a parent");
    vec![
        crates_dir.join("harn-lexer").join("src"),
        crates_dir.join("harn-parser").join("src"),
        crates_dir.join("harn-ir").join("src"),
        manifest_dir.join("src").join("compiler"),
    ]
}

pub fn fingerprint_inputs(inputs: &[CompilerInput]) -> String {
    let mut hasher = Sha256::new();
    for input in inputs {
        hasher.update(input.logical_path.as_bytes());
        hasher.update([0u8]);
        if let Ok(bytes) = std::fs::read(&input.disk_path) {
            hasher.update(&bytes);
        }
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
