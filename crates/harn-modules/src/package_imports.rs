use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::package_execution::{PackageExecutionError, PackageExecutionGuard};
use crate::package_snapshot::PackageSnapshot;

#[derive(Debug, Default, Deserialize)]
struct PackageManifest {
    #[serde(default)]
    exports: HashMap<String, String>,
}

/// How far an import resolves without consulting installed packages.
///
/// The distinction between `Rejected` and `NotPackage` is load-bearing: a
/// `std/` import that names no real stdlib module resolves to nothing and must
/// NOT fall through to package resolution, or a package could shadow the
/// standard library.
enum LocalResolution {
    /// Resolved without touching installed packages.
    Resolved(PathBuf),
    /// Owned by the stdlib namespace but not a real module. Resolution ends.
    Rejected,
    /// Not a stdlib or relative import; only packages can resolve it.
    NotPackage,
}

/// Resolve everything that does not require a package snapshot.
///
/// Sole owner of the stdlib and relative-path import rules, so the lazy and
/// pre-acquired entry points below cannot drift apart on what counts as local.
fn resolve_local_import(current_file: &Path, import_path: &str) -> LocalResolution {
    if let Some(module) = import_path
        .strip_prefix("std/")
        .or_else(|| (import_path == "observability").then_some("observability"))
    {
        return match super::stdlib::get_stdlib_source(module) {
            Some(_) => LocalResolution::Resolved(super::stdlib::stdlib_virtual_path(module)),
            None => LocalResolution::Rejected,
        };
    }

    let base = current_file.parent().unwrap_or(Path::new("."));
    let mut file_path = base.join(import_path);
    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }
    if file_path.exists() {
        return LocalResolution::Resolved(file_path);
    }

    LocalResolution::NotPackage
}

/// Resolve an import string relative to the importing file.
///
/// Returns the path as constructed so callers can compare it with their own
/// `PathBuf::join` result. The module graph canonicalizes its internal keys.
pub fn resolve_import_path(current_file: &Path, import_path: &str) -> Option<PathBuf> {
    match resolve_local_import(current_file, import_path) {
        LocalResolution::Resolved(path) => Some(path),
        LocalResolution::Rejected => None,
        // Only a package import needs a generation lease, so only a package
        // import pays for one. Acquiring it before the stdlib and relative
        // checks made every `std/...` and every sibling import — nearly all of
        // them — walk its ancestors stat-ing for a package pointer, then open,
        // flock and parse it, and then discard the snapshot unused. That is
        // pure syscall cost on the hottest path in the module graph.
        LocalResolution::NotPackage => {
            let snapshots = PackageSnapshot::acquire_nearest(current_file)
                .ok()
                .flatten()
                .into_iter()
                .collect::<Vec<_>>();
            resolve_package_import(current_file, import_path, &snapshots)
        }
    }
}

pub(crate) fn resolve_import_path_with_snapshots(
    current_file: &Path,
    import_path: &str,
    package_snapshots: &[PackageSnapshot],
) -> Option<PathBuf> {
    match resolve_local_import(current_file, import_path) {
        LocalResolution::Resolved(path) => Some(path),
        LocalResolution::Rejected => None,
        LocalResolution::NotPackage => {
            resolve_package_import(current_file, import_path, package_snapshots)
        }
    }
}

pub fn resolve_import_path_with_snapshot(
    current_file: &Path,
    import_path: &str,
    package_snapshot: &PackageSnapshot,
) -> Option<PathBuf> {
    match resolve_local_import(current_file, import_path) {
        LocalResolution::Resolved(path) => Some(path),
        LocalResolution::Rejected => None,
        // An explicit snapshot is caller-owned resolution authority. Unlike
        // lazy discovery it also covers generation-owned path-package
        // symlinks whose canonical source is outside the project root.
        LocalResolution::NotPackage => {
            resolve_from_packages_root(package_snapshot.packages_root(), import_path)
        }
    }
}

pub fn resolve_import_path_with_guard(
    current_file: &Path,
    import_path: &str,
    guard: &PackageExecutionGuard,
) -> Result<Option<PathBuf>, PackageExecutionError> {
    match resolve_local_import(current_file, import_path) {
        LocalResolution::Resolved(path) => Ok(Some(path)),
        LocalResolution::Rejected => Ok(None),
        LocalResolution::NotPackage => resolve_from_packages_root_with_guard(
            guard.snapshot().packages_root(),
            import_path,
            guard,
        ),
    }
}

/// Acquire one snapshot per DISTINCT project root among `files`.
///
/// Dedupe on the root before acquiring, not after. Acquiring is the expensive
/// half — canonicalize, two shared flocks, two TOML parses, and a re-read plus
/// SHA256 of the lockfile — so acquiring per file and discarding the duplicates
/// made a whole-tree build pay it once per FILE. Every real invocation resolves
/// many files under a single root, so all but one of those was thrown away.
pub(crate) fn acquire_package_snapshots(files: &[PathBuf]) -> Vec<PackageSnapshot> {
    let mut walked_roots = HashSet::new();
    let mut canonical_roots = HashSet::new();
    let mut snapshots = Vec::new();
    for file in files {
        // Cheap: a handful of stats up the ancestors.
        let Some(root) = PackageSnapshot::nearest_project_root(file) else {
            continue;
        };
        if !walked_roots.insert(root.clone()) {
            continue;
        }
        // Expensive: reached at most once per distinct walked root.
        let Ok(Some(snapshot)) = PackageSnapshot::acquire(&root) else {
            continue;
        };
        // `acquire` canonicalizes, so two walked roots that differ only by
        // symlink can still land on one real root. Dedupe on the canonical
        // root as the original did, or such a tree would get two snapshots
        // where it used to get one.
        if canonical_roots.insert(snapshot.project_root().to_path_buf()) {
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

fn resolve_from_packages_root_with_guard(
    packages_root: &Path,
    import_path: &str,
    guard: &PackageExecutionGuard,
) -> Result<Option<PathBuf>, PackageExecutionError> {
    let Some(safe_import_path) = safe_package_relative_path(import_path) else {
        return Ok(None);
    };
    let Some(package_name) = package_name_from_relative_path(&safe_import_path) else {
        return Ok(None);
    };
    let package_root = packages_root.join(package_name);
    let direct_path = packages_root.join(&safe_import_path);
    if let Some(path) = finalize_package_target(&package_root, &direct_path) {
        return Ok(Some(path));
    }

    let Some(export_name) = export_name_from_relative_path(&safe_import_path) else {
        return Ok(None);
    };
    let manifest_path = package_root.join("harn.toml");
    let bytes = guard.verify_entry_source(&manifest_path)?;
    let source = std::str::from_utf8(&bytes).map_err(|error| {
        PackageExecutionError::Invalid(format!(
            "package manifest {} is not valid UTF-8: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest = toml::from_str::<PackageManifest>(source).map_err(|error| {
        PackageExecutionError::Invalid(format!(
            "failed to parse package exports from {}: {error}",
            manifest_path.display()
        ))
    })?;
    let Some(export_path) = manifest.exports.get(export_name) else {
        return Ok(None);
    };
    let Some(safe_export_path) = safe_package_relative_path(export_path) else {
        return Ok(None);
    };
    Ok(finalize_package_target(
        &package_root,
        &package_root.join(safe_export_path),
    ))
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
