use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::package_snapshot::PackageSnapshot;

#[derive(Debug, Default, Deserialize)]
struct PackageManifest {
    #[serde(default)]
    exports: HashMap<String, String>,
}

/// Resolve an import string relative to the importing file.
///
/// Returns the path as constructed so callers can compare it with their own
/// `PathBuf::join` result. The module graph canonicalizes its internal keys.
pub fn resolve_import_path(current_file: &Path, import_path: &str) -> Option<PathBuf> {
    let snapshots = PackageSnapshot::acquire_nearest(current_file)
        .ok()
        .flatten()
        .into_iter()
        .collect::<Vec<_>>();
    resolve_import_path_with_snapshots(current_file, import_path, &snapshots)
}

pub(crate) fn resolve_import_path_with_snapshots(
    current_file: &Path,
    import_path: &str,
    package_snapshots: &[PackageSnapshot],
) -> Option<PathBuf> {
    if let Some(module) = import_path
        .strip_prefix("std/")
        .or_else(|| (import_path == "observability").then_some("observability"))
    {
        return super::stdlib::get_stdlib_source(module)
            .map(|_| super::stdlib::stdlib_virtual_path(module));
    }

    let base = current_file.parent().unwrap_or(Path::new("."));
    let mut file_path = base.join(import_path);
    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }
    if file_path.exists() {
        return Some(file_path);
    }

    resolve_package_import(current_file, import_path, package_snapshots)
}

pub(crate) fn acquire_package_snapshots(files: &[PathBuf]) -> Vec<PackageSnapshot> {
    let mut roots = HashSet::new();
    let mut snapshots = Vec::new();
    for file in files {
        let Ok(Some(snapshot)) = PackageSnapshot::acquire_nearest(file) else {
            continue;
        };
        if roots.insert(snapshot.project_root().to_path_buf()) {
            snapshots.push(snapshot);
        }
    }
    snapshots
}

fn resolve_package_import(
    current_file: &Path,
    import_path: &str,
    package_snapshots: &[PackageSnapshot],
) -> Option<PathBuf> {
    let current_file = canonicalize_with_existing_parent(current_file);
    package_snapshots
        .iter()
        .filter(|snapshot| current_file.starts_with(snapshot.project_root()))
        .max_by_key(|snapshot| snapshot.project_root().components().count())
        .and_then(|snapshot| resolve_from_packages_root(snapshot.packages_root(), import_path))
}

fn canonicalize_with_existing_parent(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| path.to_path_buf())
    })
}

fn resolve_from_packages_root(packages_root: &Path, import_path: &str) -> Option<PathBuf> {
    let safe_import_path = safe_package_relative_path(import_path)?;
    let package_name = package_name_from_relative_path(&safe_import_path)?;
    let package_root = packages_root.join(package_name);

    let direct_path = packages_root.join(&safe_import_path);
    if let Some(path) = finalize_package_target(&package_root, &direct_path) {
        return Some(path);
    }

    let export_name = export_name_from_relative_path(&safe_import_path)?;
    let manifest = read_package_manifest(&package_root.join("harn.toml"))?;
    let safe_export_path = safe_package_relative_path(manifest.exports.get(export_name)?)?;
    finalize_package_target(&package_root, &package_root.join(safe_export_path))
}

fn read_package_manifest(path: &Path) -> Option<PackageManifest> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn safe_package_relative_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    let mut saw_component = false;
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                saw_component = true;
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    saw_component.then_some(out)
}

fn package_name_from_relative_path(path: &Path) -> Option<&str> {
    match path.components().next()? {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

fn export_name_from_relative_path(path: &Path) -> Option<&str> {
    let mut components = path.components();
    components.next()?;
    let rest = components.as_path();
    if rest.as_os_str().is_empty() {
        None
    } else {
        rest.to_str()
    }
}

fn target_within_package_root(package_root: &Path, path: PathBuf) -> Option<PathBuf> {
    let root = package_root.canonicalize().ok()?;
    let canonical = path.canonicalize().ok()?;
    (canonical == root || canonical.starts_with(&root)).then_some(path)
}

fn finalize_package_target(package_root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let lib = path.join("lib.harn");
        return if lib.exists() {
            target_within_package_root(package_root, lib)
        } else {
            target_within_package_root(package_root, path.to_path_buf())
        };
    }
    if path.exists() {
        return target_within_package_root(package_root, path.to_path_buf());
    }
    if path.extension().is_none() {
        let mut with_extension = path.to_path_buf();
        with_extension.set_extension("harn");
        if with_extension.exists() {
            return target_within_package_root(package_root, with_extension);
        }
    }
    None
}
