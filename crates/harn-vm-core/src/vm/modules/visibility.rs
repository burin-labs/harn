//! Import path resolution and runtime sibling visibility.

use std::path::{Path, PathBuf};

use super::Vm;

pub fn resolve_module_import_path(base: &Path, path: &str) -> PathBuf {
    let synthetic_current_file = base.join("__harn_import_base__.harn");
    if let Some(resolved) = harn_modules::resolve_import_path(&synthetic_current_file, path) {
        return resolved;
    }

    let mut file_path = base.join(path);
    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }
    file_path
}

impl Vm {
    pub(super) fn sibling_import_allowed(&self, target: &Path) -> bool {
        if let Some(importer) = self.imported_paths.last() {
            return harn_modules::sibling_module_access(importer, target);
        }
        self.source_dir
            .as_deref()
            .is_some_and(|directory| harn_modules::sibling_directory_access(directory, target))
    }
}
