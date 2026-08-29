use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use harn_hostlib::code_index::{HarnReferenceInput, HarnReferenceResolver, ResolvedHarnReference};

/// Build the shared `harn-modules` to hostlib reference projection.
pub fn resolver() -> HarnReferenceResolver {
    Arc::new(resolve)
}

pub(crate) fn install(vm: &mut harn_vm::Vm) {
    let _ = harn_hostlib::install_default_with_embed_and_harn_reference_resolver(
        vm,
        harn_hostlib::embed::EmbedCapability::from_env(),
        Some(resolver()),
    );
}

fn resolve(input: &HarnReferenceInput) -> Result<Vec<ResolvedHarnReference>, String> {
    if input.files.is_empty() {
        return Ok(Vec::new());
    }
    let build = harn_modules::build_for_reference_index(
        &input.files,
        (!input.source_overrides.is_empty()).then_some(&input.source_overrides),
    );
    let unsaved: HashSet<_> = input.source_overrides.keys().cloned().collect();
    let index = harn_modules::index_references(&build.graph, &build.parsed_sources, &unsaved);
    index
        .edges()
        .into_iter()
        .map(|edge| {
            Ok(ResolvedHarnReference {
                from_path: relative(&input.root, &edge.from.file)?,
                to_path: relative(&input.root, &edge.to_file)?,
                to_name: edge.to_name,
            })
        })
        .collect()
}

fn relative(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            format!(
                "resolved Harn reference path {} is outside workspace {}",
                path.display(),
                root.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_hostlib::{BuiltinRegistry, HostlibCapability};
    use harn_vm::VmValue;
    use std::fs;

    #[test]
    fn shared_resolver_preserves_ownership_and_lexical_shadowing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.harn"), "pub fn run() { 1 }\n").unwrap();
        fs::write(dir.path().join("b.harn"), "pub fn run() { 2 }\n").unwrap();
        fs::write(
            dir.path().join("importer.harn"),
            "import { run } from \"./a\"\nfn use_it() { run() }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("shadowed.harn"),
            "import { run } from \"./a\"\nfn use_it() { let run = 2; run }\n",
        )
        .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let mut files = vec!["a.harn", "b.harn", "importer.harn", "shadowed.harn"]
            .into_iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        files.sort();
        let edges = resolve(&HarnReferenceInput {
            root,
            files,
            source_overrides: Default::default(),
        })
        .unwrap();

        assert!(edges.iter().any(|edge| {
            edge.from_path == "importer.harn" && edge.to_path == "a.harn" && edge.to_name == "run"
        }));
        assert!(!edges
            .iter()
            .any(|edge| { edge.from_path == "importer.harn" && edge.to_path == "b.harn" }));
        assert!(!edges.iter().any(|edge| {
            edge.from_path == "shadowed.harn" && edge.to_path == "a.harn" && edge.to_name == "run"
        }));

        let capability = harn_hostlib::code_index::CodeIndexCapability::new()
            .with_harn_reference_resolver(resolver());
        let mut registry = BuiltinRegistry::new();
        capability.register_builtins(&mut registry);
        let invoke = |name: &str, fields: &[(&str, VmValue)]| {
            let mut payload: harn_vm::value::DictMap = Default::default();
            for (key, value) in fields {
                payload.insert(harn_vm::value::intern_key(key), value.clone());
            }
            (registry.find(name).unwrap().handler)(&[VmValue::dict(payload)]).unwrap()
        };
        invoke(
            "hostlib_code_index_rebuild",
            &[(
                "root",
                VmValue::String(arcstr::ArcStr::from(dir.path().to_string_lossy().as_ref())),
            )],
        );
        let result = invoke(
            "hostlib_code_index_cypher",
            &[(
                "query",
                VmValue::String(arcstr::ArcStr::from(
                    "MATCH (m:Module)-[:REFS]->(f:Function {name: 'run'}) RETURN m.path AS source, f.path AS target",
                )),
            )],
        );
        let VmValue::Dict(result) = result else {
            panic!("Cypher result must be a dictionary");
        };
        let VmValue::List(rows) = result.get("rows").unwrap() else {
            panic!("Cypher rows must be a list");
        };
        let ownership = rows
            .iter()
            .map(|row| {
                let VmValue::Dict(row) = row else {
                    panic!("Cypher row must be a dictionary");
                };
                let VmValue::String(source) = row.get("source").unwrap() else {
                    panic!("source must be a string");
                };
                let VmValue::String(target) = row.get("target").unwrap() else {
                    panic!("target must be a string");
                };
                (source.to_string(), target.to_string())
            })
            .collect::<Vec<_>>();
        assert!(ownership.contains(&("importer.harn".into(), "a.harn".into())));
        assert!(!ownership.contains(&("importer.harn".into(), "b.harn".into())));
        assert!(!ownership.contains(&("shadowed.harn".into(), "a.harn".into())));
    }
}
