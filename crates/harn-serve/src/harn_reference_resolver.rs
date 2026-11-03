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
    // The host index contains workspace files only. References to the
    // standard library or other external modules have no graph endpoint here;
    // omit those edges without discarding the workspace references.
    let references = harn_modules::file_reference_edges(&build.graph, &build.parsed_sources)
        .into_iter()
        .filter_map(|edge| {
            Some(ResolvedHarnReference {
                from_path: relative(&input.root, &edge.from.file)?,
                to_path: relative(&input.root, &edge.to_file)?,
                to_name: edge.to_name,
            })
        })
        .collect();
    Ok(references)
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_hostlib::{BuiltinRegistry, HostlibCapability};
    use harn_vm::VmValue;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn file_graph_projects_repeated_sites_once_and_keeps_distinct_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let a = root.join("a.harn");
        let b = root.join("b.harn");
        let c = root.join("c.harn");
        fs::write(&a, "pub fn run() { 1 }\n").unwrap();
        fs::write(&c, "pub fn other() { 2 }\n").unwrap();
        fs::write(
            &b,
            "import { other } from \"./c\"\nimport { run } from \"./a\"\nfn use_it() {\n  other()\n  run()\n  run()\n}\n",
        )
        .unwrap();
        let files = vec![a, b, c];
        let build = harn_modules::build_for_reference_index(&files, None);
        let full =
            harn_modules::index_references(&build.graph, &build.parsed_sources, &HashSet::new());
        let expected: HashSet<_> = full
            .edges()
            .into_iter()
            .map(|edge| {
                (
                    relative(&root, &edge.from.file).unwrap(),
                    relative(&root, &edge.to_file).unwrap(),
                    edge.to_name,
                )
            })
            .collect();
        let references = resolve(&HarnReferenceInput {
            root,
            files,
            source_overrides: Default::default(),
        })
        .unwrap();
        let projected: HashSet<_> = references
            .iter()
            .map(|edge| (&edge.from_path, &edge.to_path, &edge.to_name))
            .collect();
        let expected: HashSet<_> = expected
            .iter()
            .map(|(from, to, name)| (from, to, name))
            .collect();
        assert_eq!(
            projected, expected,
            "file graph must preserve all resolved targets"
        );
        let count = |from: &str, to: &str, name: &str| {
            references
                .iter()
                .filter(|edge| edge.from_path == from && edge.to_path == to && edge.to_name == name)
                .count()
        };
        assert_eq!(count("b.harn", "a.harn", "run"), 1);
        assert_eq!(count("b.harn", "c.harn", "other"), 1);
        assert_eq!(count("b.harn", "c.harn", "run"), 0);
        let position = |to: &str| {
            references
                .iter()
                .position(|edge| edge.from_path == "b.harn" && edge.to_path == to)
                .unwrap()
        };
        assert!(position("c.harn") < position("a.harn"));
    }

    #[test]
    fn file_graph_uses_unsaved_sources_without_retaining_disk_edges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let a = root.join("a.harn");
        let b = root.join("b.harn");
        let c = root.join("c.harn");
        fs::write(&a, "pub fn run() { 1 }\n").unwrap();
        fs::write(&c, "pub fn other() { 2 }\n").unwrap();
        fs::write(&b, "import { run } from \"./a\"\nfn use_it() { run() }\n").unwrap();
        let mut source_overrides = std::collections::HashMap::new();
        source_overrides.insert(
            b.clone(),
            "import { other } from \"./c\"\nfn use_it() {\n  other()\n  other()\n}\n".to_string(),
        );
        let references = resolve(&HarnReferenceInput {
            root,
            files: vec![a, b, c],
            source_overrides,
        })
        .unwrap();
        let from_b: Vec<_> = references
            .iter()
            .filter(|edge| edge.from_path == "b.harn")
            .collect();
        assert_eq!(
            from_b
                .iter()
                .filter(|edge| edge.to_path == "c.harn" && edge.to_name == "other")
                .count(),
            1
        );
        assert!(!from_b.iter().any(|edge| edge.to_path == "a.harn"));
    }

    #[test]
    fn standard_library_references_do_not_discard_workspace_edges() {
        harn_stdlib::install().expect("the host installs its standard library");
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("local.harn"), "pub fn local() { 1 }\n").unwrap();
        fs::write(
            dir.path().join("use.harn"),
            "import { local } from \"./local\"\nimport { help_requested } from \"std/cli/argparse\"\nfn use_it() { help_requested([]); local() }\n",
        )
        .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let files = vec![root.join("local.harn"), root.join("use.harn")];
        let edges = resolve(&HarnReferenceInput {
            root,
            files,
            source_overrides: Default::default(),
        })
        .expect("external references must not discard workspace references");

        assert!(edges.iter().any(|edge| {
            edge.from_path == "use.harn" && edge.to_path == "local.harn" && edge.to_name == "local"
        }));
        assert!(edges.iter().all(|edge| !edge.to_path.starts_with("<std>")));
    }

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
