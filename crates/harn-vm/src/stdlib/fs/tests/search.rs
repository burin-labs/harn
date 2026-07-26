//! Search-builtin coverage for `find_text` and `find_evidence`.
//!
//! Split out of the parent module to keep each file under the source-length
//! ratchet; these mirror the sibling `fs/find_text.rs` and `fs/find_evidence.rs`
//! implementations and share the parent module's VM helpers.

use super::*;

#[test]
fn find_text_returns_structured_hits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.harn"),
        "alpha\nneedle here\nanother needle\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "needle docs\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![s(&dir.path().to_string_lossy()), s("needle")],
    )
    .unwrap();
    let VmValue::List(hits) = response else {
        panic!("find_text returns list");
    };
    assert_eq!(hits.len(), 3);
    let first = hits[0].as_dict().expect("hit dict");
    assert!(first["path"].display().ends_with("README.md"));
    assert_eq!(first["line"].as_int(), Some(1));
    assert_eq!(first["col"].as_int(), Some(1));
    assert_eq!(first["column"].as_int(), Some(1));
    assert_eq!(first["text"].display(), "needle docs");
}

#[test]
fn find_text_filters_case_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.harn"), "Needle\nneedle\n").unwrap();
    std::fs::write(dir.path().join("src/b.txt"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("include", s("**/*.harn")),
                ("case_insensitive", b(true)),
                ("max_matches", VmValue::Int(1)),
            ]),
        ],
    )
    .unwrap();
    let VmValue::List(hits) = response else {
        panic!("find_text returns list");
    };
    assert_eq!(hits.len(), 1);
    let hit = hits[0].as_dict().expect("hit dict");
    assert!(hit["path"].display().ends_with("src/a.harn"));
    assert_eq!(hit["text"].display(), "Needle");
}

#[test]
fn find_text_summary_modes_and_source_preset() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("src/a.harn"), "needle\nneedle\n").unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/a.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let exists = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("mode", s("exists")),
                ("preset", s("source")),
                ("parallel", b(true)),
            ]),
        ],
    )
    .unwrap();
    assert!(matches!(exists, VmValue::Bool(true)));

    let count = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("mode", s("count")),
                ("preset", s("source")),
                ("parallel", b(true)),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(count.as_int(), Some(2));
}

#[test]
fn find_text_background_returns_handle_and_feedback() {
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![("background", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "find_text");
    let handle_id = response["handle_id"].display();
    let payload = drain_feedback(&session_id, &handle_id);

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["operation"], "find_text");
    let result = payload["result"].as_array().unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("src/lib.harn"));
    assert_eq!(result[0]["line"], 1);
    assert_eq!(result[0]["col"], 1);
}

#[test]
fn find_evidence_is_labeled_overlapping_and_worker_deterministic() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("a.txt"), "needle\n").unwrap();
    std::fs::write(second.path().join("b.txt"), "a needle twice needle\n").unwrap();
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("z-root", second.path()),
        evidence_root("a-root", first.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![
        evidence_pattern("whole", "needle"),
        evidence_pattern("prefix", "need"),
    ]));
    let mut vm = vm();

    let sequential = call(
        &mut vm,
        "find_evidence",
        vec![
            roots.clone(),
            patterns.clone(),
            dict(vec![("threads", VmValue::Int(1))]),
        ],
    )
    .unwrap();
    let parallel = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("threads", VmValue::Int(4))])],
    )
    .unwrap();

    assert_eq!(
        crate::llm::vm_value_to_json(&sequential),
        crate::llm::vm_value_to_json(&parallel)
    );
    assert_eq!(field(&parallel, "status").display(), "complete");
    assert_eq!(field(&parallel, "match_count").as_int(), Some(6));
    assert_eq!(field(&parallel, "files_scanned").as_int(), Some(2));
    let VmValue::List(roots) = field(&parallel, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "id").display(), "a-root");
    let VmValue::List(groups) = field(&roots[1], "patterns") else {
        panic!("patterns must be a list");
    };
    assert_eq!(field(&groups[0], "id").display(), "prefix");
    assert_eq!(field(&groups[0], "count").as_int(), Some(2));
    assert_eq!(field(&groups[1], "count").as_int(), Some(2));
}

#[cfg(unix)]
#[test]
fn find_evidence_follows_symlinks_only_when_requested() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("linked.txt"), "needle\n").unwrap();
    symlink(external.path(), root.path().join("linked")).unwrap();
    let roots = VmValue::List(Arc::new(vec![evidence_root("repo", root.path())]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")]));
    let mut vm = vm();

    let default_receipt = call(
        &mut vm,
        "find_evidence",
        vec![roots.clone(), patterns.clone()],
    )
    .unwrap();
    assert_eq!(field(&default_receipt, "match_count").as_int(), Some(0));

    let followed = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("follow_symlinks", b(true))])],
    )
    .unwrap();
    assert_eq!(field(&followed, "status").display(), "complete");
    assert_eq!(field(&followed, "match_count").as_int(), Some(1));
    let VmValue::List(roots) = field(&followed, "roots") else {
        panic!("roots must be a list");
    };
    let VmValue::List(groups) = field(&roots[0], "patterns") else {
        panic!("patterns must be a list");
    };
    let VmValue::List(hits) = field(&groups[0], "hits") else {
        panic!("hits must be a list");
    };
    assert_eq!(field(&hits[0], "path").display(), "linked/linked.txt");
}

#[test]
fn find_evidence_background_returns_typed_feedback() {
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-evidence-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_evidence",
        vec![
            VmValue::List(Arc::new(vec![evidence_root("repo", root.path())])),
            VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")])),
            dict(vec![("background", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "find_evidence");
    let payload = drain_feedback(&session_id, &response["handle_id"].display());

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["operation"], "find_evidence");
    assert_eq!(payload["result"]["schema_version"], "harn.fs.evidence.v1");
    assert_eq!(payload["result"]["status"], "complete");
    assert_eq!(payload["result"]["match_count"], 1);
}

#[test]
fn find_evidence_settles_failed_roots_and_handles_non_utf8() {
    let readable = tempfile::tempdir().unwrap();
    std::fs::write(
        readable.path().join("bytes.bin"),
        b"before\xffneedle after\n",
    )
    .unwrap();
    std::fs::create_dir_all(readable.path().join("node_modules/pkg")).unwrap();
    std::fs::write(
        readable.path().join("node_modules/pkg/generated.js"),
        "needle\n",
    )
    .unwrap();
    let missing = readable.path().join("missing");
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("missing", &missing),
        evidence_root("readable", readable.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")]));
    let mut vm = vm();

    let receipt = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("preset", s("source"))])],
    )
    .unwrap();
    assert_eq!(field(&receipt, "status").display(), "partial");
    assert_eq!(field(&receipt, "failed_root_count").as_int(), Some(1));
    assert_eq!(field(&receipt, "match_count").as_int(), Some(1));
    let VmValue::List(roots) = field(&receipt, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "status").display(), "failed");
    assert_eq!(field(&roots[1], "status").display(), "complete");
}

#[test]
fn find_evidence_applies_stable_global_and_root_budgets() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("a.txt"), "x x x\n").unwrap();
    std::fs::write(second.path().join("b.txt"), "x x x\n").unwrap();
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("b", second.path()),
        evidence_root("a", first.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("x", "x")]));
    let mut vm = vm();

    let receipt = call(
        &mut vm,
        "find_evidence",
        vec![
            roots,
            patterns,
            dict(vec![
                ("max_matches", VmValue::Int(3)),
                ("max_matches_per_root", VmValue::Int(2)),
                ("threads", VmValue::Int(2)),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(field(&receipt, "status").display(), "truncated");
    assert_eq!(field(&receipt, "match_count").as_int(), Some(3));
    let VmValue::List(roots) = field(&receipt, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "match_count").as_int(), Some(2));
    assert_eq!(field(&roots[1], "match_count").as_int(), Some(1));
}

#[test]
fn find_evidence_rejects_duplicate_or_empty_labels() {
    let root = tempfile::tempdir().unwrap();
    let duplicate_roots = VmValue::List(Arc::new(vec![
        evidence_root("same", root.path()),
        evidence_root("same", root.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("p", "x")]));
    let mut vm = vm();
    let error = call(&mut vm, "find_evidence", vec![duplicate_roots, patterns]).unwrap_err();
    assert!(error.to_string().contains("duplicate roots id `same`"));

    let roots = VmValue::List(Arc::new(vec![evidence_root("root", root.path())]));
    let empty_patterns = VmValue::List(Arc::new(vec![evidence_pattern("", "x")]));
    let error = call(&mut vm, "find_evidence", vec![roots, empty_patterns]).unwrap_err();
    assert!(error.to_string().contains("patterns id must not be empty"));
}
