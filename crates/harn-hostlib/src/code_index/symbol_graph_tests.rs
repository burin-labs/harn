use super::*;

#[test]
fn add_and_remove_round_trip() {
    let mut g = SymbolGraph::new();
    let outcome = g.rebuild_file(1, "src/a.rs", Language::Rust, "fn foo() {}\n", &[], &[]);
    assert!(
        outcome.node_count >= 2,
        "module + function expected, got {}",
        outcome.node_count
    );
    assert!(
        outcome.symbols.iter().any(|s| s.name == "foo"),
        "rebuild_file should surface the parsed `foo` symbol"
    );
    assert!(!g.nodes_named("foo").is_empty());
    g.remove_file(1);
    assert_eq!(g.node_count(), 0);
    assert!(g.nodes_named("foo").is_empty());
}

#[test]
fn rebuild_file_emits_function_module_and_call_nodes() {
    let mut g = SymbolGraph::new();
    let src = "fn alpha() {}\nfn beta() { alpha(); }\n";
    let outcome = g.rebuild_file(7, "src/x.rs", Language::Rust, src, &[], &[]);
    assert!(
        outcome.node_count >= 3,
        "expected module + 2 functions, got {}",
        outcome.node_count
    );
    let alpha_funcs: Vec<_> = g
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::Function && n.name == "alpha")
        .collect();
    assert_eq!(alpha_funcs.len(), 1);
    let beta_funcs: Vec<_> = g
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::Function && n.name == "beta")
        .collect();
    assert_eq!(beta_funcs.len(), 1);
    let beta_calls: Vec<_> = g
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::CallSite && n.name == "alpha")
        .collect();
    assert!(!beta_calls.is_empty(), "expected a CallSite for alpha()");
}

#[test]
fn rebuild_file_emits_fields_and_enum_cases() {
    let mut g = SymbolGraph::new();
    let src = "pub struct Greeter {\n    pub name: String,\n}\n\nenum Color {\n    Red,\n}\n";
    g.rebuild_file(9, "src/lib.rs", Language::Rust, src, &[], &[]);

    let field = g
        .iter_nodes()
        .find(|n| n.kind == NodeKind::Field && n.name == "name")
        .expect("expected public field node");
    assert_eq!(field.container.as_deref(), Some("Greeter"));
    assert_eq!(field.access_level.as_deref(), Some("public"));

    let case = g
        .iter_nodes()
        .find(|n| n.kind == NodeKind::EnumCase && n.name == "Red")
        .expect("expected enum case node");
    assert_eq!(case.container.as_deref(), Some("Color"));

    let color = g
        .iter_nodes()
        .find(|n| n.kind == NodeKind::Type && n.name == "Color")
        .expect("expected enum type node");
    assert!(
        g.outgoing(color.id)
            .iter()
            .any(|edge| edge.kind == EdgeKind::Contains && edge.to == case.id),
        "enum type should contain its case"
    );
}

#[test]
fn called_by_inverse_label_resolves() {
    let (kind, reversed) = EdgeKind::parse_with_direction("CALLED_BY").unwrap();
    assert_eq!(kind, EdgeKind::Calls);
    assert!(reversed);
    let (kind, reversed) = EdgeKind::parse_with_direction("CALLS").unwrap();
    assert_eq!(kind, EdgeKind::Calls);
    assert!(!reversed);
}

/// The `REFS` name heuristic must point only at declarations another
/// file could name. Before this was enforced, a call site was a legal
/// target, so a workspace with N call sites of a popular name grew
/// REFS quadratically: 80.9M of 87.0M edges on a real 7,038-file
/// workspace pointed at call sites (#8081).
#[test]
fn refs_never_point_at_a_call_site() {
    let mut g = SymbolGraph::new();
    // One declaration of `helper`, in its own file.
    g.rebuild_file(
        1,
        "src/decl.rs",
        Language::Rust,
        "pub fn helper() -> i32 { 1 }\n",
        &[],
        &[],
    );
    // A file full of calls to it. Each call is a CallSite node named
    // `helper`, and each is a candidate REFS target under the old rule.
    g.rebuild_file(
        2,
        "src/uses.rs",
        Language::Rust,
        "fn a() { helper(); helper(); helper(); }\n",
        &[],
        &[],
    );
    // A third file that merely mentions the word.
    g.rebuild_file(
        3,
        "src/mentions.rs",
        Language::Rust,
        "fn b() { let _ = \"helper\"; helper(); }\n",
        &[],
        &[],
    );

    // Positive control: the heuristic still fires. The mentioning
    // module reaches the real declaration.
    let decl = *g
        .nodes_named("helper")
        .iter()
        .find(|id| g.node(**id).is_some_and(|n| n.kind == NodeKind::Function))
        .expect("the function declaration exists");
    let mentions_mod = g.module_node_for_file(3).unwrap();
    assert!(
        g.outgoing(mentions_mod)
            .iter()
            .any(|e| e.kind == EdgeKind::Refs && e.to == decl),
        "a module naming a cross-file function must still get a REFS edge"
    );

    // The property: no REFS edge anywhere lands on a non-addressable
    // node. Asserted over the whole graph, not just the one module,
    // so a future kind cannot quietly re-enter through another path.
    let call_sites = g
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::CallSite)
        .count();
    assert!(
        call_sites >= 4,
        "fixture must contain call sites to exclude"
    );
    for id in g.all_node_ids() {
        for edge in g.outgoing(id) {
            if edge.kind != EdgeKind::Refs {
                continue;
            }
            let target = g.node(edge.to).expect("edge target exists");
            assert!(
                target.kind.is_name_addressable(),
                "REFS edge points at a {} node named `{}`, which no bare \
                 identifier in another file can be naming",
                target.kind.as_str(),
                target.name
            );
        }
    }
}

/// A field name is scoped to its container, so a module that merely
/// contains the same word is not referencing it.
#[test]
fn refs_never_point_at_a_field_or_enum_case() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(
        1,
        "src/model.rs",
        Language::Rust,
        "pub struct Doc { pub path: String }\npub enum Mode { Fastpath }\n",
        &[],
        &[],
    );
    g.rebuild_file(
        2,
        "src/other.rs",
        Language::Rust,
        "fn go() { let path = 1; let Fastpath = 2; }\n",
        &[],
        &[],
    );
    assert!(
        g.iter_nodes().any(|n| n.kind == NodeKind::Field),
        "fixture must declare a field to exclude"
    );
    let other_mod = g.module_node_for_file(2).unwrap();
    for edge in g.outgoing(other_mod) {
        if edge.kind != EdgeKind::Refs {
            continue;
        }
        let target = g.node(edge.to).unwrap();
        assert!(
            target.kind.is_name_addressable(),
            "REFS edge points at a container-scoped {} named `{}`",
            target.kind.as_str(),
            target.name
        );
    }
}

/// Every kind is classified deliberately. A kind added later fails
/// this test until someone decides which side it belongs on, rather
/// than defaulting into the heuristic and re-opening #8081.
#[test]
fn every_node_kind_has_a_deliberate_addressability_verdict() {
    for kind in NodeKind::ALL {
        let expected = match kind {
            NodeKind::Function | NodeKind::Type | NodeKind::Macro | NodeKind::Module => true,
            NodeKind::Field | NodeKind::EnumCase | NodeKind::CallSite | NodeKind::Import => false,
        };
        assert_eq!(
            kind.is_name_addressable(),
            expected,
            "{} changed sides; decide deliberately and update #8081's reasoning",
            kind.as_str()
        );
    }
}

/// A call must not link to an identically-named function the caller
/// neither defines nor imports. Before this, every function sharing
/// the callee's name was a target, so on a 7,038-file workspace one
/// function node collected 32,887 callers and six of every seven
/// edges were wrong (#8107).
#[test]
fn a_call_does_not_reach_an_unimported_same_named_function() {
    let mut g = SymbolGraph::new();
    // Three unrelated files each declaring `assert`, plus a caller
    // that imports exactly one of them.
    g.rebuild_file(1, "a.rs", Language::Rust, "pub fn assert() {}\n", &[], &[]);
    g.rebuild_file(2, "b.rs", Language::Rust, "pub fn assert() {}\n", &[], &[]);
    g.rebuild_file(3, "c.rs", Language::Rust, "pub fn assert() {}\n", &[], &[]);
    g.rebuild_file(
        4,
        "caller.rs",
        Language::Rust,
        "use crate::b::assert;\nfn go() { assert(); }\n",
        &["crate::b".into()],
        &[2],
    );

    let call = *g
        .nodes_named("assert")
        .iter()
        .find(|id| g.node(**id).is_some_and(|n| n.kind == NodeKind::CallSite))
        .expect("the call site exists");
    let targets: Vec<&str> = g
        .outgoing(call)
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .filter_map(|e| g.node(e.to))
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(
        targets,
        vec!["b.rs"],
        "a call must reach only the declaration its file imports"
    );

    // The negative control is the whole point: the other two
    // declarations must have gained no caller at all.
    for path in ["a.rs", "c.rs"] {
        let decl = *g
            .nodes_named("assert")
            .iter()
            .find(|id| {
                g.node(**id)
                    .is_some_and(|n| n.kind == NodeKind::Function && n.path == path)
            })
            .expect("declaration exists");
        assert!(
            g.incoming(decl).iter().all(|e| e.kind != EdgeKind::Calls),
            "{path} was never imported by the caller and must have no CALLS edge"
        );
    }
}

/// A definition in the calling file wins over anything imported,
/// because that is what the language does.
#[test]
fn a_local_definition_shadows_an_imported_one() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(
        1,
        "dep.rs",
        Language::Rust,
        "pub fn helper() {}\n",
        &[],
        &[],
    );
    g.rebuild_file(
        2,
        "local.rs",
        Language::Rust,
        "use crate::dep::helper;\nfn helper() {}\nfn go() { helper(); }\n",
        &["crate::dep".into()],
        &[1],
    );

    let call = *g
        .nodes_named("helper")
        .iter()
        .find(|id| g.node(**id).is_some_and(|n| n.kind == NodeKind::CallSite))
        .expect("call site");
    let targets: Vec<&str> = g
        .outgoing(call)
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .filter_map(|e| g.node(e.to))
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(targets, vec!["local.rs"]);
}

/// Recall guard. A name with exactly one declaration anywhere is
/// unambiguous, so it still resolves even when no import edge was
/// recorded — a sibling module in the same crate, a global, or a
/// language with no import syntax. Without this the fix would trade
/// one silent wrongness for another.
#[test]
fn a_unique_name_resolves_without_an_import_edge() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(
        1,
        "only.rs",
        Language::Rust,
        "pub fn one_of_a_kind() {}\n",
        &[],
        &[],
    );
    g.rebuild_file(
        2,
        "caller.rs",
        Language::Rust,
        "fn go() { one_of_a_kind(); }\n",
        &[],
        &[],
    );

    let call = *g
        .nodes_named("one_of_a_kind")
        .iter()
        .find(|id| g.node(**id).is_some_and(|n| n.kind == NodeKind::CallSite))
        .expect("call site");
    let targets: Vec<&str> = g
        .outgoing(call)
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .filter_map(|e| g.node(e.to))
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(
        targets,
        vec!["only.rs"],
        "an unambiguous name must still resolve across files"
    );
}

/// Ambiguous and unimportable is the one case where the old code
/// invented edges. It must now produce none rather than guess.
#[test]
fn an_ambiguous_unimported_call_produces_no_edge() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(1, "a.rs", Language::Rust, "pub fn run() {}\n", &[], &[]);
    g.rebuild_file(2, "b.rs", Language::Rust, "pub fn run() {}\n", &[], &[]);
    g.rebuild_file(
        3,
        "caller.rs",
        Language::Rust,
        "fn go() { run(); }\n",
        &[],
        &[],
    );

    let call = *g
        .nodes_named("run")
        .iter()
        .find(|id| g.node(**id).is_some_and(|n| n.kind == NodeKind::CallSite))
        .expect("call site");
    let calls: Vec<_> = g
        .outgoing(call)
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .collect();
    assert!(
        calls.is_empty(),
        "two candidates and no import is a guess, not a resolution; got {} edges",
        calls.len()
    );
}

#[test]
fn link_imports_creates_module_to_module_edges() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(
        1,
        "src/a.ts",
        Language::TypeScript,
        "import { x } from \"./b\";\n",
        &["./b".into()],
        &[],
    );
    g.rebuild_file(
        2,
        "src/b.ts",
        Language::TypeScript,
        "export const x = 1;\n",
        &[],
        &[],
    );
    let mut resolved: HashMap<FileId, Vec<FileId>> = HashMap::new();
    resolved.insert(1, vec![2]);
    g.link_imports(&resolved);
    let a_mod = g.module_node_for_file(1).unwrap();
    let b_mod = g.module_node_for_file(2).unwrap();
    let edge_exists = g
        .outgoing(a_mod)
        .iter()
        .any(|e| e.kind == EdgeKind::Imports && e.to == b_mod);
    assert!(edge_exists, "expected Module→Module IMPORTS edge");
}

#[test]
fn link_imports_is_idempotent_across_repeated_relinks() {
    let mut g = SymbolGraph::new();
    g.rebuild_file(
        1,
        "src/a.ts",
        Language::TypeScript,
        "import { x } from \"./b\";\n",
        &["./b".into()],
        &[],
    );
    g.rebuild_file(
        2,
        "src/b.ts",
        Language::TypeScript,
        "export const x = 1;\n",
        &[],
        &[],
    );
    let mut resolved: HashMap<FileId, Vec<FileId>> = HashMap::new();
    resolved.insert(1, vec![2]);
    // `link_imports` re-runs over the whole workspace after every per-file
    // reindex, so relinking three times must not accumulate duplicate
    // Module→Module IMPORTS edges.
    g.link_imports(&resolved);
    g.link_imports(&resolved);
    g.link_imports(&resolved);
    let a_mod = g.module_node_for_file(1).unwrap();
    let b_mod = g.module_node_for_file(2).unwrap();
    let module_import_edges = g
        .outgoing(a_mod)
        .iter()
        .filter(|e| e.kind == EdgeKind::Imports && e.to == b_mod)
        .count();
    assert_eq!(
        module_import_edges, 1,
        "Module→Module IMPORTS edge must not duplicate across relinks"
    );
}

#[test]
fn harn_reference_projection_replaces_stale_edges_and_keeps_names_separate() {
    let mut graph = SymbolGraph::new();
    graph.rebuild_file(1, "a.harn", Language::Harn, "fn run() { 1 }", &[], &[]);
    graph.rebuild_file(2, "b.harn", Language::Harn, "fn run() { 2 }", &[], &[]);
    graph.rebuild_file(
        3,
        "use.harn",
        Language::Harn,
        "fn use_it() { run() }",
        &[],
        &[],
    );
    graph.replace_harn_references(&[ResolvedHarnReference {
        from_path: "use.harn".into(),
        to_path: "a.harn".into(),
        to_name: "run".into(),
    }]);
    let module = graph.module_node_for_file(3).unwrap();
    let target_path = |graph: &SymbolGraph| {
        graph
            .outgoing(module)
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Refs)
            .map(|edge| graph.node(edge.to).unwrap().path.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(target_path(&graph), vec!["a.harn"]);

    graph.replace_harn_references(&[ResolvedHarnReference {
        from_path: "use.harn".into(),
        to_path: "b.harn".into(),
        to_name: "run".into(),
    }]);
    assert_eq!(target_path(&graph), vec!["b.harn"]);
}
