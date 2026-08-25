//! Shared ModuleGraph reference index for find-references and call hierarchy.
//!
//! One owner: `harn_modules`. This module only gathers workspace files plus
//! unsaved buffers and asks the graph for the inverse of `definition_of`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::{index_references, DefSite, ReferenceIndex};
use ignore::WalkBuilder;
use tower_lsp::lsp_types::Url;

use crate::document::DocumentState;
use crate::document_kind::DocumentKind;

pub(crate) struct WorkspaceRefs {
    pub index: ReferenceIndex,
    pub graph: harn_modules::ModuleGraph,
}

pub(crate) fn workspace_refs(
    docs: &HashMap<Url, DocumentState>,
    workspace_root: Option<&Path>,
) -> Option<WorkspaceRefs> {
    let mut overrides = HashMap::new();
    let mut seeds = Vec::new();
    let mut unsaved = HashSet::new();
    for (uri, state) in docs {
        if state.kind != DocumentKind::Harn {
            continue;
        }
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        let path = harn_modules::canonical_path(&path);
        overrides.insert(path.clone(), state.source.as_str().to_string());
        seeds.push(path.clone());
        if state.dirty {
            unsaved.insert(path);
        }
    }
    if let Some(root) = workspace_root {
        for path in collect_harn_files(root) {
            let path = harn_modules::canonical_path(&path);
            if !seeds.iter().any(|seed| seed == &path) {
                seeds.push(path);
            }
        }
    }
    if seeds.is_empty() {
        return None;
    }
    let build = harn_modules::build_for_reference_index(&seeds, Some(&overrides));
    let index = index_references(&build.graph, &build.parsed_sources, &unsaved);
    Some(WorkspaceRefs {
        index,
        graph: build.graph,
    })
}

pub(crate) fn definition_at(
    workspace: &WorkspaceRefs,
    file: &Path,
    name: &str,
    offset: usize,
) -> Option<DefSite> {
    workspace.index.definition_at(file, name, offset)
}

fn collect_harn_files(root: &Path) -> Vec<PathBuf> {
    let mut walker = WalkBuilder::new(root);
    // Workspace intelligence uses the same committed, machine-independent
    // candidate set as Harn's search and CLI surfaces. If the built-in layer
    // cannot be configured, walking remains a visible over-inclusion rather
    // than silently producing an empty index.
    let _ = harn_vm::ignore_policy::configure(
        &mut walker,
        root,
        harn_vm::ignore_policy::IgnorePolicy::Project,
        true,
    );
    walker
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("harn"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use harn_modules::ReferenceIndex;
    use tower_lsp::lsp_types::Url;

    use crate::document::DocumentState;

    fn write_same_named_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let exported = root.join("exported.harn");
        let importer = root.join("importer.harn");
        let local = root.join("local.harn");
        fs::write(&exported, "pub fn run() { 1 }\n").unwrap();
        fs::write(
            &importer,
            "import { run } from \"./exported\"\nfn helper() { run() }\n",
        )
        .unwrap();
        fs::write(&local, "fn run() { 2 }\nfn helper() { run() }\n").unwrap();
        (exported, importer, local)
    }

    fn open_docs(paths: &[&Path]) -> HashMap<Url, DocumentState> {
        let mut docs = HashMap::new();
        for path in paths {
            let uri = Url::from_file_path(path).unwrap();
            let source = fs::read_to_string(path).unwrap();
            docs.insert(uri, DocumentState::new(source));
        }
        docs
    }

    fn edge_triples(index: &ReferenceIndex) -> Vec<(String, String, String)> {
        let mut edges: Vec<_> = index
            .edges()
            .into_iter()
            .map(|edge| {
                (
                    file_name(&edge.from.file),
                    file_name(&edge.to_file),
                    edge.to_name,
                )
            })
            .collect();
        edges.sort();
        edges
    }

    fn file_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn lsp_refs_do_not_collapse_same_named_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let (exported, importer, local) = write_same_named_fixture(tmp.path());
        let docs = open_docs(&[&exported, &importer, &local]);
        let workspace = workspace_refs(&docs, Some(tmp.path())).expect("workspace index");
        assert!(
            workspace
                .index
                .files
                .iter()
                .any(
                    |path| path.file_name().and_then(|name| name.to_str()) == Some("importer.harn")
                ),
            "cross-file control: importer must have been walked"
        );

        let exported_offset = fs::read_to_string(&exported).unwrap().find("run").unwrap();
        let local_offset = fs::read_to_string(&local).unwrap().find("run").unwrap();
        let exported_def =
            definition_at(&workspace, &exported, "run", exported_offset).expect("exported run");
        let local_def = definition_at(&workspace, &local, "run", local_offset).expect("local run");
        assert_ne!(exported_def.file, local_def.file);

        let exported_files: HashSet<_> = workspace
            .index
            .references_to(&exported_def)
            .into_iter()
            .map(|site| file_name(&site.file))
            .collect();
        assert!(
            exported_files.contains("exported.harn"),
            "{exported_files:?}"
        );
        assert!(
            exported_files.contains("importer.harn"),
            "{exported_files:?}"
        );
        assert!(
            !exported_files.contains("local.harn"),
            "local run must not collapse into exported run: {exported_files:?}"
        );

        let local_files: HashSet<_> = workspace
            .index
            .references_to(&local_def)
            .into_iter()
            .map(|site| file_name(&site.file))
            .collect();
        assert!(local_files.contains("local.harn"), "{local_files:?}");
        assert!(
            !local_files.contains("importer.harn") && !local_files.contains("exported.harn"),
            "imported run must not collapse into local run: {local_files:?}"
        );
    }

    #[test]
    fn lsp_refs_prefer_a_lexical_shadow_over_same_named_import() {
        let tmp = tempfile::tempdir().unwrap();
        let exported = tmp.path().join("exported.harn");
        let importer = tmp.path().join("importer.harn");
        fs::write(&exported, "pub fn run() { 1 }\n").unwrap();
        let importer_source =
            "import { run } from \"./exported\"\nfn helper() {\n  let run = 2\n  run\n}\n";
        fs::write(&importer, importer_source).unwrap();

        let docs = open_docs(&[&exported, &importer]);
        let workspace = workspace_refs(&docs, Some(tmp.path())).expect("workspace index");
        let local_use = importer_source.rfind("run").expect("local run use");
        let local_def = definition_at(&workspace, &importer, "run", local_use + 1)
            .expect("local shadow definition");
        assert_eq!(
            local_def.file,
            harn_modules::canonical_path(&importer),
            "the cursor must resolve through lexical binding identity"
        );

        let exported_def = workspace
            .graph
            .definition_of(&exported, "run")
            .expect("exported run");
        assert!(
            workspace
                .index
                .references_to(&exported_def)
                .iter()
                .all(|site| site.file != harn_modules::canonical_path(&importer)),
            "the local shadow must not enter the imported definition's references"
        );
    }

    #[test]
    fn lsp_and_disk_graph_agree_on_the_same_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let (exported, importer, local) = write_same_named_fixture(tmp.path());
        let docs = open_docs(&[&exported, &importer, &local]);
        let workspace = workspace_refs(&docs, Some(tmp.path())).expect("workspace index");
        assert!(
            !workspace.index.has_unsaved_buffers,
            "opened files match disk after parse, so the answer is not an unsaved overlay"
        );

        let disk = harn_modules::build_for_reference_index(&[exported, importer, local], None);
        let disk_index =
            harn_modules::index_references(&disk.graph, &disk.parsed_sources, &HashSet::new());
        assert_eq!(
            edge_triples(&workspace.index),
            edge_triples(&disk_index),
            "LSP and harn graph must answer from the same inverse of definition_of"
        );
    }

    #[test]
    fn workspace_refs_use_the_shared_project_ignore_policy() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join("included.harn"), "fn included() { 1 }\n").unwrap();
        fs::write(
            tmp.path().join("git_ignored.harn"),
            "fn git_ignored() { 1 }\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("agent_ignored.harn"),
            "fn agent_ignored() { 1 }\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("dot_ignored.harn"),
            "fn dot_ignored() { 1 }\n",
        )
        .unwrap();
        fs::write(tmp.path().join(".gitignore"), "git_ignored.harn\n").unwrap();
        fs::write(tmp.path().join(".agentignore"), "agent_ignored.harn\n").unwrap();
        fs::write(tmp.path().join(".ignore"), "dot_ignored.harn\n").unwrap();

        let workspace = workspace_refs(&HashMap::new(), Some(tmp.path())).expect("workspace index");
        let indexed: HashSet<_> = workspace
            .index
            .files
            .iter()
            .map(|path| file_name(path))
            .collect();

        assert!(indexed.contains("included.harn"), "{indexed:?}");
        assert!(indexed.contains("dot_ignored.harn"), "{indexed:?}");
        assert!(!indexed.contains("git_ignored.harn"), "{indexed:?}");
        assert!(!indexed.contains("agent_ignored.harn"), "{indexed:?}");
    }
}
