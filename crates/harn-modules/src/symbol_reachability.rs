//! Graph-wide export demand for closed programs.
//!
//! Module discovery remains effect-conservative: every syntactic import edge
//! stays in the closed graph and every reached initializer still executes. The
//! lattice only controls which public symbols and callable bytecode a closed
//! artifact must retain.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harn_parser::{namespace_import_demands, NamespaceDemand};
use serde::{Deserialize, Serialize};

use crate::{canonical_path, ModuleGraphBuild};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportDemand {
    #[default]
    InitializationOnly,
    Members(BTreeSet<String>),
    WholeNamespace,
}

impl ExportDemand {
    pub fn add_members<I>(&mut self, members: I) -> bool
    where
        I: IntoIterator<Item = String>,
    {
        match self {
            Self::WholeNamespace => false,
            Self::InitializationOnly => {
                let members = members.into_iter().collect::<BTreeSet<_>>();
                let changed = !members.is_empty();
                *self = Self::Members(members);
                changed
            }
            Self::Members(current) => {
                let before = current.len();
                current.extend(members);
                current.len() != before
            }
        }
    }

    pub fn widen(&mut self) -> bool {
        if matches!(self, Self::WholeNamespace) {
            false
        } else {
            *self = Self::WholeNamespace;
            true
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        match self {
            Self::InitializationOnly => false,
            Self::Members(members) => members.contains(name),
            Self::WholeNamespace => true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSymbolDemand {
    pub exports: ExportDemand,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolReachability {
    pub modules: BTreeMap<PathBuf, ModuleSymbolDemand>,
}

impl SymbolReachability {
    pub fn demand_for(&self, path: &Path) -> ExportDemand {
        self.modules
            .get(&canonical_path(path))
            .map(|demand| demand.exports.clone())
            .unwrap_or(ExportDemand::WholeNamespace)
    }
}

/// Resolve export demand across a closed graph to a monotone fixpoint.
///
/// Selective and namespace imports retain exact members when structural
/// analysis proves them. Wildcard flattening and every unresolved edge widen
/// the target to its complete namespace. Import edges and module initializers
/// are never removed by this pass.
pub fn closed_program_reachability(
    build: &ModuleGraphBuild,
    entrypoint: &Path,
) -> SymbolReachability {
    let mut modules = build
        .graph
        .module_paths()
        .into_iter()
        .map(|path| (canonical_path(&path), ModuleSymbolDemand::default()))
        .collect::<BTreeMap<_, _>>();
    // The entry is executed as a chunk rather than imported, but treating its
    // public surface as whole keeps future typed-entry selection conservative.
    modules
        .entry(canonical_path(entrypoint))
        .or_default()
        .exports
        .widen();

    loop {
        let mut changed = false;
        for path in build.graph.module_paths() {
            let path = canonical_path(&path);
            let namespace_demands = build
                .parsed_sources
                .get(&path)
                .map(|parsed| namespace_import_demands(&parsed.program))
                .unwrap_or_default();
            for import in build.graph.imports_for_module(&path) {
                let Some(target) = import.resolved_path.map(|path| canonical_path(&path)) else {
                    // A malformed graph cannot produce a specialized artifact;
                    // keeping every known module whole is the safe diagnostic
                    // fallback until the caller rejects the unresolved import.
                    for demand in modules.values_mut() {
                        changed |= demand.exports.widen();
                    }
                    continue;
                };
                let target_demand = modules.entry(target).or_default();
                if let Some(alias) = import.namespace_alias {
                    match namespace_demands.get(&alias) {
                        Some(NamespaceDemand::Members(members)) => {
                            changed |= target_demand.exports.add_members(members.iter().cloned());
                        }
                        Some(NamespaceDemand::Whole) | None => {
                            changed |= target_demand.exports.widen();
                        }
                    }
                } else if let Some(names) = import.selective_names {
                    changed |= target_demand.exports.add_members(names);
                } else {
                    changed |= target_demand.exports.widen();
                }
            }
        }
        if !changed {
            break;
        }
    }
    SymbolReachability { modules }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn namespace_members_union_across_graph_and_alias_escape_widens() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library.harn");
        let exact = dir.path().join("exact.harn");
        let whole = dir.path().join("whole.harn");
        let entry = dir.path().join("entry.harn");
        fs::write(&library, "pub fn kept() { 1 }\npub fn other() { 2 }").unwrap();
        fs::write(
            &exact,
            "import * as lib from \"./library.harn\"\npub fn value() { lib.kept() }",
        )
        .unwrap();
        fs::write(
            &whole,
            "import * as lib from \"./library.harn\"\npub fn value() { lib }",
        )
        .unwrap();
        fs::write(
            &entry,
            "import { value } from \"./exact.harn\"\nfn main() { value() }",
        )
        .unwrap();

        let build = crate::build_closed_program(std::slice::from_ref(&entry));
        assert_eq!(
            closed_program_reachability(&build, &entry).demand_for(&library),
            ExportDemand::Members(BTreeSet::from(["kept".to_string()]))
        );

        fs::write(
            &entry,
            "import { value } from \"./whole.harn\"\nfn main() { value() }",
        )
        .unwrap();
        let build = crate::build_closed_program(std::slice::from_ref(&entry));
        assert_eq!(
            closed_program_reachability(&build, &entry).demand_for(&library),
            ExportDemand::WholeNamespace
        );
    }
}
