//! A restored or compacted graph must answer every read exactly like the
//! graph that was edited live, and a clone must never see another clone's
//! edits through the shared base generation.

use std::collections::BTreeSet;

use super::*;

/// One name's lookups: all nodes, call sites, and name-addressable references.
type NameRow = (String, Vec<NodeId>, Vec<NodeId>, Vec<(NodeId, FileId)>);

/// Every observable fact: nodes, both adjacency directions, file
/// membership, name lookups, and the serialized snapshot itself.
#[derive(Debug, PartialEq, Eq)]
struct View {
    nodes: Vec<(NodeId, &'static str, String, FileId, String, u32)>,
    outgoing: Vec<(NodeId, Vec<(NodeId, &'static str)>)>,
    incoming: BTreeSet<(NodeId, NodeId, &'static str)>,
    files: Vec<(FileId, Vec<NodeId>)>,
    names: Vec<NameRow>,
    snapshot: String,
}

fn view(graph: &SymbolGraph) -> View {
    let ids = graph.all_node_ids();
    assert_eq!(ids.len(), graph.node_count());
    let nodes = ids
        .iter()
        .map(|id| {
            let node = graph.node(*id).unwrap();
            (
                node.id,
                node.kind.as_str(),
                node.name.clone(),
                node.file_id,
                node.path.to_string(),
                node.line,
            )
        })
        .collect();
    // Outgoing order is part of the contract: it is what a snapshot stores.
    let outgoing = ids
        .iter()
        .map(|id| {
            let edges = graph.outgoing(*id).iter();
            (*id, edges.map(|e| (e.to, e.kind.as_str())).collect())
        })
        .collect();
    // Incoming order is rebuilt on restore, so only membership is compared.
    let incoming = ids
        .iter()
        .flat_map(|id| graph.incoming(*id).iter())
        .map(|e| (e.from, e.to, e.kind.as_str()))
        .collect();
    let files = graph
        .file_ids()
        .into_iter()
        .map(|file| {
            let mut members: Vec<NodeId> = graph
                .iter_nodes()
                .filter(|n| n.file_id == file)
                .map(|n| n.id)
                .collect();
            members.sort_unstable();
            (file, members)
        })
        .collect();
    let distinct: BTreeSet<String> = graph.iter_nodes().map(|n| n.name.clone()).collect();
    let names = distinct
        .into_iter()
        .map(|name| {
            let sorted = |mut ids: Vec<NodeId>| {
                ids.sort_unstable();
                ids
            };
            let mut references = graph.names.references(&name).to_vec();
            references.sort_unstable();
            (
                name.clone(),
                sorted(graph.nodes_named(&name).to_vec()),
                sorted(graph.names.call_sites(&name).to_vec()),
                references,
            )
        })
        .collect();
    let snapshot = graph.snapshot();
    View {
        nodes,
        outgoing,
        incoming,
        files,
        names,
        snapshot: serde_json::to_string(&(snapshot.next_id, &snapshot.nodes, &snapshot.edges))
            .unwrap(),
    }
}

fn seeded() -> SymbolGraph {
    let mut graph = SymbolGraph::new();
    graph.rebuild_file(
        1,
        "a.rs",
        Language::Rust,
        "fn alpha() {}\nfn shared() {}",
        &[],
        &[],
    );
    graph.rebuild_file(
        2,
        "b.rs",
        Language::Rust,
        "struct Beta { shared: u32 }\nfn beta() { alpha(); shared(); }",
        &[],
        &[1],
    );
    graph.rebuild_file(
        3,
        "c.rs",
        Language::Rust,
        "fn gamma() { beta(); alpha(); }",
        &[],
        &[1, 2],
    );
    graph
}

/// The same edits applied to any representation of the seeded graph.
fn edit(graph: &mut SymbolGraph) {
    graph.rebuild_file(
        2,
        "b.rs",
        Language::Rust,
        "fn beta() { gamma(); }\nfn extra() { alpha(); }",
        &[],
        &[1, 3],
    );
    graph.remove_file(3);
    graph.rebuild_file(
        4,
        "d.rs",
        Language::Rust,
        "fn delta() { extra(); beta(); }",
        &[],
        &[2],
    );
    let sites = graph.take_dirty_call_sites();
    let visible = [
        (4, std::iter::once(2).collect()),
        (2, std::iter::once(1).collect()),
    ]
    .into_iter()
    .collect();
    graph.resolve_call_sites(&sites, &visible);
}

fn restored(graph: &SymbolGraph) -> SymbolGraph {
    SymbolGraph::from_snapshot(graph.snapshot()).unwrap()
}

#[test]
fn a_restored_graph_answers_like_the_live_one_before_and_after_edits() {
    let mut live = seeded();
    let mut restored = restored(&live);
    assert_eq!(view(&restored), view(&live));
    edit(&mut live);
    edit(&mut restored);
    assert_eq!(view(&restored), view(&live));
    // The negative control: the edits changed what every view reports.
    assert_ne!(view(&live), view(&seeded()));
}

#[test]
fn compaction_preserves_every_read() {
    let mut graph = restored(&seeded());
    edit(&mut graph);
    let before = view(&graph);
    graph.compact();
    assert_eq!(view(&graph), before);
    assert_eq!(
        graph.store.delta_weight(),
        0,
        "compaction empties the delta"
    );
    // Edits after compaction still land correctly.
    let mut live = seeded();
    edit(&mut live);
    graph.remove_file(4);
    live.remove_file(4);
    assert_eq!(view(&graph), view(&live));
}

#[test]
fn clones_share_the_base_without_seeing_each_others_edits() {
    let original = restored(&seeded());
    let before = view(&original);
    let mut branch = original.clone();
    edit(&mut branch);
    assert_eq!(view(&original), before);
    let mut expected = seeded();
    edit(&mut expected);
    assert_eq!(view(&branch), view(&expected));
}

#[test]
fn restore_refuses_edges_to_absent_nodes() {
    let mut snapshot = seeded().snapshot();
    let missing = snapshot.next_id - 1;
    snapshot.nodes.retain(|node| node.id != missing);
    assert!(snapshot
        .edges
        .iter()
        .any(|e| e.from == missing || e.to == missing));
    assert!(SymbolGraph::from_snapshot(snapshot).is_err());
}

#[test]
fn restore_regroups_edges_that_are_not_listed_by_source() {
    let live = seeded();
    let mut snapshot = live.snapshot();
    snapshot.edges.reverse();
    snapshot.nodes.reverse();
    let graph = SymbolGraph::from_snapshot(snapshot).unwrap();
    let mut expected = view(&live);
    let mut actual = view(&graph);
    // Reversal reverses each source's outgoing order; compare as sets.
    for (_, edges) in expected
        .outgoing
        .iter_mut()
        .chain(actual.outgoing.iter_mut())
    {
        edges.sort_unstable();
    }
    expected.snapshot.clear();
    actual.snapshot.clear();
    assert_eq!(actual, expected);
}
