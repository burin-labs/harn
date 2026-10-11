use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::time::Duration;

use super::CheckpointPolicy;
use crate::code_index::{CodeIndexCapability, CodeIndexSnapshot, EdgeKind, IndexState, NodeKind};

fn interrupt_after_two(state: &mut IndexState) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        state.refresh_with_checkpoints(
            None,
            CheckpointPolicy {
                files: 2,
                interval: Duration::MAX,
            },
            |state| {
                state.snapshot().save(&state.root).unwrap();
                panic!("simulate process exit immediately after checkpoint");
            },
        );
    }));
    assert!(result.is_err(), "the checkpoint must actually fire");
}

fn write(root: &Path, path: &str, body: &str) {
    fs::write(root.join(path), body).unwrap();
}

#[test]
fn a_checkpoint_uses_one_source_revision_for_hash_words_and_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.py", "def original():\n    return 1\n");
    let (mut state, _) = IndexState::build_from_root(root);
    let captured = "def checkpointed():\n    return 123\n";
    write(root, "a.py", captured);
    let ingested = state.ingest(&root.join("a.py"), None).unwrap();
    assert!(ingested.changed);
    // This is the exact gap between ingest and graph parsing in the old
    // reconcile. A second disk read would install disk_changed's symbols
    // under checkpointed's content hash and word postings.
    write(root, "a.py", "def disk_changed():\n    return 123456789\n");
    state.rebuild_symbol_graph_for(ingested.id, &ingested.source);
    state.relink_dirty_calls();
    state.snapshot().save(root).unwrap();
    let snapshot = CodeIndexSnapshot::load(root).unwrap().unwrap();
    let mut restored = IndexState::from_snapshot(snapshot).unwrap();
    assert_eq!(
        restored.files[&ingested.id].content_hash,
        crate::code_index::file_table::fnv1a64(captured.as_bytes())
    );
    assert!(!restored.words.get("checkpointed").is_empty());
    assert_eq!(restored.symbols.nodes_named("checkpointed").len(), 1);
    assert!(restored.symbols.nodes_named("disk_changed").is_empty());
    assert_eq!(restored.refresh_from_root(None).files_reindexed, 1);
    assert_eq!(restored.symbols.nodes_named("disk_changed").len(), 1);
}

#[test]
fn elapsed_checkpoint_trigger_requires_completed_work() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.py", "def original():\n    return 1\n");
    let (mut state, _) = IndexState::build_from_root(dir.path());
    let mut checkpoints = 0;
    let policy = || CheckpointPolicy {
        files: usize::MAX,
        interval: Duration::ZERO,
    };
    let outcome = state.refresh_with_checkpoints(None, policy(), |_| checkpoints += 1);
    assert!(outcome.is_noop());
    assert_eq!(checkpoints, 0);
    write(dir.path(), "a.py", "def replaced():\n    return 12345\n");
    let outcome = state.refresh_with_checkpoints(None, policy(), |state| {
        checkpoints += 1;
        assert!(state.symbols.nodes_named("original").is_empty());
        assert_eq!(state.symbols.nodes_named("replaced").len(), 1);
    });
    assert_eq!(outcome.files_reindexed, 1);
    assert_eq!(checkpoints, 1);
}

fn calls(state: &IndexState) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    for site in state
        .symbols
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::CallSite)
    {
        for edge in state
            .symbols
            .outgoing(site.id)
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
        {
            calls.push((
                site.path.clone(),
                state.symbols.node(edge.to).unwrap().path.clone(),
            ));
        }
    }
    calls.sort();
    calls
}

#[test]
fn checkpoint_retains_complete_graph_and_unvisited_metadata_then_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.py", "def old():\n    return 1\n");
    write(root, "b.py", "from a import old\nold()\n");
    write(root, "c.py", "def later():\n    return 1\n");
    write(root, "deleted.py", "def removed():\n    return 1\n");
    let (mut state, _) = IndexState::build_from_root(root);
    let c_id = state.path_to_id["c.py"];
    let c_hash = state.files[&c_id].content_hash;
    let c_mtime = state.files[&c_id].mtime_ms;
    write(root, "a.py", "def fresh():\n    return 12345\n");
    write(root, "b.py", "from a import fresh\nfresh()\n");
    write(root, "c.py", "def later():\n    return 12345\n");
    fs::remove_file(root.join("deleted.py")).unwrap();
    write(root, "new.py", "from a import fresh\nfresh()\n");
    interrupt_after_two(&mut state);

    let checkpoint = CodeIndexSnapshot::load(root).unwrap().unwrap();
    let mut restored = IndexState::from_snapshot(checkpoint).unwrap();
    assert_eq!(restored.files[&c_id].content_hash, c_hash);
    assert_eq!(restored.files[&c_id].mtime_ms, c_mtime);
    assert!(restored.path_to_id.contains_key("deleted.py"));
    assert!(!restored.path_to_id.contains_key("new.py"));
    assert_eq!(calls(&restored), vec![("b.py".into(), "a.py".into())]);
    assert!(restored.symbols.nodes_named("old").is_empty());

    let outcome = restored.refresh_from_root(None);
    assert_eq!(outcome.files_unchanged, 2);
    assert_eq!(outcome.files_reindexed, 2);
    assert_eq!(outcome.files_added, 1);
    assert_eq!(outcome.files_removed, 1);
    let (fresh, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&restored), calls(&fresh));
    assert_eq!(restored.symbols.node_count(), fresh.symbols.node_count());
    assert_eq!(restored.symbols.edge_count(), fresh.symbols.edge_count());
    assert!(restored.refresh_from_root(None).is_noop());
}

#[test]
fn checkpoint_resumes_head_verification_and_invalidates_progress_on_another_head() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join(".git")).unwrap();
    write(root, ".git/HEAD", "first");
    for name in ["a.py", "b.py", "c.py", "d.py"] {
        write(root, name, "def original():\n    return 1\n");
    }
    let (mut state, _) = IndexState::build_from_root(root);
    write(root, ".git/HEAD", "second");
    for name in ["a.py", "b.py"] {
        write(root, name, "def original():\n    return 12345\n");
    }
    // The fourth file changes while retaining the exact size and timestamp.
    let stamp =
        filetime::FileTime::from_last_modification_time(&fs::metadata(root.join("d.py")).unwrap());
    write(root, "d.py", "def replaced():\n    return 2\n");
    filetime::set_file_mtime(root.join("d.py"), stamp).unwrap();
    interrupt_after_two(&mut state);
    let checkpoint = CodeIndexSnapshot::load(root).unwrap().unwrap();
    assert_eq!(checkpoint.meta.git_head.as_deref(), Some("first"));

    let mut restored = IndexState::from_snapshot(checkpoint.clone()).unwrap();
    let outcome = restored.refresh_from_root(None);
    assert!(outcome.git_head_changed);
    assert_eq!(outcome.files_unchanged, 2);
    assert_eq!(outcome.files_touched_only, 1);
    assert_eq!(outcome.files_reindexed, 1);
    assert!(!restored.symbols.nodes_named("replaced").is_empty());
    assert_eq!(restored.git_head.as_deref(), Some("second"));
    assert!(restored.reconcile_progress.is_none());

    write(root, ".git/HEAD", "third");
    let mut restored = IndexState::from_snapshot(checkpoint.clone()).unwrap();
    let outcome = restored.refresh_from_root(None);
    assert_eq!(outcome.files_unchanged, 0);
    assert_eq!(outcome.files_touched_only, 3);
    assert_eq!(outcome.files_reindexed, 1);

    // Reverting to the snapshot's old HEAD is still a HEAD change for the
    // files already verified at the interrupted reconcile's newer HEAD.
    write(root, ".git/HEAD", "first");
    let mut restored = IndexState::from_snapshot(checkpoint).unwrap();
    let outcome = restored.refresh_from_root(None);
    assert!(outcome.git_head_changed);
    assert_eq!(outcome.files_unchanged, 0);
    assert_eq!(outcome.files_touched_only, 3);
    assert_eq!(outcome.files_reindexed, 1);
}

#[test]
fn fast_head_verification_does_not_checkpoint_every_unchanged_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join(".git")).unwrap();
    write(root, ".git/HEAD", "first");
    write(root, "a.py", "def original():\n    return 1\n");
    let (mut state, _) = IndexState::build_from_root(root);
    write(root, ".git/HEAD", "second");
    let mut checkpoints = 0;
    let outcome = state.refresh_with_checkpoints(
        None,
        CheckpointPolicy {
            files: 1,
            interval: Duration::MAX,
        },
        |_| checkpoints += 1,
    );
    assert!(outcome.git_head_changed);
    assert_eq!(outcome.files_touched_only, 1);
    assert_eq!(checkpoints, 0);
    assert_eq!(state.git_head.as_deref(), Some("second"));
    assert!(state.reconcile_progress.is_none());
}

#[test]
fn disk_restore_checkpoints_before_the_full_reconcile_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Reach the production file threshold. Disable elapsed time so a loaded
    // test host cannot change where this deterministic checkpoint fires.
    for i in 0..514 {
        write(
            root,
            &format!("file{i:04}.py"),
            "def original():\n    return 1\n",
        );
    }
    let (mut state, _) = IndexState::build_from_root(root);
    state.snapshot().save(root).unwrap();
    for i in 0..514 {
        write(
            root,
            &format!("file{i:04}.py"),
            "def replaced():\n    return 12345\n",
        );
    }
    let mut checkpoints = 0;
    let policy = CheckpointPolicy {
        interval: Duration::MAX,
        ..CheckpointPolicy::default()
    };
    state.refresh_with_checkpoints(None, policy, |state| {
        checkpoints += 1;
        assert_eq!(state.symbols.nodes_named("replaced").len(), 512);
        assert_eq!(state.symbols.nodes_named("original").len(), 2);
        state.snapshot().save(root).unwrap();
    });
    assert_eq!(checkpoints, 1);
    let capability = CodeIndexCapability::new();
    assert!(capability.restore_from_disk(root).unwrap());
    let snapshot = CodeIndexSnapshot::load(root).unwrap().unwrap();
    let mut state = IndexState::from_snapshot(snapshot).unwrap();
    assert!(state.symbols.nodes_named("original").is_empty());
    assert_eq!(state.symbols.nodes_named("replaced").len(), 514);
    assert!(state.refresh_from_root(None).is_noop());
}
