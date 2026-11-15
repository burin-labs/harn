use super::*;
use crate::package_snapshot::{
    generation_root, package_current_path, package_lock_digest, package_publication_lock_path,
    PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
    GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
};
use std::fs;
use std::sync::Arc;

fn write(root: &Path, relative: &str, source: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
    path
}

fn local_graph(root: &Path) -> (ModuleGraph, PathBuf) {
    write(root, "types.harn", "pub type Item = {value: int}\n");
    write(root, "helper.harn", "pub fn helper() -> int { return 1 }\n");
    let entry = write(
        root,
        "entry.harn",
        "import { Item } from \"./types\"\nimport { helper } from \"./helper\"\npub fn echo(value: Item) -> Item { helper(); return value }\n",
    );
    (crate::build(std::slice::from_ref(&entry)), entry)
}

fn publish_packages(root: &Path, generation: &str, aliases: &[&str]) -> PathBuf {
    write(root, "harn.toml", "[package]\nname = \"consumer\"\n");
    let generation_root = generation_root(root, generation);
    let packages = generation_root.join(GENERATION_PACKAGES_DIR);
    fs::create_dir_all(&packages).unwrap();
    let mut lock = "version = 4\n".to_owned();
    for alias in aliases {
        lock.push_str(&format!("[[package]]\nname = \"{alias}\"\n"));
    }
    fs::write(generation_root.join(GENERATION_LOCK_FILE), &lock).unwrap();
    fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest =
        PackageGenerationManifest::new(generation, package_lock_digest(lock.as_bytes())).unwrap();
    fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    let pointer = PackageGenerationPointer::new(generation).unwrap();
    fs::write(
        package_current_path(root),
        toml::to_string(&pointer).unwrap(),
    )
    .unwrap();
    fs::File::create(package_publication_lock_path(root)).unwrap();
    let snapshot = crate::package_snapshot::PackageSnapshot::acquire(root)
        .expect("published snapshot is valid")
        .expect("published snapshot is present");
    assert_eq!(snapshot.package_names().len(), aliases.len());
    packages
}

fn package_entry(root: &Path, aliases: &[&str]) -> PathBuf {
    let mut source = String::new();
    for alias in aliases {
        source.push_str(&format!("import \"{alias}/types\"\n"));
    }
    source.push_str("pub fn echo() -> int { return 1 }\n");
    write(root, "entry.harn", &source)
}

#[test]
fn relocated_complete_graph_has_same_documents_and_private_physical_lookup() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (graph_a, entry_a) = local_graph(a.path());
    let (graph_b, entry_b) = local_graph(b.path());
    let documents_a = graph_a.source_documents(&entry_a).unwrap();
    let documents_b = graph_b.source_documents(&entry_b).unwrap();
    let names_a = documents_a.documents.values().collect::<BTreeSet<_>>();
    let names_b = documents_b.documents.values().collect::<BTreeSet<_>>();
    assert_eq!(names_a, names_b);
    assert_eq!(
        names_a.len(),
        3,
        "include callable-only imports, not just type sites"
    );
    let entry_a = entry_a.canonicalize().unwrap();
    assert_eq!(
        documents_a.document_for(&entry_a),
        Some("harn:source/entry.harn")
    );
    assert_eq!(
        documents_a.resolve("harn:source/entry.harn"),
        Some(entry_a.as_path())
    );
    assert_ne!(
        documents_a.resolve("harn:source/entry.harn"),
        documents_b.resolve("harn:source/entry.harn")
    );
    assert_eq!(
        documents_a
            .physical_paths()
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>(),
        graph_a.module_paths().into_iter().collect::<BTreeSet<_>>()
    );
    let unknown = write(a.path(), "unimported.harn", "pub fn private() {}\n");
    assert!(documents_a.document_for(&unknown).is_none());
    assert!(documents_a.resolve("harn:source/unimported.harn").is_none());
    assert_eq!(
        graph_a.source_documents(&unknown).unwrap_err(),
        SourceDocumentError::UnknownEntry
    );
}

#[test]
fn package_generation_and_machine_roots_do_not_enter_document_identity() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for (root, generation) in [(a.path(), "generation-a"), (b.path(), "generation-b")] {
        let packages = publish_packages(root, generation, &["dependency"]);
        write(
            &packages,
            "dependency/types.harn",
            "pub type Item = {value: int}\n",
        );
    }
    let entry_a = package_entry(a.path(), &["dependency"]);
    let entry_b = package_entry(b.path(), &["dependency"]);
    let graph_a = crate::build(std::slice::from_ref(&entry_a));
    let graph_b = crate::build(std::slice::from_ref(&entry_b));
    assert_eq!(
        graph_a._package_snapshots.len(),
        1,
        "known package snapshot"
    );
    let documents_a = graph_a.source_documents(&entry_a).unwrap();
    let documents_b = graph_b.source_documents(&entry_b).unwrap();
    let physical_a = graph_a._package_snapshots[0]
        .packages_root()
        .join("dependency/types.harn")
        .canonicalize()
        .unwrap();
    let physical_b = graph_b._package_snapshots[0]
        .packages_root()
        .join("dependency/types.harn")
        .canonicalize()
        .unwrap();
    let document = documents_a
        .document_for(&physical_a)
        .expect("known package");
    assert!(document.starts_with("harn:package/dependency/"));
    assert!(document.ends_with("/types.harn"));
    assert_ne!(physical_a, physical_b);
    assert_eq!(documents_a.resolve(document), Some(physical_a.as_path()));
    assert_eq!(documents_b.document_for(&physical_b), Some(document));
    assert_eq!(
        documents_a.documents.values().collect::<BTreeSet<_>>(),
        documents_b.documents.values().collect::<BTreeSet<_>>()
    );
}

#[cfg(unix)]
#[test]
fn path_package_symlink_keeps_package_identity_outside_project() {
    let projects = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    let types = write(
        dependency.path(),
        "types.harn",
        "pub type Item = {value: int}\n",
    );
    let packages = publish_packages(projects.path(), "generation-path", &["dependency"]);
    std::os::unix::fs::symlink(dependency.path(), packages.join("dependency")).unwrap();
    let entry = package_entry(projects.path(), &["dependency"]);
    let graph = crate::build(std::slice::from_ref(&entry));
    let documents = graph.source_documents(&entry).unwrap();
    let physical = types.canonicalize().unwrap();
    assert!(!physical.starts_with(projects.path()));
    let document = documents
        .document_for(&physical)
        .expect("known path package");
    assert!(document.starts_with("harn:package/dependency/"));
    assert_eq!(documents.resolve(document), Some(physical.as_path()));
}

#[cfg(unix)]
#[test]
fn multiple_aliases_for_one_physical_package_share_one_identity() {
    let project = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    write(
        dependency.path(),
        "types.harn",
        "pub type Item = {value: int}\n",
    );
    let packages = publish_packages(project.path(), "generation-aliases", &["first", "second"]);
    for alias in ["first", "second"] {
        std::os::unix::fs::symlink(dependency.path(), packages.join(alias)).unwrap();
    }
    let entry = package_entry(project.path(), &["first", "second"]);
    let graph = crate::build(std::slice::from_ref(&entry));
    let documents = graph.source_documents(&entry).unwrap();
    let physical = dependency.path().join("types.harn").canonicalize().unwrap();
    let document = documents
        .document_for(&physical)
        .expect("shared canonical package");
    assert!(document.starts_with("harn:package/first/"));
    assert_eq!(documents.physical_paths().count(), 2);
    package_entry(project.path(), &["second", "first"]);
    let reordered = crate::build(std::slice::from_ref(&entry))
        .source_documents(&entry)
        .unwrap();
    assert_eq!(reordered.document_for(&physical), Some(document));
}

#[test]
fn same_alias_in_distinct_scopes_has_distinct_portable_documents() {
    fn scoped(root: &Path, generation: &str) -> BTreeSet<String> {
        let mut entries = Vec::new();
        for scope in ["left", "right"] {
            let root = root.join(scope);
            let packages = publish_packages(&root, generation, &["dependency"]);
            write(
                &packages,
                "dependency/types.harn",
                "pub type Item = {value: int}\n",
            );
            entries.push(package_entry(&root, &["dependency"]));
        }
        let graph = crate::build(&entries);
        assert_eq!(graph._package_snapshots.len(), 2);
        let documents = graph.source_documents(&entries[0]).unwrap();
        let package_names = documents
            .documents
            .values()
            .filter(|name| name.starts_with("harn:package/dependency/"))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            package_names.len(),
            2,
            "two physical dependency scopes stay distinct"
        );
        package_names
    }
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assert_eq!(
        scoped(a.path(), "generation-old"),
        scoped(b.path(), "generation-new")
    );
}

#[test]
fn logical_collision_refuses_a_second_physical_winner() {
    let mut documents = SourceDocuments {
        documents: BTreeMap::new(),
        physical: BTreeMap::new(),
    };
    documents
        .insert(PathBuf::from("/first.harn"), "harn:source/file.harn".into())
        .unwrap();
    assert_eq!(
        documents.insert(
            PathBuf::from("/second.harn"),
            "harn:source/file.harn".into()
        ),
        Err(SourceDocumentError::DocumentCollision(
            "harn:source/file.harn".into()
        ))
    );
    assert_eq!(
        documents.resolve("harn:source/file.harn"),
        Some(Path::new("/first.harn"))
    );
}

#[test]
fn private_records_roundtrip_and_refuse_empty_duplicate_or_malformed_censuses() {
    let directory = tempfile::tempdir().unwrap();
    let (graph, entry) = local_graph(directory.path());
    let documents = graph.source_documents(&entry).unwrap();
    let records = documents.records();
    assert_eq!(records.len(), 3);
    let restored = SourceDocuments::from_records(records.clone()).unwrap();
    assert_eq!(restored.records(), records);
    assert_eq!(
        restored.document_for(&entry.canonicalize().unwrap()),
        Some("harn:source/entry.harn")
    );
    assert!(SourceDocuments::from_records(Vec::new()).is_err());
    let mut duplicate = records.clone();
    duplicate.push(records[0].clone());
    assert!(SourceDocuments::from_records(duplicate).is_err());
    for document in [
        "file:///etc/passwd",
        "harn:synthetic/std/json",
        "harn:source/../secret",
        "harn:source/%bad",
        "harn:package/name/bad/file.harn",
    ] {
        let mut malformed = records.clone();
        malformed[0].document = document.into();
        assert!(
            SourceDocuments::from_records(malformed).is_err(),
            "{document}"
        );
    }
    let mut relative = records;
    relative[0].path = PathBuf::from("relative.harn");
    assert!(SourceDocuments::from_records(relative).is_err());
    assert!(serde_json::from_value::<SourceDocumentRecord>(
        serde_json::json!({"document": "harn:source/file.harn"})
    )
    .is_err());
    assert!(restored.resolve("harn:source/not-recorded.harn").is_none());
}

#[test]
fn reached_nested_scope_matches_direct_resolver_and_retains_its_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let outer = directory.path();
    let inner = outer.join("nested");
    let outer_packages = publish_packages(outer, "outer-generation", &["dependency"]);
    let inner_packages = publish_packages(&inner, "inner-generation", &["dependency"]);
    write(
        &outer_packages,
        "dependency/types.harn",
        "pub type Item = {outer: int}\n",
    );
    let expected = write(
        &inner_packages,
        "dependency/types.harn",
        "pub type Item = {inner: string}\n",
    )
    .canonicalize()
    .unwrap();
    let nested_entry = package_entry(&inner, &["dependency"]);
    let entry = write(
        outer,
        "entry.harn",
        "import \"./nested/entry\"\npub fn root() {}\n",
    );
    let direct = crate::resolve_import_path(&nested_entry, "dependency/types")
        .expect("direct resolver reaches nested package")
        .canonicalize()
        .unwrap();
    assert_eq!(direct, expected, "known non-null direct owner control");
    let graph = crate::build(std::slice::from_ref(&entry));
    let observed = graph
        .imports_for_module(&nested_entry)
        .into_iter()
        .find(|import| import.raw_path == "dependency/types")
        .and_then(|import| import.resolved_path)
        .expect("graph reached a real package target");
    assert_eq!(
        observed, direct,
        "graph and direct resolver must use the same nearest package scope"
    );
    assert_eq!(
        graph._package_snapshots.len(),
        2,
        "both reached scope leases belong to the graph"
    );
    let documents = graph.source_documents(&entry).unwrap();
    let document = documents
        .document_for(&expected)
        .expect("nested package root is measured");
    assert!(document.starts_with("harn:package/dependency/"));
}

#[test]
fn parent_and_reserved_characters_are_opaque_and_unknown_names_grant_no_access() {
    let directory = tempfile::tempdir().unwrap();
    let imported = write(
        directory.path(),
        "shared#types.harn",
        "pub type Item = {value: int}\n",
    );
    let entry = write(
        directory.path(),
        "src/entry.harn",
        "import \"../shared#types\"\npub fn echo() {}\n",
    );
    let mut graph = crate::build(std::slice::from_ref(&entry));
    graph.modules.insert(
        PathBuf::from("<std>/json"),
        Arc::new(crate::ModuleInfo::default()),
    );
    let documents = graph.source_documents(&entry).unwrap();
    let physical = imported.canonicalize().unwrap();
    let document = "harn:source/%2E%2E/shared%23types.harn";
    assert_eq!(documents.document_for(&physical), Some(document));
    assert_eq!(documents.resolve(document), Some(physical.as_path()));
    assert!(documents
        .resolve("harn:source/../shared#types.harn")
        .is_none());
    assert!(documents.resolve("harn:source/%2E%2E/.env").is_none());
    assert_eq!(
        documents.document_for(Path::new("<std>/json")),
        Some("harn:synthetic/std/json")
    );
    assert!(documents.resolve("harn:synthetic/std/json").is_none());
    assert_eq!(documents.physical_paths().count(), 2);
}

#[cfg(unix)]
#[test]
fn non_utf8_module_name_refuses_instead_of_using_lossy_identity() {
    use std::os::unix::ffi::OsStringExt;
    let directory = tempfile::tempdir().unwrap();
    let (mut graph, entry) = local_graph(directory.path());
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(vec![0xff]));
    graph
        .modules
        .insert(path, Arc::new(crate::ModuleInfo::default()));
    assert!(matches!(
        graph.source_documents(&entry),
        Err(SourceDocumentError::InvalidPath(_))
    ));
}
