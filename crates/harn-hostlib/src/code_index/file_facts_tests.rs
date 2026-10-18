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
