//! The resolved-directory visibility rule shared by static and runtime imports.

use std::path::Path;

use crate::{normalize_path, DefKind, ModuleGraph};

/// A sibling export may cross exactly one resolved module-directory seam.
pub fn sibling_module_access(importer: &Path, target: &Path) -> bool {
    normalize_path(importer)
        .parent()
        .is_some_and(|directory| sibling_directory_access(directory, target))
}

/// Runtime projection when the importer directory is already known.
pub fn sibling_directory_access(importer_directory: &Path, target: &Path) -> bool {
    normalize_path(target)
        .parent()
        .is_some_and(|directory| normalize_path(importer_directory) == directory)
}

impl ModuleGraph {
    /// Exported symbol names for `file`, sorted alphabetically.
    pub fn exports_for_module(&self, file: &Path) -> Vec<String> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };
        let mut exports: Vec<String> = module.exports.iter().cloned().collect();
        exports.sort();
        exports
    }

    /// Names the importer may bind from a target module. This never changes
    /// the target's public export projection.
    pub fn exports_for_import(&self, importer: &Path, target: &Path) -> Vec<String> {
        let importer = normalize_path(importer);
        let target = normalize_path(target);
        let Some(module) = self.modules.get(&target) else {
            return Vec::new();
        };
        let mut names = module.exports.clone();
        if sibling_module_access(&importer, &target) {
            names.extend(module.sibling_exports.iter().cloned());
        }
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        names
    }

    pub(crate) fn exported_kind_for_import(
        &self,
        importer: &Path,
        target: &Path,
        name: &str,
    ) -> Option<DefKind> {
        if sibling_module_access(importer, target) {
            let target = normalize_path(target);
            if let Some(module) = self.modules.get(&target) {
                if module.sibling_exports.contains(name) {
                    return module
                        .declarations
                        .get(name)
                        .map(|declaration| declaration.kind);
                }
            }
        }
        self.exported_kind(target, name)
    }
}
