use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum CaptureStage {
    BeforePublish,
    AfterPublish,
}

type CaptureHook = Box<dyn Fn(CaptureStage)>;

thread_local! {
    static CAPTURE_HOOK: std::cell::RefCell<Option<CaptureHook>> = const { std::cell::RefCell::new(None) };
}

type RetirementHook = Box<dyn Fn()>;

thread_local! {
    static RETIREMENT_HOOK: std::cell::RefCell<Option<RetirementHook>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn retirement_hook() {
    RETIREMENT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

thread_local! {
    static GATE_WAIT_HOOK: std::cell::RefCell<Option<RetirementHook>> = const { std::cell::RefCell::new(None) };
}

/// Runs on an operation's thread when it finds a retirement holding the gate.
pub(super) fn gate_wait_hook() {
    GATE_WAIT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

#[derive(Debug, PartialEq)]
enum LoaderEvent {
    WaitingForRetirement,
    Finished,
}

#[test]
fn a_load_during_retirement_waits_and_finds_no_checkpoint() {
    use std::sync::mpsc::channel;

    let (dir, id) = deleted_in_subdir("retiring");
    let root = dir.path().to_path_buf();
    let (held_tx, held_rx) = channel();
    let (release_tx, release_rx) = channel::<()>();
    let (events_tx, events_rx) = channel();
    harn_parser::runtime_stack::scope(|scope| {
        let retiring_id = &id;
        let retiring = scope.spawn(move || {
            RETIREMENT_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }));
            });
            let receipt = drop_session(retiring_id);
            RETIREMENT_HOOK.with(|slot| *slot.borrow_mut() = None);
            receipt
        });
        held_rx.recv().unwrap();
        assert!(
            session_dir(&root, &id).exists(),
            "the record must still be on disk while retirement is held"
        );
        let (loader_id, loader_root) = (&id, &root);
        let loader = scope.spawn(move || {
            let waiting = events_tx.clone();
            GATE_WAIT_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    let _ = waiting.send(LoaderEvent::WaitingForRetirement);
                }));
            });
            let outcomes = [
                serde_json::to_value(persist::resume(loader_id, loader_root).unwrap()).unwrap(),
                serde_json::to_value(call_begin(loader_id, "late").unwrap()).unwrap(),
                serde_json::to_value(plan(loader_id, "turn-1").unwrap()).unwrap(),
            ];
            GATE_WAIT_HOOK.with(|slot| *slot.borrow_mut() = None);
            events_tx.send(LoaderEvent::Finished).unwrap();
            outcomes
        });
        // The first thing the loader reports is either that it found the
        // retirement holding the gate, or that it already finished.
        let first = events_rx.recv().unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            first,
            LoaderEvent::WaitingForRetirement,
            "a load, call admission or plan ran while the session was being retired"
        );
        assert!(retiring.join().unwrap().dropped);
        for outcome in loader.join().unwrap() {
            assert_eq!(outcome["reason"], "no_checkpoint", "{outcome}");
        }
    });
    assert!(find_session(&id).is_none());
    assert!(!session_dir(&root, &id).exists());
}

#[test]
fn a_drop_from_inside_a_checkpoint_operation_is_refused() {
    let (dir, id) = deleted_in_subdir("drop-inside");
    let hook_id = id.clone();
    CALL_BEGIN_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let receipt = drop_session(&hook_id);
            assert!(!receipt.dropped);
            assert!(receipt.error.is_some());
        }));
    });
    call_begin(&id, "tool-2").unwrap();
    CALL_BEGIN_HOOK.with(|slot| *slot.borrow_mut() = None);
    call_end(&id, "tool-2", None).unwrap();
    assert!(find_session(&id).is_some());
    assert!(drop_session(&id).dropped);
    assert!(!session_dir(dir.path(), &id).exists());
}

pub(super) fn capture_hook(stage: CaptureStage) {
    CAPTURE_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook(stage);
        }
    });
}

#[test]
fn racing_first_capture_cannot_acknowledge_an_unstored_writer_policy() {
    use std::sync::mpsc::channel;

    let dir = tempfile::TempDir::new().unwrap();
    git(dir.path(), &["init", "-q"]);
    let id = format!("capture-policy-race-{}", std::process::id());
    let (scanned_tx, scanned_rx) = channel();
    let (published_tx, published_rx) = channel();
    let (finished_tx, finished_rx) = channel();
    harn_parser::runtime_stack::scope(|scope| {
        let root = dir.path();
        let strict_id = &id;
        let strict = scope.spawn(move || {
            CAPTURE_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |stage| {
                    if stage == CaptureStage::BeforePublish {
                        scanned_tx.send(()).unwrap();
                        published_rx.recv().unwrap();
                    }
                }));
            });
            let result = capture_with_writer_policy(
                strict_id,
                "strict",
                Some(root),
                Some(WriterPolicy::RequireKnown),
            );
            CAPTURE_HOOK.with(|slot| *slot.borrow_mut() = None);
            finished_tx.send(()).unwrap();
            result
        });
        scanned_rx.recv().unwrap();
        CAPTURE_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |stage| {
                if stage == CaptureStage::AfterPublish {
                    published_tx.send(()).unwrap();
                    finished_rx.recv().unwrap();
                }
            }));
        });
        let legacy = capture(&id, "legacy", Some(dir.path()));
        CAPTURE_HOOK.with(|slot| *slot.borrow_mut() = None);
        let strict = strict.join().unwrap().unwrap();
        assert!(
            matches!(strict, Outcome::Unavailable(ref why) if why.reason == "concurrent_calls")
        );
        assert!(
            matches!(legacy.unwrap(), Outcome::Ready(receipt) if receipt.writer_policy == WriterPolicy::LegacyScan)
        );
    });
    let state = find_session(&id).unwrap();
    assert_eq!(lock(&state).writer_policy, WriterPolicy::LegacyScan);
    assert_eq!(lock(&state).boundaries.len(), 1);
    drop_session(&id);
}

#[test]
fn fork_refuses_when_a_call_starts_and_finishes_during_copy() {
    let (dir, id) = deleted_in_subdir("fork-race-call");
    let child = format!("{id}-child");
    let outcome = fork::fork_session_checked(&id, &child, dir.path(), || {
        call_begin(&id, "concurrent").unwrap();
        stdfs::write(dir.path().join("sub/f.txt"), "later edit\n").unwrap();
        call_end(&id, "concurrent", None).unwrap();
    })
    .unwrap();
    let value = serde_json::to_value(outcome).unwrap();
    assert_eq!(value["reason"], "source_changed", "{value}");
    assert!(find_session(&child).is_none());
    assert!(!session_dir(dir.path(), &child).exists());
    drop_session(&id);
}

#[test]
fn fork_refuses_when_a_restore_changes_the_same_boundary_during_copy() {
    let (dir, id) = deleted_in_subdir("fork-race-restore");
    let child = format!("{id}-child");
    let outcome = fork::fork_session_checked(&id, &child, dir.path(), || {
        assert!(matches!(
            restore(&id, "turn-1", None).unwrap(),
            Outcome::Ready(_)
        ));
        assert_eq!(
            stdfs::read_to_string(dir.path().join("sub/f.txt")).unwrap(),
            "f original\n"
        );
    })
    .unwrap();
    let value = serde_json::to_value(outcome).unwrap();
    assert_eq!(value["reason"], "source_changed", "{value}");
    assert!(find_session(&child).is_none());
    assert!(!session_dir(dir.path(), &child).exists());
    drop_session(&id);
}
use std::os::unix::fs::symlink;

#[test]
fn checkpoint_capture_accepts_an_existing_root_alias_but_rejects_another_root() {
    let (dir, id) = deleted_in_subdir("root-alias");
    let alias = dir.path().with_file_name(format!(
        "{}-alias",
        dir.path()
            .file_name()
            .expect("tempdir name")
            .to_string_lossy()
    ));
    symlink(dir.path(), &alias).unwrap();

    let outcome = capture(&id, "turn-2", Some(&alias)).unwrap();
    assert!(matches!(outcome, Outcome::Ready(_)), "{outcome:?}");

    let other = tempfile::TempDir::new().unwrap();
    let outcome = capture(&id, "turn-3", Some(other.path()));
    assert!(matches!(
        outcome,
        Err(HostlibError::InvalidParameter { param: "root", .. })
    ));
    stdfs::remove_file(alias).unwrap();
    drop_session(&id);
}

#[test]
fn a_new_runtime_path_is_excluded_by_the_scan_without_a_second_list() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    stdfs::create_dir_all(root.join(".harn/future-runtime")).unwrap();
    stdfs::create_dir_all(root.join(".harn/skills")).unwrap();
    stdfs::write(root.join(".harn/future-runtime/state"), "runtime").unwrap();
    stdfs::write(root.join(".harn/skills/user.md"), "user").unwrap();
    let mut declared = crate::workspace_runtime_paths::owned_paths(root);
    declared.push(root.join(".harn/future-runtime"));
    let scan = scan_tree_with_exclusions(root, false, declared);
    assert_eq!(scan.errors, 0);
    assert!(!scan.index.contains_key(".harn/future-runtime/state"));
    assert!(scan.index.contains_key(".harn/skills/user.md"));
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A worktree with `sub/f.txt` deleted by a tool call after boundary
/// `turn-1`, so restoring recreates it. Returns (root dir, session id).
fn deleted_in_subdir(tag: &str) -> (tempfile::TempDir, String) {
    deleted_leaf_in_subdir(tag, "f.txt")
}

fn deleted_leaf_in_subdir(tag: &str, leaf: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "t"]);
    stdfs::create_dir(root.join("sub")).unwrap();
    stdfs::write(root.join("sub").join(leaf), "f original\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    let id = format!("anchor-{tag}-{}", std::process::id());
    assert!(matches!(
        capture(&id, "turn-1", Some(root)).unwrap(),
        Outcome::Ready(_)
    ));
    call_begin(&id, "tool-1").unwrap();
    stdfs::remove_file(root.join("sub").join(leaf)).unwrap();
    call_end(&id, "tool-1", None).unwrap();
    (dir, id)
}

fn swap_sub_for_symlink(root: &Path, outside: &Path) {
    stdfs::rename(root.join("sub"), root.join("sub.moved")).unwrap();
    symlink(outside, root.join("sub")).unwrap();
}

fn with_hook(stage: Stage, hook: impl Fn() + 'static) {
    RESTORE_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |at| {
            if at == stage {
                hook();
            }
        }));
    });
}

fn clear_hook() {
    RESTORE_HOOK.with(|slot| *slot.borrow_mut() = None);
}

#[test]
fn checkpoint_operations_refuse_before_call_begin_returns() {
    let (dir, id) = deleted_in_subdir("beginning");
    let hook_id = id.clone();
    let hook_root = dir.path().to_path_buf();
    CALL_BEGIN_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            for outcome in [
                serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap(),
                serde_json::to_value(restore(&hook_id, "turn-1", None).unwrap()).unwrap(),
                serde_json::to_value(capture(&hook_id, "turn-2", Some(&hook_root)).unwrap())
                    .unwrap(),
                serde_json::to_value(
                    fork_session(&hook_id, "beginning-child", &hook_root).unwrap(),
                )
                .unwrap(),
            ] {
                assert_eq!(outcome["available"], false, "{outcome}");
                assert_eq!(outcome["reason"], "calls_in_flight");
                assert!(outcome["detail"].as_str().unwrap().contains("tool-2"));
            }
            assert!(!hook_root.join("sub/f.txt").exists());
            assert!(call_begin(&hook_id, "tool-2").is_err());
            assert!(call_end(&hook_id, "tool-2", None).is_err());
            let outcome = serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap();
            assert_eq!(outcome["reason"], "calls_in_flight", "{outcome}");
        }));
    });
    call_begin(&id, "tool-2").unwrap();
    CALL_BEGIN_HOOK.with(|slot| *slot.borrow_mut() = None);
    call_end(&id, "tool-2", None).unwrap();
    assert!(matches!(plan(&id, "turn-1").unwrap(), Outcome::Ready(_)));
    call_begin(&id, "tool-2").unwrap();
    call_end(&id, "tool-2", None).unwrap();
    drop_session(&id);
}

#[test]
fn restore_and_fork_refuse_while_the_call_end_scan_is_running() {
    let (dir, id) = deleted_in_subdir("ending");
    call_begin(&id, "tool-2").unwrap();
    stdfs::write(dir.path().join("sub/f.txt"), "new contents\n").unwrap();
    let hook_id = id.clone();
    let hook_root = dir.path().to_path_buf();
    CALL_END_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            for outcome in [
                serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap(),
                serde_json::to_value(restore(&hook_id, "turn-1", None).unwrap()).unwrap(),
                serde_json::to_value(fork_session(&hook_id, "ending-child", &hook_root).unwrap())
                    .unwrap(),
            ] {
                assert_eq!(outcome["available"], false, "{outcome}");
                assert_eq!(outcome["reason"], "calls_in_flight");
                assert!(outcome["detail"].as_str().unwrap().contains("tool-2"));
            }
            assert_eq!(
                stdfs::read_to_string(hook_root.join("sub/f.txt")).unwrap(),
                "new contents\n"
            );
        }));
    });
    call_end(&id, "tool-2", None).unwrap();
    CALL_END_HOOK.with(|slot| *slot.borrow_mut() = None);
    assert!(matches!(plan(&id, "turn-1").unwrap(), Outcome::Ready(_)));
    drop_session(&id);
}

#[test]
fn strict_restore_preserves_a_file_created_after_the_final_guard() {
    let (dir, id) = deleted_in_subdir("final-guard");
    lock(&find_session(&id).unwrap()).writer_policy = WriterPolicy::RequireKnown;
    let root = dir.path().to_path_buf();
    with_hook(Stage::AfterFinalGuard, move || {
        stdfs::write(root.join("sub/f.txt"), "person's concurrent edit\n").unwrap();
    });
    let confirmation = ConfirmedPath {
        session_id: id.clone(),
        boundary_id: "turn-1".to_string(),
        path: "sub/f.txt".to_string(),
        observed: ObservedFileState::Absent,
    };
    let result = restore_with_confirmations(&id, "turn-1", None, &[confirmation]).unwrap();
    clear_hook();
    let Outcome::Ready(receipt) = result else {
        panic!("restore unavailable");
    };
    assert!(receipt.restored.is_empty(), "{receipt:?}");
    assert_eq!(
        stdfs::read_to_string(dir.path().join("sub/f.txt")).unwrap(),
        "person's concurrent edit\n"
    );
    drop_session(&id);
}

#[test]
fn ancestor_swapped_after_planning_fails_the_path() {
    let (dir, id) = deleted_in_subdir("before");
    let outside = tempfile::TempDir::new().unwrap();
    let (root, out) = (dir.path().to_path_buf(), outside.path().to_path_buf());
    with_hook(Stage::BeforeWalk, move || swap_sub_for_symlink(&root, &out));
    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore unavailable");
    };
    clear_hook();
    drop_session(&id);
    assert!(receipt.restored.is_empty(), "{receipt:?}");
    let failed: Vec<_> = receipt.failed.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(failed, ["sub/f.txt"], "{receipt:?}");
    assert_eq!(stdfs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn ancestor_swapped_after_the_walk_cannot_escape_the_root() {
    let (dir, id) = deleted_in_subdir("after");
    let outside = tempfile::TempDir::new().unwrap();
    let (root, out) = (dir.path().to_path_buf(), outside.path().to_path_buf());
    with_hook(Stage::AfterWalk, move || swap_sub_for_symlink(&root, &out));
    let result = restore(&id, "turn-1", None);
    clear_hook();
    drop_session(&id);
    result.unwrap();
    assert_eq!(
        stdfs::read_dir(outside.path()).unwrap().count(),
        0,
        "a swapped ancestor redirected the write outside the root"
    );
}

#[test]
fn restore_recreates_a_file_with_a_255_byte_name() {
    let leaf = "a".repeat(255);
    let (dir, id) = deleted_leaf_in_subdir("longname", &leaf);
    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore unavailable");
    };
    drop_session(&id);
    assert!(receipt.failed.is_empty(), "{receipt:?}");
    let restored = stdfs::read_to_string(dir.path().join("sub").join(&leaf)).unwrap();
    assert_eq!(restored, "f original\n");
}

#[test]
fn a_restarted_host_resumes_the_history_an_earlier_process_recorded() {
    let (dir, id) = deleted_in_subdir("resume");
    let root = dir.path();
    call_begin(&id, "net").unwrap();
    call_end(&id, "net", Some(SideEffectLevel::Network)).unwrap();
    let before = serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap();
    // A new process starts with an empty registry and the files on disk.
    assert!(unload(&id));
    assert!(matches!(
        plan(&id, "turn-1").unwrap(),
        Outcome::Unavailable(ref why) if why.reason == "no_checkpoint"
    ));

    let resumed = resume(&id, root).unwrap();
    assert!(
        matches!(resumed, Outcome::Ready(ref r) if r.loaded && r.boundaries == 1 && r.calls == 2),
        "{resumed:?}"
    );
    assert_eq!(
        serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap(),
        before
    );
    // The resumed revision keeps later invocations distinct from recorded ones.
    call_begin(&id, "net").unwrap();
    call_end(&id, "net", Some(SideEffectLevel::Network)).unwrap();
    let after = serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap();
    let invocations: BTreeSet<u64> = after["irreversible"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|effect| effect["invocation"].as_u64())
        .collect();
    assert_eq!(invocations.len(), 2, "{after}");

    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore after resume was unavailable");
    };
    assert!(receipt.failed.is_empty(), "{receipt:?}");
    assert_eq!(
        stdfs::read_to_string(root.join("sub").join("f.txt")).unwrap(),
        "f original\n"
    );
    drop_session(&id);
    assert!(matches!(
        resume(&id, root).unwrap(),
        Outcome::Unavailable(ref why) if why.reason == "no_checkpoint"
    ));
}

#[test]
fn begin_refuses_before_effects_when_namespace_durability_is_not_confirmed() {
    let (dir, id) = deleted_in_subdir("begin-namespace-failure");
    let root = dir.path();
    let index = session_dir(root, &id).join("index.json");
    let original_index = stdfs::read(&index).unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::MissingNamespaceSync);

    let begin = call_begin(&id, "refused-call").unwrap();
    assert!(matches!(begin, Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_persist_failed"));
    let replaced_index = stdfs::read(&index).unwrap();
    assert_ne!(replaced_index, original_index);
    let replaced: serde_json::Value = serde_json::from_slice(&replaced_index).unwrap();
    assert_eq!(replaced["phase"]["state"], "incomplete");

    // Namespace durability was not confirmed after replacement. The begin
    // admitted no effect, but a cold host must treat the visible incomplete
    // index as interrupted rather than resume the previous complete state.
    assert!(unload(&id));
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    drop_session(&id);
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "no_checkpoint")
    );
}

#[test]
fn begin_prewrite_failure_keeps_the_previous_complete_index_available() {
    let (dir, id) = deleted_in_subdir("begin-prewrite-failure");
    let root = dir.path();
    let index = session_dir(root, &id).join("index.json");
    let original_index = stdfs::read(&index).unwrap();
    let before_plan = serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::Error);

    let begin = call_begin(&id, "refused-call").unwrap();
    assert!(matches!(begin, Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_persist_failed"));
    assert_eq!(stdfs::read(&index).unwrap(), original_index);

    assert!(unload(&id));
    assert!(matches!(resume(&id, root).unwrap(), Outcome::Ready(_)));
    assert_eq!(
        serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap(),
        before_plan
    );
    drop_session(&id);
}

#[test]
fn failed_capture_is_advisory_but_next_tool_begins_with_a_durable_incomplete_index() {
    let (dir, id) = deleted_in_subdir("capture-advisory");
    let root = dir.path();
    let index_path = session_dir(root, &id).join("index.json");
    let before_capture = stdfs::read(&index_path).unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::Error);

    assert!(matches!(
        capture(&id, "turn-2", Some(root)).unwrap(),
        Outcome::Unavailable(ref why) if why.reason == "capture_failed"
    ));
    assert_eq!(stdfs::read(&index_path).unwrap(), before_capture);

    assert!(matches!(
        call_begin(&id, "later").unwrap(),
        Outcome::Ready(_)
    ));
    let before_effect: serde_json::Value =
        serde_json::from_slice(&stdfs::read(&index_path).unwrap()).unwrap();
    assert_eq!(before_effect["phase"]["state"], "incomplete");
    assert_eq!(before_effect["boundaries"][1]["id"], "turn-2");
    stdfs::write(root.join("sub/later.txt"), "later\n").unwrap();
    assert!(matches!(
        call_end(&id, "later", None).unwrap(),
        Outcome::Ready(_)
    ));

    assert!(unload(&id));
    assert!(matches!(resume(&id, root).unwrap(), Outcome::Ready(_)));
    let Outcome::Ready(plan) = plan(&id, "turn-2").unwrap() else {
        panic!("the boundary published before the tool effect did not resume");
    };
    assert!(plan
        .entries
        .iter()
        .any(|entry| entry.path == "sub/later.txt"));
    drop_session(&id);
}

#[test]
fn cold_resume_refuses_an_interrupted_call_and_keeps_drop_available() {
    let (dir, id) = deleted_in_subdir("interrupted-call");
    let root = dir.path();
    assert!(matches!(
        call_begin(&id, "left-open").unwrap(),
        Outcome::Ready(_)
    ));
    let index_path = session_dir(root, &id).join("index.json");
    let index: serde_json::Value =
        serde_json::from_slice(&stdfs::read(index_path).unwrap()).unwrap();
    assert_eq!(index["phase"]["state"], "incomplete");
    assert_eq!(index["phase"]["pending_call_ids"][0], "left-open");
    assert!(!unload(&id), "ordinary unload must preserve an active call");

    // Model process loss even though normal unload correctly refuses a live
    // call. Resume registers only the poisoned drop handle.
    sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned")
        .remove(&id);
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    assert!(
        matches!(plan(&id, "turn-1").unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    assert!(unload(&id));
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    assert!(drop_session(&id).dropped);
    assert!(!session_dir(root, &id).exists());
}

#[test]
fn overlapping_calls_keep_the_index_incomplete_until_both_attributions_land() {
    let (dir, id) = deleted_in_subdir("overlapping-call-index");
    let root = dir.path();
    assert!(matches!(
        call_begin(&id, "call-a").unwrap(),
        Outcome::Ready(_)
    ));
    assert!(matches!(
        call_begin(&id, "call-b").unwrap(),
        Outcome::Ready(_)
    ));
    assert!(matches!(
        call_end(&id, "call-a", Some(SideEffectLevel::Network)).unwrap(),
        Outcome::Ready(_)
    ));
    let index_path = session_dir(root, &id).join("index.json");
    let after_a: serde_json::Value =
        serde_json::from_slice(&stdfs::read(&index_path).unwrap()).unwrap();
    assert_eq!(after_a["phase"]["state"], "incomplete");
    assert_eq!(after_a["phase"]["pending_call_ids"][0], "call-b");
    assert!(matches!(
        call_end(&id, "call-b", Some(SideEffectLevel::Network)).unwrap(),
        Outcome::Ready(_)
    ));
    let complete: serde_json::Value =
        serde_json::from_slice(&stdfs::read(&index_path).unwrap()).unwrap();
    assert_eq!(complete["phase"]["state"], "complete");

    assert!(unload(&id));
    let Outcome::Ready(resumed) = resume(&id, root).unwrap() else {
        panic!("completed overlapping calls did not resume");
    };
    assert_eq!(resumed.calls, 3);
    let Outcome::Ready(plan) = plan(&id, "turn-1").unwrap() else {
        panic!("completed calls have no plan");
    };
    let invocations: BTreeSet<_> = plan
        .irreversible
        .iter()
        .map(|call| call.invocation)
        .collect();
    assert_eq!(invocations.len(), 2);
    assert!(plan
        .irreversible
        .iter()
        .any(|call| call.call_id == "call-a"));
    assert!(plan
        .irreversible
        .iter()
        .any(|call| call.call_id == "call-b"));
    drop_session(&id);
}

#[test]
fn a_failed_call_end_returns_effects_and_a_later_pending_call_can_complete_the_index() {
    let (dir, id) = deleted_in_subdir("call-end-failure-recovery");
    let root = dir.path();
    assert!(matches!(
        call_begin(&id, "call-a").unwrap(),
        Outcome::Ready(_)
    ));
    assert!(matches!(
        call_begin(&id, "call-b").unwrap(),
        Outcome::Ready(_)
    ));
    stdfs::write(root.join("sub/call-a.txt"), "a\n").unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::Error);

    let Outcome::Ready(call_a) = call_end(&id, "call-a", Some(SideEffectLevel::Network)).unwrap()
    else {
        panic!("the completed call outcome was lost");
    };
    assert!(call_a.persistence_error.is_some(), "{call_a:?}");
    assert!(call_a
        .changes
        .iter()
        .any(|change| change.path == "sub/call-a.txt"));
    let index_path = session_dir(root, &id).join("index.json");
    let interrupted: serde_json::Value =
        serde_json::from_slice(&stdfs::read(&index_path).unwrap()).unwrap();
    assert_eq!(interrupted["phase"]["state"], "incomplete");

    // Call B was already admitted. Its final save can safely complete the
    // full in-memory attribution for both calls and recover the session.
    stdfs::write(root.join("sub/call-b.txt"), "b\n").unwrap();
    let Outcome::Ready(call_b) = call_end(&id, "call-b", Some(SideEffectLevel::Network)).unwrap()
    else {
        panic!("the remaining call did not complete its attribution");
    };
    assert!(call_b.persistence_error.is_none(), "{call_b:?}");
    assert!(unload(&id));
    assert!(matches!(resume(&id, root).unwrap(), Outcome::Ready(_)));
    let Outcome::Ready(plan) = plan(&id, "turn-1").unwrap() else {
        panic!("a complete retry did not resume");
    };
    assert!(plan
        .irreversible
        .iter()
        .any(|call| call.call_id == "call-a"));
    assert!(plan
        .irreversible
        .iter()
        .any(|call| call.call_id == "call-b"));
    assert!(plan
        .entries
        .iter()
        .any(|entry| entry.path == "sub/call-a.txt"));
    assert!(plan
        .entries
        .iter()
        .any(|entry| entry.path == "sub/call-b.txt"));
    drop_session(&id);
}

#[test]
fn a_failed_final_call_write_keeps_the_completed_effect_receipt_and_refuses_cold_resume() {
    let (dir, id) = deleted_in_subdir("call-end-final-failure");
    let root = dir.path();
    assert!(matches!(
        call_begin(&id, "completed").unwrap(),
        Outcome::Ready(_)
    ));
    stdfs::write(root.join("sub/completed.txt"), "done\n").unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::Error);

    let Outcome::Ready(receipt) =
        call_end(&id, "completed", Some(SideEffectLevel::Network)).unwrap()
    else {
        panic!("completed tool effects were hidden by an index error");
    };
    assert!(receipt.persistence_error.is_some(), "{receipt:?}");
    assert!(receipt
        .changes
        .iter()
        .any(|change| change.path == "sub/completed.txt"));
    assert_eq!(
        stdfs::read_to_string(root.join("sub/completed.txt")).unwrap(),
        "done\n"
    );
    assert!(unload(&id));
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    assert!(drop_session(&id).dropped);
}

#[test]
fn restore_persists_the_final_trimmed_timeline_before_restart() {
    let (dir, id) = deleted_in_subdir("restore-final-timeline");
    let root = dir.path();
    assert!(matches!(
        capture(&id, "turn-2", Some(root)).unwrap(),
        Outcome::Ready(_)
    ));
    assert!(matches!(
        call_begin(&id, "later").unwrap(),
        Outcome::Ready(_)
    ));
    stdfs::write(root.join("sub/later.txt"), "later\n").unwrap();
    assert!(matches!(
        call_end(&id, "later", None).unwrap(),
        Outcome::Ready(_)
    ));

    let Outcome::Ready(restored) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore unavailable");
    };
    assert!(restored.persistence_error.is_none(), "{restored:?}");
    assert_eq!(
        stdfs::read_to_string(root.join("sub/f.txt")).unwrap(),
        "f original\n"
    );
    assert!(!root.join("sub/later.txt").exists());
    let in_memory = serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap();

    assert!(unload(&id));
    let Outcome::Ready(resumed) = resume(&id, root).unwrap() else {
        panic!("trimmed restore timeline did not resume");
    };
    assert_eq!(resumed.boundary_ids, vec!["turn-1"]);
    assert_eq!(resumed.calls, 0);
    assert_eq!(
        serde_json::to_value(plan(&id, "turn-1").unwrap()).unwrap(),
        in_memory
    );
    drop_session(&id);
}

#[test]
fn restore_prewrite_failure_changes_no_files_and_final_failure_keeps_receipt() {
    let (dir, id) = deleted_in_subdir("restore-persist-failures");
    let root = dir.path();
    let index = session_dir(root, &id).join("index.json");
    let original_index = stdfs::read(&index).unwrap();
    persist::fail_write_after(&id, root, 0, persist::TestWriteResult::Error);
    assert!(matches!(
        restore(&id, "turn-1", None).unwrap(),
        Outcome::Unavailable(ref why) if why.reason == "checkpoint_persist_failed"
    ));
    assert!(!root.join("sub/f.txt").exists());
    assert_eq!(stdfs::read(&index).unwrap(), original_index);

    // The first write is the pre-effect incomplete record; fail only the
    // final complete-state write after the restore has changed the file.
    persist::fail_write_after(&id, root, 1, persist::TestWriteResult::Error);
    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore effect was hidden by a final index failure");
    };
    assert!(!receipt.restored.is_empty(), "{receipt:?}");
    assert!(receipt.persistence_error.is_some(), "{receipt:?}");
    assert_eq!(
        stdfs::read_to_string(root.join("sub/f.txt")).unwrap(),
        "f original\n"
    );
    let incomplete: serde_json::Value =
        serde_json::from_slice(&stdfs::read(&index).unwrap()).unwrap();
    assert_eq!(incomplete["phase"]["state"], "incomplete");

    assert!(unload(&id));
    assert!(
        matches!(resume(&id, root).unwrap(), Outcome::Unavailable(ref why)
        if why.reason == "checkpoint_interrupted")
    );
    assert!(drop_session(&id).dropped);
}
