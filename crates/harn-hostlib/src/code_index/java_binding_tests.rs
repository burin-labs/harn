use std::fs;

use harn_vm::VmValue;
use tempfile::TempDir;

use crate::code_index::CodeIndexCapability;
use crate::test_support::*;

use super::builtins::SharedIndex;
use super::state::IndexState;
use super::symbol_graph::{EdgeKind, NodeKind, SymbolGraph};

struct Workspace {
    dir: TempDir,
    index: SharedIndex,
    initial_files: Vec<(String, String)>,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (path, source) in files {
            let abs = dir.path().join(path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, source).unwrap();
        }
        let capability = CodeIndexCapability::new();
        let index = capability.shared();
        *index.lock().unwrap() = Some(IndexState::build_from_root(dir.path()).0);
        Self {
            dir,
            index,
            initial_files: files
                .iter()
                .map(|(path, source)| (path.to_string(), source.to_string()))
                .collect(),
        }
    }

    fn rename(&self, name: &str, path: &str, line: Option<i64>, col: Option<i64>) -> VmValue {
        super::rename::run(
            &self.index,
            &[dict(&[
                (
                    "symbol_ref",
                    dict(&[
                        ("name", vm_string(name)),
                        ("path", vm_string(path)),
                        ("line", line.map_or(VmValue::Nil, VmValue::Int)),
                        ("col", col.map_or(VmValue::Nil, VmValue::Int)),
                    ]),
                ),
                ("new_name", vm_string("loadById")),
                ("scope", vm_string("workspace")),
                ("backend", vm_string("syntax_tree")),
            ])],
        )
        .unwrap()
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    fn assert_unchanged(&self) {
        for (path, source) in &self.initial_files {
            assert_eq!(
                fs::read(self.dir.path().join(path)).unwrap(),
                source.as_bytes(),
                "{path}"
            );
        }
    }
}

fn calls(graph: &SymbolGraph, owner: &str) -> usize {
    let methods: Vec<_> = graph
        .iter_nodes()
        .filter(|node| {
            node.kind == NodeKind::Function
                && node.name == "getById"
                && node.owner_type.as_deref() == Some(owner)
        })
        .collect();
    assert_eq!(methods.len(), 1, "one measured method on {owner}");
    graph
        .incoming(methods[0].id)
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .count()
}

const DOCUMENTS: &str = "package lab;\npublic final class DocumentStore {\n  public int getById(int id) { return id; }\n}\n";
const AUDIT: &str = "package lab;\npublic final class AuditStore {\n  // getById is a separate audit API.\n  public int getById(int id) { return id; }\n  public String label() { return \"getById\"; }\n}\n";
const KNOWN_PAGE: &str = "package lab; final class KnownPage { int read(DocumentStore documents) { return documents.getById(1); } }";

#[test]
fn java_type_rename_refuses_unproven_references_before_writing() {
    for scope in ["file", "workspace"] {
        for dry_run in [false, true] {
            for replacement in [false, true] {
                for backend in ["syntax_tree", "auto", "language_server"] {
                    let ws = Workspace::new(&[
                        ("lab/DocumentStore.java", DOCUMENTS),
                        ("lab/KnownPage.java", KNOWN_PAGE),
                        ("lab/AuditStore.java", AUDIT),
                    ]);
                    let mut request = vec![
                        (
                            "symbol_ref",
                            dict(&[
                                ("name", vm_string("DocumentStore")),
                                ("path", vm_string("lab/DocumentStore.java")),
                                ("line", VmValue::Int(2)),
                                ("kind", vm_string("Type")),
                            ]),
                        ),
                        ("scope", vm_string(scope)),
                        ("backend", vm_string(backend)),
                        ("dry_run", VmValue::Bool(dry_run)),
                    ];
                    request.push(if replacement {
                        ("replacement_text", vm_string("DocumentLibrary"))
                    } else {
                        ("new_name", vm_string("DocumentLibrary"))
                    });
                    let result = super::rename::run(&ws.index, &[dict(&request)]).unwrap();
                    assert_eq!(
                        s(field(&result, "result")),
                        "unsupported_language",
                        "{result:?}"
                    );
                    assert_eq!(n(field(&result, "match_count")), 0, "{result:?}");
                    ws.assert_unchanged();
                }
            }
        }
    }

    let ws = Workspace::new(&[("lab/DocumentStore.java", DOCUMENTS)]);
    let result = super::rename::run(
        &ws.index,
        &[dict(&[
            (
                "symbol_ref",
                dict(&[
                    ("name", vm_string("DocumentStore")),
                    ("path", vm_string("lab/DocumentStore.java")),
                    ("line", VmValue::Int(2)),
                ]),
            ),
            ("new_name", vm_string("not-valid")),
            ("scope", vm_string("workspace")),
        ])],
    )
    .unwrap();
    assert_eq!(
        s(field(&result, "result")),
        "invalid_identifier",
        "{result:?}"
    );
    ws.assert_unchanged();
}

#[test]
fn java_constructor_seed_refuses_type_rename_before_writing() {
    let caller =
        "package lab; final class KnownPage { DocumentStore store = new DocumentStore(); }";
    for (path, source, name, line) in [
        ("lab/DocumentStore.java", "package lab;\npublic final class DocumentStore {\n  public DocumentStore() {}\n}\n", "DocumentStore", 3),
        ("lab/Colour.java", "package lab;\nenum Colour {\n  RED;\n  Colour() {}\n}\n", "Colour", 4),
        ("lab/Badge.java", "package lab;\nrecord Badge(int value) {\n  Badge {}\n}\n", "Badge", 3),
        ("lab/Outer.java", "package lab;\nfinal class Outer {\n  static final class Inner {\n    Inner() {}\n  }\n}\n", "Inner", 4),
    ] {
        for backend in ["syntax_tree", "auto", "language_server"] {
            for dry_run in [false, true] {
                let ws = Workspace::new(&[(path, source), ("lab/KnownPage.java", caller)]);
                let result = super::rename::run(
                    &ws.index,
                    &[dict(&[
                        (
                            "symbol_ref",
                            dict(&[
                                ("name", vm_string(name)),
                                ("path", vm_string(path)),
                                ("line", VmValue::Int(line)),
                                ("kind", vm_string("Function")),
                            ]),
                        ),
                        ("new_name", vm_string("DocumentLibrary")),
                        ("scope", vm_string("workspace")),
                        ("backend", vm_string(backend)),
                        ("dry_run", VmValue::Bool(dry_run)),
                    ])],
                )
                .unwrap();
                assert_eq!(
                    s(field(&result, "result")),
                    "unsupported_language",
                    "{result:?}"
                );
                assert_eq!(n(field(&result, "match_count")), 0, "{result:?}");
                ws.assert_unchanged();
            }
        }
    }
}

#[test]
fn java_same_named_method_is_not_a_constructor() {
    let source =
        "package lab; final class DocumentStore { DocumentStore() {} void DocumentStore() {} }";
    let ws = Workspace::new(&[("lab/DocumentStore.java", source)]);
    let index = ws.index.lock().unwrap();
    let state = index.as_ref().unwrap();
    let seed = state
        .symbols
        .iter_nodes()
        .find(|node| {
            node.kind == NodeKind::Function
                && node.name == "DocumentStore"
                && node.col == Some(source.rfind("DocumentStore").unwrap() as u32 + 1)
        })
        .expect("the method is indexed");
    let tree = crate::ast::api::parse_tree(source, crate::ast::Language::Java).unwrap();
    assert!(!super::rename_java::is_constructor(&tree, source, seed));
    drop(index);
    let result = ws.rename(
        "DocumentStore",
        "lab/DocumentStore.java",
        Some(1),
        Some(source.rfind("DocumentStore").unwrap() as i64 + 1),
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        ws.read("lab/DocumentStore.java"),
        source.replace("void DocumentStore()", "void loadById()")
    );
}

fn assert_known_call(graph: &SymbolGraph, path: &str) {
    let sites: Vec<_> = graph
        .iter_nodes()
        .filter(|node| {
            node.kind == NodeKind::CallSite && node.name == "getById" && node.path.as_ref() == path
        })
        .collect();
    assert_eq!(sites.len(), 1, "the same-fixture concrete call is indexed");
    let targets: Vec<_> = graph
        .outgoing(sites[0].id)
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .filter_map(|edge| graph.node(edge.to))
        .collect();
    assert_eq!(targets.len(), 1, "the concrete call resolves");
    assert_eq!(targets[0].owner_type.as_deref(), Some("lab.DocumentStore"));
}

#[test]
fn java_rename_ignores_new_name_on_unrelated_method_owners() {
    let audit = "package lab; final class AuditStore { int getById(int id) { return id; } int loadById(int id) { return id; } }";
    let page = "package lab; final class Page { int read(DocumentStore documents, AuditStore audit) { /* 😀 */ return documents.getById(1) + audit.getById(2) + audit.loadById(3); } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/AuditStore.java", audit),
        ("lab/Page.java", page),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_eq!(calls(graph, "lab.DocumentStore"), 1);
        assert_eq!(calls(graph, "lab.AuditStore"), 1);
        let audit_new = graph.java_method("loadById", "lab.AuditStore").unwrap();
        assert_eq!(
            graph
                .incoming(audit_new.id)
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .count(),
            1
        );
    }
    let result = ws.rename(
        "DocumentStore.getById",
        "lab/DocumentStore.java",
        Some(3),
        Some(14),
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(n(field(&result, "match_count")), 2);
    assert_eq!(
        ws.read("lab/DocumentStore.java"),
        DOCUMENTS.replace("getById", "loadById")
    );
    assert_eq!(ws.read("lab/AuditStore.java"), audit);
    assert_eq!(
        ws.read("lab/Page.java"),
        page.replace("documents.getById", "documents.loadById")
    );
}

#[test]
fn java_rename_keeps_same_owner_new_name_conflicts() {
    let documents = "package lab;\npublic final class DocumentStore {\n  public int getById(int id) { return id; }\n  public int loadById(String id) { return id.length(); }\n}\n";
    let page = "package lab; final class Page { int read(DocumentStore documents) { return documents.getById(1) + documents.loadById(\"x\"); } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", documents),
        ("lab/Page.java", page),
    ]);
    let result = ws.rename(
        "DocumentStore.getById",
        "lab/DocumentStore.java",
        Some(3),
        Some(14),
    );
    assert_eq!(s(field(&result, "result")), "conflict", "{result:?}");
    let VmValue::List(conflicts) = field(&result, "conflicts") else {
        panic!("missing actual same-owner conflicts")
    };
    assert_eq!(conflicts.len(), 2);
    assert_eq!(n(field(&result, "match_count")), 0);
    assert_eq!(ws.read("lab/DocumentStore.java"), documents);
    assert_eq!(ws.read("lab/Page.java"), page);
}

#[test]
fn java_rename_refuses_unknown_new_name_receiver_bindings() {
    let audit = "package lab; class AuditStore { int loadById(int id) { return id; } }";
    let page = "package lab; final class Page<T extends AuditStore> { int read(DocumentStore documents, T audit) { return documents.getById(1) + audit.loadById(2); } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/AuditStore.java", audit),
        ("lab/Page.java", page),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_eq!(calls(graph, "lab.DocumentStore"), 1);
        let sites: Vec<_> = graph
            .iter_nodes()
            .filter(|node| node.kind == NodeKind::CallSite && node.name == "loadById")
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(sites[0].owner_type.is_none());
    }
    let result = ws.rename(
        "DocumentStore.getById",
        "lab/DocumentStore.java",
        Some(3),
        Some(14),
    );
    assert_eq!(
        s(field(&result, "ambiguity_reason")),
        "unresolved_reference_binding",
        "{result:?}"
    );
    assert_eq!(n(field(&result, "match_count")), 0);
    assert_eq!(ws.read("lab/DocumentStore.java"), DOCUMENTS);
    assert_eq!(ws.read("lab/AuditStore.java"), audit);
    assert_eq!(ws.read("lab/Page.java"), page);
}

#[test]
fn java_annotated_type_parameters_do_not_bind_to_namesake_classes() {
    let marker = "package lab; @java.lang.annotation.Target(java.lang.annotation.ElementType.TYPE_PARAMETER) @interface Marker {}";
    for source in [
        "package lab; class Page<DocumentStore extends AuditStore> { int read(DocumentStore documents) { return documents.getById(1); } }",
        "package lab; class Page<@Marker DocumentStore extends AuditStore> { int read(DocumentStore documents) { return documents.getById(1); } }",
        "package lab; class Page { <DocumentStore extends AuditStore> int read(DocumentStore documents) { return documents.getById(1); } }",
        "package lab; class Page { <@Marker DocumentStore extends AuditStore> int read(DocumentStore documents) { return documents.getById(1); } }",
    ] {
        let ws = Workspace::new(&[
            ("lab/DocumentStore.java", DOCUMENTS),
            ("lab/AuditStore.java", AUDIT),
            ("lab/Marker.java", marker),
            ("lab/Page.java", source),
            ("lab/KnownPage.java", KNOWN_PAGE),
        ]);
        {
            let guard = ws.index.lock().unwrap();
            let graph = &guard.as_ref().unwrap().symbols;
            assert_known_call(graph, "lab/KnownPage.java");
            assert_eq!(calls(graph, "lab.DocumentStore"), 1, "{source}");
            assert_eq!(
                graph
                    .iter_nodes()
                    .filter(|node| node.kind == NodeKind::CallSite && node.name == "getById")
                    .count(),
                2,
                "the unresolved reference was indexed"
            );
        }
        let result = ws.rename(
            "DocumentStore.getById",
            "lab/DocumentStore.java",
            Some(3),
            Some(14),
        );
        assert_eq!(
            s(field(&result, "result")),
            "ambiguous_symbol",
            "{result:?}"
        );
        assert_eq!(
            s(field(&result, "ambiguity_reason")),
            "unresolved_reference_binding"
        );
        assert_eq!(n(field(&result, "match_count")), 0);
        let VmValue::List(candidates) = field(&result, "warnings") else {
            panic!("missing unresolved declaration metadata")
        };
        assert_eq!(candidates.len(), 2);
        let mut symbols: Vec<_> = candidates.iter().map(|candidate| {
            assert_eq!(s(field(candidate, "kind")), "Function");
            assert!(n(field(candidate, "line")) > 0);
            assert!(n(field(candidate, "col")) > 0);
            let VmValue::Dict(metadata) = candidate else {
                panic!("expected declaration metadata")
            };
            assert!(!metadata.contains_key("retry_arguments"));
            s(field(candidate, "qualified_name"))
        }).collect();
        symbols.sort();
        assert_eq!(symbols, ["lab.AuditStore.getById", "lab.DocumentStore.getById"]);
        assert_eq!(ws.read("lab/DocumentStore.java"), DOCUMENTS);
        assert_eq!(ws.read("lab/AuditStore.java"), AUDIT);
        assert_eq!(ws.read("lab/Page.java"), source);
        ws.assert_unchanged();
    }
}

#[test]
fn java_generic_receiver_controls_refuse_unproved_bindings() {
    let audit = "package lab; class AuditStore implements Runnable { public int getById(int id) { return id; } public void run() {} }";
    let marker = "package lab; @java.lang.annotation.Target(java.lang.annotation.ElementType.TYPE_PARAMETER) @interface Marker {}";
    for source in [
        "package lab; class Page<DocumentStore extends AuditStore & Runnable> { int read(DocumentStore documents) { return documents.getById(1); } }",
        "package lab; class Page<@Marker DocumentStore extends AuditStore & Runnable> { int read(DocumentStore documents) { return documents.getById(1); } }",
        "package lab; class Holder<T> { T value; } class Page { int read(Holder<? extends AuditStore> documents) { return documents.value.getById(1); } }",
        "package lab; class Base<T extends AuditStore> { T documents; } class Page extends Base<AuditStore> { int read() { return documents.getById(1); } }",
        "package lab; class Base<T extends AuditStore> { T documents; } class Page extends Base { int read() { return documents.getById(1); } }",
    ] {
        let ws = Workspace::new(&[("lab/DocumentStore.java", DOCUMENTS), ("lab/AuditStore.java", audit), ("lab/Marker.java", marker), ("lab/Page.java", source), ("lab/KnownPage.java", KNOWN_PAGE)]);
        {
            let guard = ws.index.lock().unwrap();
            let graph = &guard.as_ref().unwrap().symbols;
            assert_known_call(graph, "lab/KnownPage.java");
            assert_eq!(calls(graph, "lab.DocumentStore"), 1, "{source}");
            let sites: Vec<_> = graph.iter_nodes().filter(|node| node.kind == NodeKind::CallSite && node.name == "getById" && node.path.as_ref() == "lab/Page.java").collect();
            assert_eq!(sites.len(), 1, "the generic reference was indexed");
            assert!(sites[0].owner_type.is_none(), "{source}");
        }
        let result = ws.rename("DocumentStore.getById", "lab/DocumentStore.java", Some(3), Some(14));
        assert_eq!(s(field(&result, "ambiguity_reason")), "unresolved_reference_binding", "{result:?}");
        assert_eq!(n(field(&result, "match_count")), 0);
        assert_eq!(ws.read("lab/DocumentStore.java"), DOCUMENTS);
        assert_eq!(ws.read("lab/AuditStore.java"), audit);
        assert_eq!(ws.read("lab/Page.java"), source);
        ws.assert_unchanged();
    }
    for annotation in ["", "@Marker "] {
        let source = format!("package lab; class Page<{annotation}DocumentStore> {{ String read(DocumentStore documents) {{ return documents.toString(); }} }}");
        let ws = Workspace::new(&[
            ("lab/DocumentStore.java", DOCUMENTS),
            ("lab/Marker.java", marker),
            ("lab/Page.java", &source),
            ("lab/KnownPage.java", KNOWN_PAGE),
        ]);
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_known_call(graph, "lab/KnownPage.java");
        let sites: Vec<_> = graph
            .iter_nodes()
            .filter(|node| node.kind == NodeKind::CallSite && node.name == "toString")
            .collect();
        assert_eq!(sites.len(), 1, "the unbounded Object call was indexed");
        assert!(sites[0].owner_type.is_none(), "{source}");
    }
}

#[test]
fn java_snapshot_rebuilds_v8_annotation_edges_and_restores_current_edges() {
    use super::snapshot::{CodeIndexSnapshot, SNAPSHOT_FORMAT_VERSION};
    use super::symbol_graph::Edge;

    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/AuditStore.java", AUDIT),
        ("lab/Marker.java", "package lab; @java.lang.annotation.Target(java.lang.annotation.ElementType.TYPE_PARAMETER) @interface Marker {}"),
        ("lab/Page.java", "package lab; class Page<@Marker DocumentStore extends AuditStore> { int read(DocumentStore documents) { return documents.getById(1); } }"),
        ("lab/KnownPage.java", "package lab; class KnownPage { int read(DocumentStore documents) { return documents.getById(1); } }"),
    ]);
    let mut snapshot = ws.index.lock().unwrap().as_ref().unwrap().snapshot();
    assert_eq!(snapshot.meta.format_version, SNAPSHOT_FORMAT_VERSION);
    snapshot.save(ws.dir.path()).unwrap();
    let restored = CodeIndexCapability::new();
    assert!(restored.restore_from_disk(ws.dir.path()).unwrap());
    restored.wait_until_idle();
    assert_eq!(
        calls(
            &restored.shared().lock().unwrap().as_ref().unwrap().symbols,
            "lab.DocumentStore"
        ),
        1
    );

    let graph = snapshot.symbols.as_mut().unwrap();
    let target = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == NodeKind::Function
                && node.owner_type.as_deref() == Some("lab.DocumentStore")
                && node.name == "getById"
        })
        .unwrap()
        .id;
    let false_call = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == NodeKind::CallSite
                && node.path.as_ref() == "lab/Page.java"
                && node.name == "getById"
        })
        .unwrap()
        .id;
    graph.edges.push(Edge {
        from: false_call,
        to: target,
        kind: EdgeKind::Calls,
    });
    snapshot.meta.format_version = 8;
    snapshot.save(ws.dir.path()).unwrap();
    let upgraded = CodeIndexCapability::new();
    assert!(!upgraded.restore_from_disk(ws.dir.path()).unwrap());
    assert!(upgraded.shared().lock().unwrap().is_none());
    upgraded.warm_session(ws.dir.path());
    upgraded.wait_until_idle();
    assert_eq!(
        calls(
            &upgraded.shared().lock().unwrap().as_ref().unwrap().symbols,
            "lab.DocumentStore"
        ),
        1
    );
    let rebuilt = CodeIndexSnapshot::load(ws.dir.path()).unwrap().unwrap();
    assert_eq!(rebuilt.meta.format_version, SNAPSHOT_FORMAT_VERSION);
}

#[test]
fn java_relative_type_names_do_not_bind_to_packages_or_enum_namesakes() {
    let ws = Workspace::new(&[
        ("Outer/Store.java", "package Outer; public class Store { public int getById(int id) { return 100; } }"),
        ("lab/Outer.java", "package lab; class Outer { static class Store { int getById(int id) { return 10; } } }"),
        ("lab/Page.java", "package lab; class Page { int read(Outer.Store store) { return store.getById(1); } }"),
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/EnumPage.java", "package lab; enum EnumPage { FIRST; static class DocumentStore { int getById(int id) { return id; } } int read(DocumentStore store) { return store.getById(1); } }"),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_eq!(calls(graph, "Outer.Store"), 0);
        assert_eq!(calls(graph, "lab.DocumentStore"), 0);
        assert_eq!(
            graph
                .iter_nodes()
                .filter(|node| node.kind == NodeKind::CallSite && node.name == "getById")
                .count(),
            2
        );
    }
    let before = ws.read("lab/Page.java");
    let result = ws.rename("Outer.Store.getById", "Outer/Store.java", None, None);
    assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
    assert_eq!(ws.read("lab/Page.java"), before);
}

#[test]
fn java_namesake_fixture_binds_thirty_calls_and_renames_only_document_receivers() {
    let mut files = vec![
        (
            "src/lab/DocumentStore.java".to_string(),
            DOCUMENTS.to_string(),
        ),
        ("src/lab/AuditStore.java".to_string(), AUDIT.to_string()),
    ];
    for page in 0..15 {
        files.push((format!("src/lab/Page{page}.java"), format!("package lab;\nclass Page{page} {{\n  int render(int id) {{\n    DocumentStore documents = new DocumentStore();\n    return documents.getById(id) + documents.getById(id + 1);\n  }}\n}}\n")));
    }
    let audit_call = "package lab;\nclass AuditPage { int render(AuditStore audit) { return audit.getById(1); } }\n";
    files.push(("src/lab/AuditPage.java".into(), audit_call.into()));
    let borrowed: Vec<_> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let ws = Workspace::new(&borrowed);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_eq!(calls(graph, "lab.DocumentStore"), 30);
        assert_eq!(calls(graph, "lab.AuditStore"), 1);
        let restored = SymbolGraph::from_snapshot(graph.snapshot()).unwrap();
        assert_eq!(calls(&restored, "lab.DocumentStore"), 30);
        assert_eq!(calls(&restored, "lab.AuditStore"), 1);
    }
    let result = ws.rename(
        "DocumentStore.getById",
        "src/lab/DocumentStore.java",
        Some(3),
        Some(14),
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(ws.read("src/lab/AuditStore.java"), AUDIT);
    assert_eq!(ws.read("src/lab/AuditPage.java"), audit_call);
    assert!(ws.read("src/lab/DocumentStore.java").contains("loadById"));
    for page in 0..15 {
        let source = ws.read(&format!("src/lab/Page{page}.java"));
        assert_eq!(source.matches("documents.loadById(").count(), 2);
        assert!(!source.contains("getById"));
    }
}

#[test]
fn java_field_parameter_local_and_this_receivers_keep_their_own_bindings() {
    let consumer = "package lab;\nclass Page {\n  DocumentStore store;\n  DocumentStore parameter;\n  int read(AuditStore store) {\n    int a = this.store.getById(1);\n    int b = store.getById(2);\n    { DocumentStore local = parameter; a += local.getById(3); }\n    { AuditStore parameter = store; a += parameter.getById(4); }\n    return a + parameter.getById(5);\n  }\n}\n";
    let ws = Workspace::new(&[
        ("DocumentStore.java", DOCUMENTS),
        ("AuditStore.java", AUDIT),
        ("Page.java", consumer),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_eq!(calls(graph, "lab.DocumentStore"), 3);
        assert_eq!(calls(graph, "lab.AuditStore"), 2);
    }
    let result = ws.rename(
        "lab.DocumentStore.getById",
        "DocumentStore.java",
        None,
        None,
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        ws.read("Page.java"),
        consumer
            .replace("this.store.getById", "this.store.loadById")
            .replace("local.getById", "local.loadById")
            .replace("parameter.getById(5)", "parameter.loadById(5)")
    );
}

#[test]
fn java_explicit_imports_separate_same_simple_type_names() {
    let first =
        "package first; public final class Store { public int getById(int id) { return id; } }";
    let second =
        "package second; public final class Store { public int getById(int id) { return id; } }";
    let consumer = "package app; import first.Store; class Page { int read(Store store) { return store.getById(1); } }";
    let ws = Workspace::new(&[
        ("first/Store.java", first),
        ("second/Store.java", second),
        ("app/Page.java", consumer),
    ]);
    let guard = ws.index.lock().unwrap();
    let graph = &guard.as_ref().unwrap().symbols;
    assert_eq!(calls(graph, "first.Store"), 1);
    assert_eq!(calls(graph, "second.Store"), 0);
}

#[test]
fn java_unknown_receivers_never_inherit_unique_or_same_file_name_certainty() {
    for unknown in [
        "Object store",
        "var store = unknown()",
        "External store",
        "Child store",
    ] {
        let source = format!("package lab; class DocumentStore {{ int getById(int id) {{ return id; }} int read({}) {{ return store.getById(1); }} }}", if unknown.starts_with("var ") { "" } else { unknown });
        let source = if unknown.starts_with("var ") {
            source.replace("return store", "var store = unknown(); return store")
        } else {
            source
        };
        let ws = Workspace::new(&[("DocumentStore.java", &source)]);
        {
            let guard = ws.index.lock().unwrap();
            assert_eq!(
                calls(&guard.as_ref().unwrap().symbols, "lab.DocumentStore"),
                0,
                "{unknown}"
            );
        }
        let result = ws.rename("getById", "DocumentStore.java", None, None);
        assert_eq!(
            s(field(&result, "result")),
            "ambiguous_symbol",
            "{unknown}: {result:?}"
        );
        assert_eq!(
            s(field(&result, "ambiguity_reason")),
            "unresolved_reference_binding"
        );
        assert_eq!(ws.read("DocumentStore.java"), source);
    }
}

#[test]
fn java_definition_selectors_do_not_fall_back_when_path_or_column_is_wrong() {
    let ws = Workspace::new(&[
        ("DocumentStore.java", DOCUMENTS),
        ("AuditStore.java", AUDIT),
    ]);
    for (name, path, line, col) in [
        ("getById", "Missing.java", None, None),
        ("getById", "elsewhere/DocumentStore.java", None, None),
        ("DocumentStore.getById", "AuditStore.java", None, None),
        ("getById", "DocumentStore.java", Some(3), Some(1)),
    ] {
        let result = ws.rename(name, path, line, col);
        assert_eq!(s(field(&result, "result")), "no_match", "{result:?}");
    }
    assert_eq!(ws.read("DocumentStore.java"), DOCUMENTS);
    assert_eq!(ws.read("AuditStore.java"), AUDIT);
}

#[test]
fn ambiguity_reasons_separate_definition_selection_from_unresolved_references() {
    let ws = Workspace::new(&[
        ("DocumentStore.java", DOCUMENTS),
        ("AuditStore.java", AUDIT),
    ]);
    let ambiguous = ws.rename("getById", ".", None, None);
    assert_eq!(s(field(&ambiguous, "result")), "ambiguous_symbol");
    assert_eq!(
        s(field(&ambiguous, "ambiguity_reason")),
        "multiple_definition_seeds"
    );
    let VmValue::List(candidates) = field(&ambiguous, "warnings") else {
        panic!("missing candidate list")
    };
    assert_eq!(candidates.len(), 2);
    for candidate in candidates.iter() {
        assert_eq!(n(field(candidate, "col")), 14);
        assert!(s(field(candidate, "symbol")).starts_with("lab."));
    }
    let first = "export function load() { return 1; }";
    let second = "export function load() { return 2; }";
    let unrelated = Workspace::new(&[("first.ts", first), ("second.ts", second)]);
    let unresolved = unrelated.rename("load", "first.ts", None, None);
    assert_eq!(s(field(&unresolved, "result")), "ambiguous_symbol");
    assert_eq!(
        s(field(&unresolved, "ambiguity_reason")),
        "unresolved_reference_binding"
    );
    assert_eq!(unrelated.read("first.ts"), first);
    assert_eq!(unrelated.read("second.ts"), second);
}

#[test]
fn same_line_qualified_candidates_preserve_the_selected_definition_ids() {
    let source = "class First { void load(int x) {} void load(String x) {} } class Second { void load(int x) {} }";
    let ws = Workspace::new(&[("Stores.java", source)]);
    let result = ws.rename("First.load", "Stores.java", None, None);
    assert_eq!(
        s(field(&result, "ambiguity_reason")),
        "multiple_definition_seeds"
    );
    let VmValue::List(candidates) = field(&result, "warnings") else {
        panic!("missing candidates")
    };
    assert_eq!(candidates.len(), 2);
    let mut columns = Vec::new();
    for candidate in candidates.iter() {
        assert_eq!(s(field(candidate, "symbol")), "First.load");
        columns.push(n(field(candidate, "col")));
    }
    columns.sort_unstable();
    columns.dedup();
    assert_eq!(columns.len(), 2);
    assert_eq!(ws.read("Stores.java"), source);
}

#[test]
fn selectors_use_utf16_columns_after_an_astral_character() {
    let source = "final class Store { String marker = \"🦀\"; int getById(int id) { return id; } }";
    let ws = Workspace::new(&[("Store.java", source)]);
    let at = source.find("getById").unwrap();
    let utf16_col = source.get(..at).unwrap().encode_utf16().count() as i64 + 1;
    let byte_col = at as i64 + 1;
    assert_ne!(utf16_col, byte_col);
    let wrong = ws.rename("Store.getById", "Store.java", Some(1), Some(byte_col));
    assert_eq!(s(field(&wrong, "result")), "no_match");
    assert_eq!(ws.read("Store.java"), source);
    let right = ws.rename("Store.getById", "Store.java", Some(1), Some(utf16_col));
    assert_eq!(s(field(&right, "result")), "applied", "{right:?}");
    assert_eq!(ws.read("Store.java"), source.replace("getById", "loadById"));
}

#[test]
fn java_nested_type_and_pattern_shadows_never_bind_to_a_same_package_class() {
    for source in [
        "package lab; class Page { static class DocumentStore { int getById(int id) { return id; } } int read(DocumentStore store) { return store.getById(1); } }",
        "package lab; class Page { DocumentStore item; int read(Object other) { if (other instanceof AuditStore item) { return item.getById(1); } return 0; } }",
    ] {
        let ws = Workspace::new(&[("DocumentStore.java", DOCUMENTS), ("AuditStore.java", AUDIT), ("Page.java", source), ("KnownPage.java", KNOWN_PAGE)]);
        {
            let guard = ws.index.lock().unwrap();
            let graph = &guard.as_ref().unwrap().symbols;
            assert_known_call(graph, "KnownPage.java");
            assert_eq!(calls(graph, "lab.DocumentStore"), 1);
        }
        let result = ws.rename("DocumentStore.getById", "DocumentStore.java", None, None);
        assert_eq!(s(field(&result, "result")), "ambiguous_symbol", "{result:?}");
        assert_eq!(ws.read("DocumentStore.java"), DOCUMENTS);
        assert_eq!(ws.read("Page.java"), source);
        ws.assert_unchanged();
    }
}

#[test]
fn java_inheritance_contracts_and_overloads_refuse_before_writes() {
    for source in [
        "package lab; class Base { int getById(int id) { return id; } } class DocumentStore extends Base { @Override int getById(int id) { return id; } }",
        "package lab; interface Store { int getById(int id); } class DocumentStore implements Store { public int getById(int id) { return id; } }",
        "package lab; class DocumentStore { int getById(int id) { return id; } int getById(String id) { return id.length(); } }",
    ] {
        let ws = Workspace::new(&[("DocumentStore.java", source)]);
        let result = ws.rename("DocumentStore.getById", "DocumentStore.java", None, None);
        assert_eq!(s(field(&result, "result")), "ambiguous_symbol", "{result:?}");
        assert_eq!(ws.read("DocumentStore.java"), source);
    }
}

#[test]
fn java_reindex_replaces_receiver_edges_and_new_overloads_clear_old_certainty() {
    let source =
        "package lab; class Page { int read(DocumentStore store) { return store.getById(1); } }";
    let ws = Workspace::new(&[
        ("DocumentStore.java", DOCUMENTS),
        ("AuditStore.java", AUDIT),
        ("Page.java", source),
    ]);
    let mut guard = ws.index.lock().unwrap();
    let state = guard.as_mut().unwrap();
    assert_eq!(calls(&state.symbols, "lab.DocumentStore"), 1);
    fs::write(
        ws.dir.path().join("Page.java"),
        source.replace("DocumentStore store", "AuditStore store"),
    )
    .unwrap();
    state
        .reindex_file(&ws.dir.path().join("Page.java"))
        .unwrap();
    assert_eq!(calls(&state.symbols, "lab.DocumentStore"), 0);
    assert_eq!(calls(&state.symbols, "lab.AuditStore"), 1);
    fs::write(
        ws.dir.path().join("AuditStore.java"),
        AUDIT.replace(
            "  public String label()",
            "  public int getById(String id) { return id.length(); }\n  public String label()",
        ),
    )
    .unwrap();
    state
        .reindex_file(&ws.dir.path().join("AuditStore.java"))
        .unwrap();
    assert_eq!(
        state
            .symbols
            .iter_nodes()
            .filter(|node| node.kind == NodeKind::CallSite && node.name == "getById")
            .map(|node| state
                .symbols
                .outgoing(node.id)
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .count())
            .sum::<usize>(),
        0
    );
}

#[test]
fn java_value_namespace_and_array_bindings_do_not_become_type_receivers() {
    for source in [
        "package lab; record Page(AuditStore DocumentStore) { int read() { return DocumentStore.getById(1); } }",
        "package lab; enum Page { DocumentStore; int getById(int id) { return id; } int read() { return DocumentStore.getById(1); } }",
        "package lab; class Page { DocumentStore item; int read() { try (AuditStore item = new AuditStore()) { return item.getById(1); } } }",
        "package lab; class Page { int read(DocumentStore values[]) { return values.clone().length; } }",
        "package lab; class Page { DocumentStore values[]; int read() { DocumentStore local[] = values; return values.clone().length + local.clone().length; } }",
    ] {
        let audit = AUDIT.replace("final class AuditStore {", "final class AuditStore implements AutoCloseable {")
            .replace("  public String label()", "  public void close() {}\n  public String label()");
        let ws = Workspace::new(&[("DocumentStore.java", DOCUMENTS), ("AuditStore.java", &audit), ("Page.java", source), ("KnownPage.java", KNOWN_PAGE)]);
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_known_call(graph, "KnownPage.java");
        assert_eq!(calls(graph, "lab.DocumentStore"), 1, "{source}");
        if source.contains(".clone()") {
            let array_calls: Vec<_> = graph.iter_nodes().filter(|node| node.kind == NodeKind::CallSite && node.name == "clone").collect();
            assert!(!array_calls.is_empty(), "array clone calls must be extracted");
            assert!(array_calls.iter().all(|node| node.owner_type.is_none()), "an array is not an instance of its element type");
        }
    }
}

#[path = "java_binding_safety_tests.rs"]
mod safety;

#[test]
fn java_object_overrides_refuse_even_without_an_explicit_superclass() {
    for annotation in ["", "@Override "] {
        let source = format!("package lab; class Store {{ {annotation}public String toString() {{ return \"store\"; }} }} class Page {{ String read(Store store) {{ return store.toString(); }} }}");
        let ws = Workspace::new(&[("Store.java", &source)]);
        let result = ws.rename("Store.toString", "Store.java", None, None);
        assert_eq!(
            s(field(&result, "result")),
            "ambiguous_symbol",
            "{result:?}"
        );
        assert_eq!(ws.read("Store.java"), source);
    }
}
