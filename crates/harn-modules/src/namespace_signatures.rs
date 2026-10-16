//! Resolve namespace members against their defining module, then lower their
//! call contracts through the shared parser projection.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub use harn_parser::NamespaceMemberSignature;
use harn_parser::SNode;

use crate::{callable_decl_name, normalize_path, ModuleGraph};

impl ModuleGraph {
    pub(crate) fn namespace_member_signatures(
        &self,
        importer: &Path,
        module_path: &Path,
        member_names: &[String],
    ) -> BTreeMap<String, NamespaceMemberSignature> {
        let mut out = BTreeMap::new();
        for name in member_names {
            let mut visited = HashSet::new();
            let sibling_decl = crate::sibling_module_access(importer, module_path)
                .then(|| self.modules.get(&normalize_path(module_path)))
                .flatten()
                .filter(|module| module.sibling_exports.contains(name))
                .and_then(|module| {
                    module
                        .callable_declarations
                        .iter()
                        .find(|decl| callable_decl_name(decl) == Some(name.as_str()))
                        .cloned()
                });
            let Some(decl) = sibling_decl
                .or_else(|| self.find_exported_callable_decl(module_path, name, &mut visited))
            else {
                continue;
            };
            let origin = self
                .export_definition_of(module_path, name)
                .map_or_else(|| normalize_path(module_path), |site| site.file);
            let signature =
                harn_parser::namespace_signatures::lower_callable_signature(&decl, |name| {
                    self.find_exported_type_decl(&origin, name, &mut HashSet::new())
                        .or_else(|| self.local_type_decl(&origin, name))
                });
            if let Some(signature) = signature {
                out.insert(name.clone(), signature);
            }
        }
        out
    }

    fn local_type_decl(&self, module_path: &Path, name: &str) -> Option<SNode> {
        let module = self
            .modules
            .get(module_path)
            .or_else(|| self.modules.get(&normalize_path(module_path)))?;
        module
            .type_declarations
            .iter()
            .find(|decl| crate::type_decl_name(decl) == Some(name))
            .cloned()
    }
}
