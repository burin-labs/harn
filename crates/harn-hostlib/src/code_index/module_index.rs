//! Module membership for languages where a file's module is implied by
//! where the file sits.
//!
//! A Swift target and a Go package are the same shape: a set of files
//! that see each other with no import statement, named from outside by
//! one import that resolves to all of them. [`ModuleIndex`] answers both
//! directions, and the per-language rules for what counts as a module
//! live in [`super::imports_swift`] and [`super::imports_go`].

use std::collections::HashMap;

use super::file_table::FileId;
use super::imports_go::GoModules;
use super::imports_swift::swift_module_root;

/// Module membership for languages where a file's module is implied by
/// where it sits rather than written at the top of the file.
///
/// Swift is the case this exists for. `import Foo` names a build target,
/// so it resolves to every file in that target, and the files of one
/// target already see each other with no import statement. Both are the
/// same underlying fact, and both are O(files) to store here rather than
/// O(files squared) if same-target visibility were written out as one
/// dependency edge per pair.
#[derive(Debug, Default, Clone)]
pub(crate) struct ModuleIndex {
    /// Go module declarations, needed to turn an import path into a
    /// directory. Empty for a workspace with no `go.mod`.
    go_modules: GoModules,
    /// Module root (workspace-relative, e.g. `Sources/Core`) to its files.
    by_root: HashMap<String, Vec<FileId>>,
    /// Module name (`Core`) to the roots declaring it. Two roots can
    /// share a name, so this is a list and lookups union them.
    by_name: HashMap<String, Vec<String>>,
    /// File to the root of the module it belongs to.
    home: HashMap<FileId, String>,
}

impl ModuleIndex {
    /// Rebuild from the workspace path table. O(paths); the caller
    /// refreshes it whenever the path set changes, never per file.
    pub(crate) fn build(path_to_id: &HashMap<String, FileId>) -> Self {
        let mut index = ModuleIndex::default();
        for (path, id) in path_to_id {
            let Some(root) = module_root(path) else {
                continue;
            };
            index.by_root.entry(root.clone()).or_default().push(*id);
            index.home.insert(*id, root);
        }
        for (root, files) in &mut index.by_root {
            files.sort_unstable();
            // Name lookup is the Swift half: a Swift import names a
            // target by name, while a Go import names a directory
            // outright and is looked up through `files_in_root`.
            if root.contains("/Sources/")
                || root.contains("/Tests/")
                || root.starts_with("Sources/")
                || root.starts_with("Tests/")
            {
                let name = root.rsplit('/').next().unwrap_or(root).to_string();
                index.by_name.entry(name).or_default().push(root.clone());
            }
        }
        for roots in index.by_name.values_mut() {
            roots.sort();
        }
        index
    }

    /// The module root(s) named `name`. This, not the file list, is
    /// what the dep graph stores: a module of N files named by M
    /// importers costs M roots here and N times M only when expanded.
    pub(crate) fn roots_named(&self, name: &str) -> &[String] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Expand module roots back into their files.
    pub(crate) fn files_in_roots(&self, roots: &[String]) -> Vec<FileId> {
        let mut out: Vec<FileId> = roots
            .iter()
            .filter_map(|root| self.by_root.get(root))
            .flatten()
            .copied()
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every file in the module(s) named `name`.
    pub(crate) fn files_named(&self, name: &str) -> Vec<FileId> {
        self.files_in_roots(self.roots_named(name))
    }

    /// Attach the workspace's Go module declarations.
    pub(crate) fn with_go_modules(mut self, go_modules: GoModules) -> Self {
        self.go_modules = go_modules;
        self
    }

    /// The workspace directory an import path names, if the workspace
    /// actually holds files there.
    pub(crate) fn go_root_for_import(&self, import_path: &str) -> Option<String> {
        let dir = self.go_modules.directory_for(import_path)?;
        self.by_root.contains_key(&dir).then_some(dir)
    }

    /// Every file in the Go package the import path names.
    pub(crate) fn go_files_for_import(&self, import_path: &str) -> Vec<FileId> {
        let Some(dir) = self.go_root_for_import(import_path) else {
            return Vec::new();
        };
        self.files_in_roots(std::slice::from_ref(&dir))
    }

    /// The root of the module `file` belongs to, if any.
    pub(crate) fn home_of(&self, file: FileId) -> Option<&str> {
        self.home.get(&file).map(String::as_str)
    }

    /// Every other file in `file`'s own module. Empty when the file
    /// belongs to no module the index understands.
    pub(crate) fn siblings_of(&self, file: FileId) -> Vec<FileId> {
        let Some(root) = self.home.get(&file) else {
            return Vec::new();
        };
        self.by_root
            .get(root)
            .map(|files| files.iter().copied().filter(|id| *id != file).collect())
            .unwrap_or_default()
    }
}

/// The module a file belongs to, for the languages where membership is
/// implied by location: a Swift target, or a Go package.
fn module_root(path: &str) -> Option<String> {
    if path.ends_with(".go") {
        // A Go package is exactly its directory. A file at the workspace
        // root belongs to the root package, spelled as the empty string.
        return Some(
            path.rsplit_once('/')
                .map(|(head, _)| head.to_string())
                .unwrap_or_default(),
        );
    }
    swift_module_root(path)
}
