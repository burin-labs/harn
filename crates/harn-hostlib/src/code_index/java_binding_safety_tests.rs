use super::*;

#[test]
fn explicit_java_path_never_falls_back_to_an_unrelated_language() {
    let typescript = "export function getById() { return 1; }\nexport const result = getById();\n";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/KnownPage.java", KNOWN_PAGE),
        ("src/a.ts", typescript),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_known_call(graph, "lab/KnownPage.java");
        assert_eq!(
            graph
                .iter_nodes()
                .filter(|node| node.kind == NodeKind::Function && node.name == "getById")
                .count(),
            2
        );
        assert!(matches!(
            super::super::refactor_core::resolve_seed_selected(
                graph,
                "missing.ts",
                "getById",
                None,
                None,
                None
            ),
            super::super::refactor_core::SeedLookup::None
        ));
        assert!(matches!(
            super::super::refactor_core::resolve_seed(graph, "missing.ts", "getById", None, None),
            super::super::refactor_core::SeedLookup::One(_)
        ));
    }
    let result = ws.rename("getById", "missing.java", None, None);
    assert_eq!(s(field(&result, "result")), "no_match", "{result:?}");
    ws.assert_unchanged();
}

fn assert_refused(ws: &Workspace) {
    let result = ws.rename(
        "DocumentStore.getById",
        "lab/DocumentStore.java",
        None,
        None,
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
    ws.assert_unchanged();
}

fn assert_only_known_call(ws: &Workspace) {
    let guard = ws.index.lock().unwrap();
    let graph = &guard.as_ref().unwrap().symbols;
    assert_known_call(graph, "lab/KnownPage.java");
    assert_eq!(calls(graph, "lab.DocumentStore"), 1);
    let sites: Vec<_> = graph
        .iter_nodes()
        .filter(|node| {
            node.kind == NodeKind::CallSite
                && node.name == "getById"
                && node.path.as_ref() == "lab/Page.java"
        })
        .collect();
    assert_eq!(sites.len(), 1, "the uncertain call is present");
    assert!(sites[0].owner_type.is_none());
}

#[test]
fn switch_local_shadows_refuse_without_losing_the_concrete_call() {
    for page in [
        "package lab; final class Page { DocumentStore store; int read(int k, AuditStore a) { switch (k) { case 1: AuditStore store = a; return store.getById(1); default: return 0; } } }",
        "package lab; final class Page { DocumentStore store; int read(int k, AuditStore a) { switch (k) { case 0: AuditStore store = a; return 0; case 1: store = a; return store.getById(1); default: return 0; } } }",
    ] {
        let ws = Workspace::new(&[("lab/DocumentStore.java", DOCUMENTS), ("lab/AuditStore.java", AUDIT), ("lab/Page.java", page), ("lab/KnownPage.java", KNOWN_PAGE)]);
        assert_only_known_call(&ws);
        assert_refused(&ws);
    }
}

#[test]
fn inherited_member_types_refuse_package_name_guesses() {
    for (base, page) in [
        ("package lab; class Base { static final class DocumentStore { int getById(int id) { return id; } } }", "package lab; final class Page extends Base { DocumentStore store; int read() { return store.getById(1); } }"),
        ("package lab; interface Base { final class DocumentStore { int getById(int id) { return id; } } }", "package lab; final class Page implements Base { DocumentStore store; int read() { return store.getById(1); } }"),
    ] {
        let ws = Workspace::new(&[("lab/DocumentStore.java", DOCUMENTS), ("lab/Base.java", base), ("lab/Page.java", page), ("lab/KnownPage.java", KNOWN_PAGE)]);
        assert_only_known_call(&ws);
        assert_refused(&ws);
    }
    let outer = "package lab; class Outer extends Object { final class DocumentStore { int getById(int id) { return id; } DocumentStore next; int read() { return next.getById(1); } } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/Outer.java", outer),
        ("lab/KnownPage.java", KNOWN_PAGE),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        let graph = &guard.as_ref().unwrap().symbols;
        assert_known_call(graph, "lab/KnownPage.java");
        assert_eq!(calls(graph, "lab.DocumentStore"), 1);
    }
    assert_refused(&ws);
}

#[test]
fn subtype_dispatch_refuses_even_when_the_subtype_has_no_old_name() {
    let documents = DOCUMENTS.replace("final class", "class");
    let special = "package lab; class Special extends DocumentStore { public int loadById(int id) { return -id; } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", &documents),
        ("lab/Special.java", special),
        ("lab/KnownPage.java", KNOWN_PAGE),
    ]);
    {
        let guard = ws.index.lock().unwrap();
        assert_known_call(&guard.as_ref().unwrap().symbols, "lab/KnownPage.java");
    }
    assert_refused(&ws);

    let outer = "package lab; class Outer { private static class DocumentStore { int getById(int id) { return id; } int read() { return getById(1); } } private static class Special extends DocumentStore { int loadById(int id) { return -id; } } }";
    let ws = Workspace::new(&[("lab/Outer.java", outer)]);
    assert_eq!(
        calls(
            &ws.index.lock().unwrap().as_ref().unwrap().symbols,
            "lab.Outer.DocumentStore"
        ),
        1
    );
    let result = ws.rename(
        "lab.Outer.DocumentStore.getById",
        "lab/Outer.java",
        None,
        None,
    );
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    ws.assert_unchanged();

    let private = "package lab; public class DocumentStore { private int getById(int id) { return id; } public int read() { return getById(1); } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", private),
        ("lab/Special.java", special),
    ]);
    assert_eq!(
        calls(
            &ws.index.lock().unwrap().as_ref().unwrap().symbols,
            "lab.DocumentStore"
        ),
        1
    );
    let result = ws.rename(
        "DocumentStore.getById",
        "lab/DocumentStore.java",
        None,
        None,
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(n(field(&result, "match_count")), 2);
    assert_eq!(
        ws.read("lab/DocumentStore.java"),
        private.replace("getById", "loadById")
    );
    assert_eq!(ws.read("lab/Special.java"), special);
}

#[test]
fn proposed_object_method_names_refuse_with_a_reached_original_call() {
    let documents = "package lab; public final class DocumentStore { public String describe() { return \"d\"; } }";
    let page = "package lab; final class Page { String read(DocumentStore documents) { return documents.describe(); } }";
    for new_name in [
        "toString",
        "equals",
        "hashCode",
        "clone",
        "finalize",
        "getClass",
        "notify",
        "notifyAll",
        "wait",
    ] {
        let ws = Workspace::new(&[
            ("lab/DocumentStore.java", documents),
            ("lab/Page.java", page),
        ]);
        {
            let guard = ws.index.lock().unwrap();
            let graph = &guard.as_ref().unwrap().symbols;
            let method = graph.java_method("describe", "lab.DocumentStore").unwrap();
            assert_eq!(
                graph
                    .incoming(method.id)
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::Calls)
                    .count(),
                1
            );
        }
        let result = super::super::rename::run(
            &ws.index,
            &[dict(&[
                (
                    "symbol_ref",
                    dict(&[
                        ("name", vm_string("DocumentStore.describe")),
                        ("path", vm_string("lab/DocumentStore.java")),
                    ]),
                ),
                ("new_name", vm_string(new_name)),
                ("scope", vm_string("workspace")),
                ("backend", vm_string("syntax_tree")),
            ])],
        )
        .unwrap();
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
        ws.assert_unchanged();
    }
}

#[test]
fn unsupported_local_bindings_stop_before_field_fallback() {
    let closeable = AUDIT
        .replace(
            "final class AuditStore {",
            "final class AuditStore implements AutoCloseable {",
        )
        .replace(
            "  public String label()",
            "  public void close() {}\n  public String label()",
        );
    let exception = AUDIT.replace(
        "final class AuditStore {",
        "final class AuditStore extends Exception {",
    );
    for (audit, page) in [
        (AUDIT, "package lab; class Page { DocumentStore item; java.util.function.Function<AuditStore, Integer> read() { return item -> item.getById(1); } }"),
        (AUDIT, "package lab; class Page { DocumentStore item; int read(AuditStore[] items) { for (AuditStore item : items) { return item.getById(1); } return 0; } }"),
        (exception.as_str(), "package lab; class Page { DocumentStore item; int read() { try { throw new AuditStore(); } catch (AuditStore item) { return item.getById(1); } } }"),
        (closeable.as_str(), "package lab; class Page { DocumentStore item; int read() { try (AuditStore item = new AuditStore()) { return item.getById(1); } } }"),
        (AUDIT, "package lab; class Page { DocumentStore item; int read(Object other) { if (other instanceof AuditStore item) { return item.getById(1); } return 0; } }"),
        (AUDIT, "package lab; record Page(AuditStore item) { int read() { return item.getById(1); } }"),
    ] {
        let ws = Workspace::new(&[("lab/DocumentStore.java", DOCUMENTS), ("lab/AuditStore.java", audit), ("lab/Page.java", page), ("lab/KnownPage.java", KNOWN_PAGE)]);
        assert_only_known_call(&ws);
        assert_refused(&ws);
    }
}

#[test]
fn snapshots_reject_unsafe_v10_edges_and_restore_populated_current_graphs() {
    use crate::code_index::snapshot::{CodeIndexSnapshot, SNAPSHOT_FORMAT_VERSION};
    use crate::code_index::symbol_graph::Edge;

    let page = "package lab; final class Page { DocumentStore store; int read(int k, AuditStore a) { switch (k) { case 1: AuditStore store = a; return store.getById(1); default: return 0; } } }";
    let ws = Workspace::new(&[
        ("lab/DocumentStore.java", DOCUMENTS),
        ("lab/AuditStore.java", AUDIT),
        ("lab/Page.java", page),
        ("lab/KnownPage.java", KNOWN_PAGE),
    ]);
    assert_only_known_call(&ws);
    let mut snapshot = ws.index.lock().unwrap().as_ref().unwrap().snapshot();
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
    let cached_call = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == false_call)
        .unwrap();
    assert_eq!(
        cached_call.owner_type.replace("lab.DocumentStore".into()),
        None
    );
    assert!(!graph
        .edges
        .iter()
        .any(|edge| edge.from == false_call && edge.to == target && edge.kind == EdgeKind::Calls));
    graph.edges.push(Edge {
        from: false_call,
        to: target,
        kind: EdgeKind::Calls,
    });
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.to == target && edge.kind == EdgeKind::Calls)
            .count(),
        2
    );
    snapshot.meta.format_version = 10;
    snapshot.save(ws.dir.path()).unwrap();
    let upgraded = CodeIndexCapability::new();
    assert!(!upgraded.restore_from_disk(ws.dir.path()).unwrap());
    assert!(upgraded.shared().lock().unwrap().is_none());
    upgraded.warm_session(ws.dir.path());
    upgraded.wait_until_idle();
    let guard = upgraded.shared();
    let guard = guard.lock().unwrap();
    let state = guard.as_ref().unwrap();
    assert_known_call(&state.symbols, "lab/KnownPage.java");
    assert_eq!(calls(&state.symbols, "lab.DocumentStore"), 1);
    let rebuilt_calls: Vec<_> = state
        .symbols
        .iter_nodes()
        .filter(|node| {
            node.kind == NodeKind::CallSite
                && node.path.as_ref() == "lab/Page.java"
                && node.name == "getById"
        })
        .collect();
    assert_eq!(rebuilt_calls.len(), 1);
    assert!(rebuilt_calls[0].owner_type.is_none());
    let rebuilt = CodeIndexSnapshot::load(ws.dir.path()).unwrap().unwrap();
    assert_eq!(rebuilt.meta.format_version, SNAPSHOT_FORMAT_VERSION);
}
