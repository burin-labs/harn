//! Embedded stdlib source access for the module graph.

use std::path::{Component, Path, PathBuf};

/// Return the embedded stdlib source for `module` (the part after
/// `std/`), or `None` if no stdlib module with that name exists.
pub(crate) fn get_stdlib_source(module: &str) -> Option<&'static str> {
    harn_stdlib::get_stdlib_source(module)
}

/// Rust builtins exported through an embedded stdlib module's public surface.
pub(crate) fn builtin_reexports(module: &str) -> &'static [&'static str] {
    harn_stdlib::builtin_reexports(module)
}

/// Sentinel path used to key embedded stdlib modules in the module
/// graph. Real files never resolve to this path, so collisions are
/// impossible.
pub(crate) fn stdlib_virtual_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<std>/{module}"))
}

pub(crate) fn is_stdlib_virtual_path(path: &Path) -> bool {
    path.to_str().is_some_and(|path| path.starts_with("<std>/"))
}

/// The canonical form of a virtual stdlib path, or `None` for an ordinary one.
///
/// Module-graph paths (`<std>/module`) are canonical as written. Every other
/// embedded-stdlib spelling, such as the runtime's `<stdlib>/module.harn` or a
/// virtual parent directory, cannot exist on disk: canonicalizing it always
/// fails and is never memoized, so module sibling checks would call `realpath`
/// on every import. It resolves lexically, exactly as that failure path does.
pub(crate) fn canonical_virtual_path(path: &Path) -> Option<PathBuf> {
    if is_stdlib_virtual_path(path) {
        return Some(path.to_path_buf());
    }
    is_embedded_stdlib_spelling(path).then(|| crate::manifest_walk::normalize_lexically(path))
}

/// Whether `path` lies under either virtual stdlib root: the module graph's
/// `<std>` or the runtime's `<stdlib>`, including the roots themselves.
pub(crate) fn is_embedded_stdlib_spelling(path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    ["<std>", "<stdlib>"].iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Resolve a relative import owned by an embedded stdlib module.
///
/// Embedded modules use virtual paths rather than mirror files on disk. Keep
/// their sibling-import semantics identical to ordinary modules while refusing
/// any path that escapes the stdlib namespace.
pub(crate) fn relative_stdlib_module(current_file: &Path, import_path: &str) -> Option<String> {
    let current = current_file.to_str()?.strip_prefix("<std>/")?;
    let mut parts = current.split('/').collect::<Vec<_>>();
    parts.pop()?;

    for component in Path::new(import_path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    let last = parts.last_mut()?;
    *last = last.strip_suffix(".harn").unwrap_or(last);
    let module = parts.join("/");
    get_stdlib_source(&module).map(|_| module)
}
