//! REFS edges must not depend on the order files reach the index.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::code_index::{EdgeKind, IndexState, NodeKind};

fn write(root: &Path, path: &str, body: &str) {
    let abs = root.join(path);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

/// Paths of the modules holding a REFS edge into the function `name`.
fn referrers(state: &IndexState, name: &str) -> Vec<String> {
    let decl = state
        .symbols
        .nodes_named(name)
        .iter()
        .copied()
        .find(|id| {
            state
                .symbols
                .node(*id)
                .is_some_and(|n| n.kind == NodeKind::Function)
        })
        .unwrap_or_else(|| panic!("no function `{name}` in the graph"));
    let mut out: Vec<String> = state
        .symbols
        .incoming(decl)
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Refs)
        .filter_map(|edge| state.symbols.node(edge.from))
        .map(|node| node.path.clone())
        .collect();
    out.sort();
    out
}

#[test]
fn refs_reach_a_declaration_ingested_after_its_referrer() {
    // The walk is sorted, so `a_use.py` is ingested before `z_decl.py`.
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "a_use.py",
        "from z_decl import fetch_rows\n\nfetch_rows()\n",
    );
    write(root, "z_decl.py", "def fetch_rows():\n    return []\n");
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(
        referrers(&state, "fetch_rows"),
        vec!["a_use.py".to_string()]
    );
}

#[test]
fn refs_reach_a_declaration_ingested_before_its_referrer() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", "def fetch_rows():\n    return []\n");
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\nfetch_rows()\n",
    );
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(
        referrers(&state, "fetch_rows"),
        vec!["z_use.py".to_string()]
    );
}

#[test]
fn reindexing_the_declaring_file_keeps_its_incoming_refs() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", "def fetch_rows():\n    return []\n");
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\nfetch_rows()\n",
    );
    let (mut state, _) = IndexState::build_from_root(root);

    write(root, "a_decl.py", "def fetch_rows():\n    return [1]\n");
    state.reindex_file(&root.join("a_decl.py")).unwrap();
    assert_eq!(
        referrers(&state, "fetch_rows"),
        vec!["z_use.py".to_string()]
    );
}

#[test]
fn a_declaration_added_by_reindex_gains_existing_referrers() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", "def other():\n    return []\n");
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\nfetch_rows()\n",
    );
    let (mut state, _) = IndexState::build_from_root(root);

    write(
        root,
        "a_decl.py",
        "def other():\n    return []\n\n\ndef fetch_rows():\n    return []\n",
    );
    state.reindex_file(&root.join("a_decl.py")).unwrap();
    assert_eq!(
        referrers(&state, "fetch_rows"),
        vec!["z_use.py".to_string()]
    );
}

#[test]
fn refresh_from_root_links_a_newly_added_declaring_file() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\nfetch_rows()\n",
    );
    let (mut state, _) = IndexState::build_from_root(root);

    write(root, "a_decl.py", "def fetch_rows():\n    return []\n");
    state.refresh_from_root(None);
    assert_eq!(
        referrers(&state, "fetch_rows"),
        vec!["z_use.py".to_string()]
    );
}

#[test]
fn a_referrer_that_stops_naming_the_declaration_loses_its_edge() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", "def fetch_rows():\n    return []\n");
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\nfetch_rows()\n",
    );
    let (mut state, _) = IndexState::build_from_root(root);

    write(root, "z_use.py", "print(1)\n");
    state.reindex_file(&root.join("z_use.py")).unwrap();
    assert!(referrers(&state, "fetch_rows").is_empty());
}
