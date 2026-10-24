//! The fact pool must change only how fast an index is built, never what it
//! contains, and it must stay bounded and stoppable.

use std::fs;
use std::ops::ControlFlow;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::tempdir;

use super::{extract, publish_extracted_in_order, FileFacts, FileJob, QUEUED_FACTS_PER_WORKER};
use crate::code_index::{EdgeKind, IndexState, NodeKind};

fn write(root: &Path, path: &str, body: impl AsRef<[u8]>) {
    let abs = root.join(path);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

/// A workspace that exercises every fact kind: callers walked before and
/// after their declarations, REFS in both directions, imports, a file the
/// grammar rejects, files without a grammar, and an unreadable file.
fn mixed_workspace(root: &Path) {
    for i in 0..40 {
        // `a_*` callers sort before the `z_*` declarations they name.
        write(
            root,
            &format!("a_use_{i:02}.py"),
            format!("from z_decl_{i:02} import fetch_{i}\n\nfetch_{i}()\nshared_helper()\n"),
        );
        write(
            root,
            &format!("z_decl_{i:02}.py"),
            format!("def fetch_{i}():\n    return shared_helper()\n"),
        );
    }
    write(root, "m_shared.py", "def shared_helper():\n    return 1\n");
    write(
        root,
        "src/lib.rs",
        "mod util;\npub fn run() -> u32 { util::helper() + shared_helper() }\n",
    );
    write(root, "src/util.rs", "pub fn helper() -> u32 { 2 }\n");
    write(root, "src/broken.rs", "fn broken( {{{{ \n let = ;\n");
    write(root, "README.md", "# fetch_1 shared_helper\n");
    write(root, "config.json", "{\"shared_helper\": true}\n");
    write(root, "binary.py", [0xff, 0xfe, 0x00, 0x80]);
}

/// Every installed fact, including ids and posting order, in a form that
/// does not depend on hash-map iteration order.
fn exact_facts(state: &IndexState) -> String {
    let mut snapshot = state.snapshot();
    snapshot.files.sort_by_key(|file| file.id);
    for file in &mut snapshot.files {
        file.mtime_ms = 0;
    }
    snapshot
        .deps
        .sort_by_key(|row| serde_json::to_string(row).unwrap());
    serde_json::to_string(&(
        snapshot.next_file_id,
        &snapshot.files,
        &snapshot.trigrams,
        &snapshot.words,
        &snapshot.deps,
        &snapshot.symbols,
    ))
    .unwrap()
}

fn resolved_calls(state: &IndexState) -> usize {
    state
        .symbols
        .iter_nodes()
        .filter(|node| node.kind == NodeKind::CallSite)
        .map(|node| {
            state
                .symbols
                .outgoing(node.id)
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .count()
        })
        .sum()
}

#[test]
fn non_java_graph_nodes_keep_measured_declaration_name_positions() {
    for (path, language, source, name) in [
        (
            "entry.ts",
            crate::ast::Language::TypeScript,
            "@Component({})\nexport class Widget {}\n",
            "Widget",
        ),
        (
            "entry.swift",
            crate::ast::Language::Swift,
            "@MainActor\nclass Widget {}\n",
            "Widget",
        ),
        (
            "entry.py",
            crate::ast::Language::Python,
            "@decorate\ndef Widget():\n    pass\n",
            "Widget",
        ),
    ] {
        let facts = super::GraphFacts::extract(path, language, source);
        let symbol = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .unwrap();
        let dir = tempdir().unwrap();
        write(dir.path(), path, source);
        let (state, _) = IndexState::build_from_root(dir.path());
        let node = state
            .symbols
            .iter_nodes()
            .find(|node| node.name == name)
            .unwrap();
        let name_offset = source.find(name).unwrap();
        let position = crate::code_index::source_positions::LineIndex::new(source).position(
            name_offset,
            crate::code_index::source_positions::Encoding::Utf16,
        );
        let declaration = facts.declarations[&(symbol.start_row, symbol.start_col)]
            .iter()
            .find(|fact| fact.name == name)
            .unwrap();
        assert_eq!(
            (declaration.line, declaration.col),
            (position.line + 1, position.character + 1),
        );
        assert_eq!(node.line, position.line + 1, "{path}");
        assert_eq!(node.col, Some(position.character + 1), "{path}");
        assert_eq!(node.owner_type, None, "{path}");
    }
}

#[test]
fn exported_declarations_keep_measured_utf16_identifier_positions() {
    for (declaration, names, indexed) in [
        ("export class Span {}", &["Span"][..], true),
        ("export interface Span {}", &["Span"][..], true),
        ("export type Span = { start: number };", &["Span"][..], true),
        ("export function Span() {}", &["Span"][..], true),
        ("export const Span = () => 1;", &["Span"][..], true),
        (
            "export const left = () => 1, right = () => 2;",
            &["left", "right"][..],
            true,
        ),
        ("export const Span = 1;", &["Span"][..], false),
        ("type Span = number;", &["Span"][..], true),
    ] {
        let source = format!("/* 🦀 */ {declaration}\n");
        let facts =
            super::GraphFacts::extract("entry.ts", crate::ast::Language::TypeScript, &source);
        let dir = tempdir().unwrap();
        write(dir.path(), "entry.ts", &source);
        let (state, _) = IndexState::build_from_root(dir.path());
        for name in names {
            let at = source.find(name).unwrap();
            let col = source.get(..at).unwrap().encode_utf16().count() as u32 + 1;
            assert_ne!(col, at as u32 + 1, "the control must distinguish units");
            let symbols: Vec<_> = facts
                .symbols
                .iter()
                .filter(|symbol| symbol.name == *name)
                .collect();
            assert_eq!(symbols.len(), 1, "{declaration}: {name}");
            let symbol = symbols[0];
            let positions: Vec<_> = facts.declarations[&(symbol.start_row, symbol.start_col)]
                .iter()
                .filter(|fact| fact.name == *name)
                .collect();
            assert_eq!(positions.len(), 1, "{declaration}: {name}");
            assert_eq!((positions[0].line, positions[0].col), (1, col));
            let nodes: Vec<_> = state
                .symbols
                .iter_nodes()
                .filter(|node| node.name == *name)
                .collect();
            assert_eq!(nodes.len(), usize::from(indexed), "{declaration}: {name}");
            if indexed {
                assert_eq!((nodes[0].line, nodes[0].col), (1, Some(col)));
            }
        }
    }
}

#[test]
fn plain_enum_cases_keep_their_actual_utf16_name_positions() {
    use crate::code_index::cypher::{execute, CypherValue};

    let dir = tempdir().unwrap();
    let source = "/* 🦀 */ export enum State { Ready, Done = 1 }\n";
    write(dir.path(), "entry.ts", source);
    let (state, _) = IndexState::build_from_root(dir.path());
    for name in ["Ready", "Done"] {
        let offset = source.find(name).unwrap();
        let col = source.get(..offset).unwrap().encode_utf16().count() as i64 + 1;
        assert_ne!(col, offset as i64 + 1);
        let rows = execute(
            &format!("MATCH (d) WHERE d.name = '{name}' AND d.kind = 'EnumCase' RETURN d.line AS line, d.col AS col, d.qualified_name AS qualified"),
            &state.symbols,
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "{name}");
        assert_eq!(rows[0]["line"], CypherValue::Int(1));
        assert_eq!(rows[0]["col"], CypherValue::Int(col));
        assert_eq!(
            rows[0]["qualified"],
            CypherValue::String(format!("State.{name}"))
        );
    }
}

#[test]
fn non_java_name_positions_and_aliases_survive_query_and_snapshot_projection() {
    use crate::code_index::cypher::{execute, CypherValue};
    use crate::code_index::snapshot::{CodeIndexSnapshot, SNAPSHOT_FORMAT_VERSION};
    use crate::code_index::CodeIndexCapability;

    for (path, source) in [
        (
            "entry.rs",
            "/* 🦀 */ struct First; struct Second; impl First { fn load() {} } impl Second { fn load() {} }\n",
        ),
        (
            "entry.ts",
            "/* 🦀 */ class First { load() {} } class Second { load() {} }\n",
        ),
    ] {
        let dir = tempdir().unwrap();
        write(dir.path(), path, source);
        let (state, _) = IndexState::build_from_root(dir.path());
        let query = "MATCH (d) WHERE d.name = 'load' AND d.kind = 'Function' RETURN d.id AS id, d.path AS path, d.line AS line, d.col AS col, d.qualified_name AS qualified, d.receiver_qualified_name AS receiver";
        let before = execute(query, &state.symbols).unwrap();
        assert_eq!(before.len(), 2, "{path}");
        for (owner, offset) in [
            ("First", source.find("load").unwrap()),
            ("Second", source.rfind("load").unwrap()),
        ] {
            let col = source.get(..offset).unwrap().encode_utf16().count() as i64 + 1;
            assert_ne!(col, offset as i64 + 1);
            let alias = format!("{owner}.load");
            let rows = execute(
                &format!("MATCH (d) WHERE d.qualified_name = '{alias}' AND d.path = '{path}' AND d.line = 1 AND d.col = {col} AND d.kind = 'Function' RETURN d.receiver_qualified_name AS receiver"),
                &state.symbols,
            )
            .unwrap();
            assert_eq!(rows.len(), 1, "{path}: {alias}");
            assert_eq!(rows[0]["receiver"], CypherValue::String(alias));
            let wrong = execute(
                &format!("MATCH (d) WHERE d.name = 'load' AND d.path = '{path}' AND d.col = {} RETURN d.id AS id", col - 1),
                &state.symbols,
            )
            .unwrap();
            assert!(wrong.is_empty(), "whitespace must not select {path}: {owner}");
        }
        let mut snapshot = state.snapshot();
        snapshot.save(dir.path()).unwrap();
        let restored = CodeIndexCapability::new();
        assert!(restored.restore_from_disk(dir.path()).unwrap());
        restored.wait_until_idle();
        assert_eq!(
            execute(
                query,
                &restored.shared().lock().unwrap().as_ref().unwrap().symbols,
            )
            .unwrap(),
            before,
            "{path}",
        );
        for node in &mut snapshot.symbols.as_mut().unwrap().nodes {
            node.col = None;
        }
        snapshot.meta.format_version = 11;
        snapshot.save(dir.path()).unwrap();
        let upgraded = CodeIndexCapability::new();
        assert!(!upgraded.restore_from_disk(dir.path()).unwrap());
        assert!(upgraded.shared().lock().unwrap().is_none());
        upgraded.warm_session(dir.path());
        upgraded.wait_until_idle();
        let rebuilt = execute(
            query,
            &upgraded.shared().lock().unwrap().as_ref().unwrap().symbols,
        )
        .unwrap();
        assert_eq!(rebuilt, before, "{path}");
        assert_eq!(
            CodeIndexSnapshot::load(dir.path())
                .unwrap()
                .unwrap()
                .meta
                .format_version,
            SNAPSHOT_FORMAT_VERSION,
        );
    }
}

#[test]
fn snapshots_rebuild_v9_missing_positions_and_restore_current_java_positions() {
    use crate::code_index::snapshot::{CodeIndexSnapshot, SNAPSHOT_FORMAT_VERSION};
    use crate::code_index::CodeIndexCapability;

    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "Span.java",
        "package lab;\nfinal class Span {}\n",
    );
    let (state, _) = IndexState::build_from_root(dir.path());
    let span_col = |graph: &crate::code_index::SymbolGraph| {
        let nodes: Vec<_> = graph
            .iter_nodes()
            .filter(|node| {
                node.name == "Span" && node.kind == crate::code_index::symbol_graph::NodeKind::Type
            })
            .collect();
        assert_eq!(nodes.len(), 1);
        nodes[0].col
    };
    assert_eq!(span_col(&state.symbols), Some(13));
    let mut snapshot = state.snapshot();
    snapshot.save(dir.path()).unwrap();
    let restored = CodeIndexCapability::new();
    assert!(restored.restore_from_disk(dir.path()).unwrap());
    restored.wait_until_idle();
    assert_eq!(
        span_col(&restored.shared().lock().unwrap().as_ref().unwrap().symbols),
        Some(13)
    );
    let node = snapshot
        .symbols
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| {
            node.name == "Span" && node.kind == crate::code_index::symbol_graph::NodeKind::Type
        })
        .unwrap();
    assert_eq!(node.col.take(), Some(13));
    snapshot.meta.format_version = 9;
    snapshot.save(dir.path()).unwrap();
    let upgraded = CodeIndexCapability::new();
    assert!(!upgraded.restore_from_disk(dir.path()).unwrap());
    assert!(upgraded.shared().lock().unwrap().is_none());
    upgraded.warm_session(dir.path());
    upgraded.wait_until_idle();
    assert_eq!(
        span_col(&upgraded.shared().lock().unwrap().as_ref().unwrap().symbols),
        Some(13)
    );
    assert_eq!(
        CodeIndexSnapshot::load(dir.path())
            .unwrap()
            .unwrap()
            .meta
            .format_version,
        SNAPSHOT_FORMAT_VERSION
    );
}

#[test]
fn every_worker_count_installs_identical_facts() {
    let dir = tempdir().unwrap();
    mixed_workspace(dir.path());
    let (serial, outcome) = IndexState::build_from_root_with_workers(dir.path(), 1);
    assert_eq!(outcome.files_skipped, 1, "the unreadable file is skipped");
    // A declaration walked after its caller still resolves, and the file
    // the grammar rejects keeps a Module-only slice instead of failing.
    assert!(resolved_calls(&serial) >= 80);
    let broken = serial.lookup_path("src/broken.rs").unwrap();
    assert!(serial.symbols.module_node_for_file(broken).is_some());
    let expected = exact_facts(&serial);
    for workers in [2, 4, 6, 64] {
        let (parallel, parallel_outcome) =
            IndexState::build_from_root_with_workers(dir.path(), workers);
        assert_eq!(parallel_outcome.files_indexed, outcome.files_indexed);
        assert_eq!(parallel_outcome.files_skipped, outcome.files_skipped);
        assert_eq!(
            exact_facts(&parallel),
            expected,
            "{workers} workers changed the index"
        );
    }
}

#[test]
fn a_parallel_refresh_matches_a_serial_one() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    mixed_workspace(root);
    let (base, _) = IndexState::build_from_root_with_workers(root, 1);
    let saved = base.snapshot();
    for i in (0..40).step_by(3) {
        write(
            root,
            &format!("z_decl_{i:02}.py"),
            format!("def fetch_{i}():\n    return {i}\n\ndef added_{i}():\n    return 0\n"),
        );
    }
    // Same bytes, newer timestamp: content-checked without a parse.
    write(root, "src/util.rs", "pub fn helper() -> u32 { 2 }\n");
    let mut results = Vec::new();
    for workers in [1, 4] {
        let mut state = IndexState::from_snapshot(saved.clone()).unwrap();
        let outcome = state.refresh_with_checkpoints(
            None,
            super::super::reconcile::CheckpointPolicy {
                workers,
                ..Default::default()
            },
            |_| {},
        );
        assert_eq!(outcome.files_reindexed, 14);
        results.push(exact_facts(&state));
    }
    assert_eq!(results[0], results[1]);
}

fn jobs(count: usize) -> (tempfile::TempDir, Vec<FileJob>) {
    let dir = tempdir().unwrap();
    let jobs = (0..count)
        .map(|i| {
            let rel = format!("f{i:04}.py");
            write(dir.path(), &rel, format!("def f{i}():\n    return {i}\n"));
            FileJob {
                abs: PathBuf::from(dir.path()).join(&rel),
                rel,
                language: harn_languages::Language::Python,
                size_bytes: 0,
                mtime_ms: 0,
                known_hash: None,
            }
        })
        .collect();
    (dir, jobs)
}

#[test]
fn workers_never_run_further_ahead_than_the_window() {
    let (_dir, jobs) = jobs(300);
    let workers = 3;
    let claimed = AtomicUsize::new(0);
    let counting = |job: &FileJob| {
        claimed.fetch_add(1, Ordering::SeqCst);
        extract(job)
    };
    let mut published = Vec::new();
    publish_extracted_in_order(&jobs, workers, &counting, |job, facts| {
        assert!(matches!(facts, FileFacts::Changed(_)));
        published.push(job.rel.clone());
        // Taking job i lets claims reach i + 1 + window, never further.
        let bound = published.len() + workers * QUEUED_FACTS_PER_WORKER;
        assert!(claimed.load(Ordering::SeqCst) <= bound);
        ControlFlow::Continue(())
    });
    let expected: Vec<_> = jobs.iter().map(|job| job.rel.clone()).collect();
    assert_eq!(published, expected, "publication follows job order");
}

#[test]
fn a_stopped_publisher_stops_every_worker() {
    let (_dir, jobs) = jobs(300);
    let workers = 4;
    let claimed = AtomicUsize::new(0);
    let counting = |job: &FileJob| {
        claimed.fetch_add(1, Ordering::SeqCst);
        extract(job)
    };
    let mut published = 0;
    publish_extracted_in_order(&jobs, workers, &counting, |_, _| {
        published += 1;
        if published == 5 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    // Returning at all proves every worker exited: the scope joins them.
    assert_eq!(published, 5);
    assert!(claimed.load(Ordering::SeqCst) <= 5 + workers * QUEUED_FACTS_PER_WORKER);
}

#[test]
fn a_panicking_publisher_unwinds_through_the_pool() {
    let (_dir, jobs) = jobs(100);
    let result = catch_unwind(AssertUnwindSafe(|| {
        publish_extracted_in_order(&jobs, 4, &extract, |job, _| {
            assert_ne!(job.rel, "f0007.py", "simulated publisher failure");
            ControlFlow::Continue(())
        });
    }));
    assert!(result.is_err());
}

#[test]
fn a_panicking_worker_fails_the_build_instead_of_hanging_it() {
    let (_dir, jobs) = jobs(100);
    let failing = |job: &FileJob| {
        assert_ne!(job.rel, "f0042.py", "simulated extraction failure");
        extract(job)
    };
    let mut published = 0;
    let result = catch_unwind(AssertUnwindSafe(|| {
        publish_extracted_in_order(&jobs, 4, &failing, |_, _| {
            published += 1;
            ControlFlow::Continue(())
        });
    }));
    assert!(result.is_err());
    assert_eq!(published, 42, "nothing after the failed file is published");
}

#[test]
fn a_later_worker_panic_does_not_hide_an_earlier_lost_job() {
    let window = super::Window::new(8);
    // The earliest lost job survives a later unwind, so the publisher
    // fails at job 5 instead of waiting for it forever.
    for claimed in [5, 7] {
        let window = &window;
        let unwound = harn_parser::runtime_stack::scope(|scope| {
            scope
                .spawn(move || {
                    let _flag = super::PanicFlag {
                        window,
                        claimed: Some(claimed),
                    };
                    panic!("simulated extraction failure");
                })
                .join()
        });
        assert!(unwound.is_err());
    }
    assert_eq!(window.lock().lost, Some(5));
}

#[test]
fn reindexing_unchanged_bytes_keeps_posting_order_and_graph() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a.py", "def shared_name():\n    return 1\n");
    write(root, "b.py", "shared_name()\n");
    let (mut state, _) = IndexState::build_from_root(root);
    let before = exact_facts(&state);
    let hits = |state: &IndexState| -> Vec<u32> {
        state
            .words
            .get("shared_name")
            .iter()
            .map(|hit| hit.file)
            .collect()
    };
    let order = hits(&state);
    // Same bytes, newer timestamp: an editor save with no edits.
    write(root, "a.py", "def shared_name():\n    return 1\n");
    state.reindex_file(&root.join("a.py")).unwrap();
    assert_eq!(hits(&state), order);
    assert_eq!(exact_facts(&state), before);
}
