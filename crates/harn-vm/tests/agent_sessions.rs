#![recursion_limit = "256"]
//! Integration tests for first-class sessions.

use harn_vm::value::VmError;
use tempfile::tempdir;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out(source: &str) -> Vec<String> {
    let raw = run(source).unwrap();
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

fn harn_string(value: &str) -> String {
    serde_json::to_string(value).expect("string literal")
}

#[test]
fn open_mints_and_is_idempotent() {
    let lines = out(r"
pipeline main(task) {
  const a = agent_session_open()
  const b = agent_session_open(a)
  log(a == b)
  log(agent_session_exists(a))
}
");
    assert_eq!(lines, vec!["true", "true"]);
}

#[test]
fn inject_then_length_and_snapshot() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "hello"})
  agent_session_inject(s, {role: "assistant", content: "hi"})
  log(agent_session_length(s))
  const snap = agent_session_snapshot(s)
  log(len(snap["messages"]))
  log(snap["parent_id"] == nil)
  log(len(snap["child_ids"]))
  log(snap["branched_at_event_index"] == nil)
}
"#);
    assert_eq!(lines, vec!["2", "2", "true", "0", "true"]);
}

#[test]
fn reset_clears_history_preserves_id() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "a"})
  agent_session_inject(s, {role: "user", content: "b"})
  agent_session_reset(s)
  log(agent_session_length(s))
  log(agent_session_exists(s))
}
"#);
    assert_eq!(lines, vec!["0", "true"]);
}

#[test]
fn tool_format_contract_is_first_class_and_resettable() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open("tool-contract")
  log(agent_session_tool_format(s) == nil)
  agent_session_claim_tool_format(s, "native")
  log(agent_session_tool_format(s))
  const conflict = try {
    agent_session_claim_tool_format(s, "text")
  }
  log(is_err(conflict))
  agent_session_reset(s)
  log(agent_session_tool_format(s) == nil)
  agent_session_claim_tool_format(s, "text")
  log(agent_session_tool_format(s))
}
"#);
    assert_eq!(lines, vec!["true", "native", "true", "true", "text"]);
}

#[test]
fn agent_loop_rejects_tool_format_switch_on_same_session() {
    let lines = out(r#"
pipeline main(task) {
  llm_mock_clear()
  llm_mock({text: "first ##DONE##"})
  const s = agent_session_open("agent-loop-tool-contract")
  const first = agent_loop(
    "first turn",
    nil,
    {provider: "mock", model: "mock", session_id: s, max_iterations: 1, tool_format: "text"},
  )
  log(first?.tools?.mode)
  log(agent_session_tool_format(s))
  const switched = try {
    agent_loop(
      "second turn",
      nil,
      {provider: "mock", model: "mock", session_id: s, max_iterations: 1, tool_format: "native"},
    )
  }
  log(is_err(switched))
  log(contains(json_stringify(unwrap_err(switched)), "tool_format"))
}
"#);
    assert_eq!(lines, vec!["text", "text", "true", "true"]);
}

#[test]
fn fork_is_independent_in_both_directions() {
    let lines = out(r#"
pipeline main(task) {
  const src = agent_session_open()
  agent_session_inject(src, {role: "user", content: "shared"})
  const dst = agent_session_fork(src)
  const src_snap = agent_session_snapshot(src)
  const dst_snap = agent_session_snapshot(dst)
  const dst_ancestry = agent_session_ancestry(dst)
  log(agent_session_length(dst))
  log(dst_snap["parent_id"] == src)
  log(len(src_snap["child_ids"]))
  log(dst_ancestry["root_id"] == src)

  agent_session_inject(src, {role: "user", content: "src-only"})
  agent_session_inject(dst, {role: "user", content: "dst-only-1"})
  agent_session_inject(dst, {role: "user", content: "dst-only-2"})

  log(agent_session_length(src))
  log(agent_session_length(dst))
  log(src == dst)
}
"#);
    assert_eq!(lines, vec!["1", "true", "1", "true", "2", "3", "false"]);
}

#[test]
fn fork_carries_tool_format_contract() {
    let lines = out(r#"
pipeline main(task) {
  const src = agent_session_open("tool-fork-src")
  agent_session_claim_tool_format(src, "native")
  const dst = agent_session_fork(src, "tool-fork-dst")
  log(agent_session_tool_format(dst))
  const conflict = try {
    agent_session_claim_tool_format(dst, "text")
  }
  log(is_err(conflict))
}
"#);
    assert_eq!(lines, vec!["native", "true"]);
}

#[test]
fn fork_at_records_branch_index_and_root_lineage() {
    let lines = out(r#"
pipeline main(task) {
  const root = agent_session_open("root")
  agent_session_inject(root, {role: "user", content: "a"})
  agent_session_inject(root, {role: "assistant", content: "b"})
  agent_session_inject(root, {role: "user", content: "c"})
  const branch = agent_session_fork_at(root, 2, "branch")
  const snap = agent_session_snapshot(branch)
  const ancestry = agent_session_ancestry(branch)
  log(agent_session_length(branch))
  log(snap["branched_at_event_index"])
  log(ancestry["parent_id"] == root)
  log(ancestry["root_id"] == root)
}
"#);
    assert_eq!(lines, vec!["2", "2", "true", "true"]);
}

#[test]
fn trim_retains_last_n() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "a"})
  agent_session_inject(s, {role: "user", content: "b"})
  agent_session_inject(s, {role: "user", content: "c"})
  agent_session_inject(s, {role: "user", content: "d"})
  const kept = agent_session_trim(s, 2)
  log(kept)
  log(agent_session_length(s))
  const snap = agent_session_snapshot(s)
  log(snap["messages"][0]["content"])
  log(snap["messages"][1]["content"])
}
"#);
    assert_eq!(lines, vec!["2", "2", "c", "d"]);
}

#[test]
fn trim_clamps_to_available() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "only"})
  log(agent_session_trim(s, 100))
}
"#);
    assert_eq!(lines, vec!["1"]);
}

#[test]
fn close_removes_session() {
    let lines = out(r"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_close(s)
  log(agent_session_exists(s))
}
");
    assert_eq!(lines, vec!["false"]);
}

#[test]
fn inject_without_role_errors() {
    let err = run(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_inject(s, {content: "oops"})
}
"#)
    .unwrap_err();
    assert!(err.to_lowercase().contains("role"), "got: {err}");
}

#[test]
fn operations_on_unknown_session_error() {
    for op in [
        r#"agent_session_reset("does-not-exist")"#,
        r#"agent_session_fork("does-not-exist")"#,
        r#"agent_session_close("does-not-exist")"#,
        r#"agent_session_trim("does-not-exist", 1)"#,
        r#"agent_session_inject("does-not-exist", {role: "user"})"#,
        r#"agent_session_length("does-not-exist")"#,
    ] {
        let src = format!("pipeline main(task) {{ {op} }}");
        let err = run(&src).unwrap_err();
        assert!(
            err.contains("does-not-exist") || err.to_lowercase().contains("unknown"),
            "{op} => {err}"
        );
    }
}

#[test]
fn exists_and_snapshot_on_unknown_are_safe() {
    let lines = out(r#"
pipeline main(task) {
  log(agent_session_exists("nope"))
  const snap = agent_session_snapshot("nope")
  log(snap == nil)
  const ancestry = agent_session_ancestry("nope")
  log(ancestry == nil)
}
"#);
    assert_eq!(lines, vec!["false", "true", "true"]);
}

#[test]
fn fork_at_on_unknown_or_negative_keep_first_errors() {
    for op in [
        r#"agent_session_fork_at("does-not-exist", 1)"#,
        r"
const s = agent_session_open()
agent_session_fork_at(s, -1)
",
    ] {
        let src = format!("pipeline main(task) {{ {op} }}");
        let err = run(&src).unwrap_err();
        assert!(
            err.contains("does-not-exist")
                || err.contains("keep_first")
                || err.to_lowercase().contains("unknown"),
            "{op} => {err}"
        );
    }
}

#[test]
fn lru_eviction_kicks_in_at_cap() {
    harn_vm::reset_thread_local_state();
    harn_vm::agent_sessions::set_session_cap(3);
    let a = harn_vm::agent_sessions::open_or_create(Some("a".to_string()));
    let _b = harn_vm::agent_sessions::open_or_create(Some("b".to_string()));
    let _c = harn_vm::agent_sessions::open_or_create(Some("c".to_string()));
    // touch a so b becomes the least-recent
    harn_vm::agent_sessions::open_or_create(Some(a));
    let _d = harn_vm::agent_sessions::open_or_create(Some("d".to_string()));
    assert!(harn_vm::agent_sessions::exists("a"));
    assert!(!harn_vm::agent_sessions::exists("b"), "b should be evicted");
    assert!(harn_vm::agent_sessions::exists("c"));
    assert!(harn_vm::agent_sessions::exists("d"));
    harn_vm::agent_sessions::set_session_cap(harn_vm::agent_sessions::DEFAULT_SESSION_CAP);
}

#[test]
fn compact_unknown_key_errors() {
    let err = run(r"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_compact(s, {bogus: 1})
}
")
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn open_pins_workspace_anchor_and_surfaces_in_snapshot() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open(
    "anchor-open",
    {
      workspace_anchor: {
        primary: "/workspace/main",
        anchored_at: "2026-05-23T00:00:00Z",
      },
    },
  )
  log(agent_session_workspace_anchor(s)["primary"])
  const snap = agent_session_snapshot(s)
  log(snap["workspace_anchor"]["primary"])
  log(snap["workspace_anchor"]["anchored_at"])
  log(len(snap["workspace_anchor"]["additional_roots"]))
}
"#);
    assert_eq!(
        lines,
        vec![
            "/workspace/main",
            "/workspace/main",
            "2026-05-23T00:00:00Z",
            "0"
        ]
    );
}

#[test]
fn set_workspace_anchor_replaces_and_clears() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open("anchor-set")
  log(agent_session_workspace_anchor(s) == nil)
  const changed = agent_session_set_workspace_anchor(s, {
    primary: "/workspace/initial",
    anchored_at: "2026-05-23T00:00:00Z",
  })
  log(changed)
  log(agent_session_workspace_anchor(s)["primary"])
  const same = agent_session_set_workspace_anchor(s, {
    primary: "/workspace/initial",
    anchored_at: "2026-05-23T00:00:00Z",
  })
  log(same)
  const cleared = agent_session_set_workspace_anchor(s, nil)
  log(cleared)
  log(agent_session_workspace_anchor(s) == nil)
}
"#);
    assert_eq!(
        lines,
        vec![
            "true",
            "true",
            "/workspace/initial",
            "false",
            "true",
            "true"
        ]
    );
}

#[test]
fn workspace_anchor_round_trips_through_fork() {
    let lines = out(r#"
pipeline main(task) {
  const src = agent_session_open("anchor-fork-src", {
    workspace_anchor: {
      primary: "/workspace/main",
      additional_roots: [
        {path: "/workspace/lib", mount_mode: "read_only", mounted_at: "2026-05-23T00:00:00Z"},
      ],
      anchored_at: "2026-05-23T00:00:00Z",
    },
  })
  const dst = agent_session_fork(src, "anchor-fork-dst")
  const anchor = agent_session_workspace_anchor(dst)
  log(anchor["primary"])
  log(len(anchor["additional_roots"]))
  log(anchor["additional_roots"][0]["mount_mode"])
}
"#);
    assert_eq!(lines, vec!["/workspace/main", "1", "read_only"]);
}

#[test]
fn open_rejects_unknown_option_keys() {
    let err = run(r"
pipeline main(task) {
  agent_session_open(nil, {bogus: 1})
}
")
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn workspace_anchor_requires_primary() {
    let err = run(r#"
pipeline main(task) {
  agent_session_open(nil, {workspace_anchor: {anchored_at: "2026-05-23T00:00:00Z"}})
}
"#)
    .unwrap_err();
    assert!(err.contains("primary"), "got: {err}");
}

#[test]
fn workspace_anchor_rejects_unknown_mount_mode() {
    let err = run(r#"
pipeline main(task) {
  agent_session_open(nil, {
    workspace_anchor: {
      primary: "/workspace/main",
      additional_roots: [{path: "/workspace/lib", mount_mode: "bogus"}],
    },
  })
}
"#)
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn add_root_uses_default_mount_mode_emits_event_and_removes_cleanly() {
    let primary = tempdir().expect("primary tempdir");
    let mounted = tempdir().expect("mounted tempdir");
    let primary_path = primary.path().display().to_string();
    let mounted_path = std::fs::canonicalize(mounted.path())
        .expect("canonical mounted tempdir")
        .display()
        .to_string();
    let primary_literal = harn_string(&primary_path);
    let mounted_literal = harn_string(&mounted_path);
    let lines = out(&format!(
        r#"
pipeline main(task) {{
  const s = agent_session_open("anchor-roots", {{
    workspace_policy: {{default_mount_mode: "extend"}},
    workspace_anchor: {{
      primary: {primary_literal},
      anchored_at: "2026-05-24T00:00:00Z",
    }},
  }})
  const added = agent_session_add_root(s, {mounted_literal}, {{reason: "shared"}})
  log(added.ok)
  log(added.mounted_at != nil)
  const roots = agent_session_list_roots(s)
  log(roots["primary"])
  log(len(roots["additional"]))
  log(roots["additional"][0]["mount_mode"])
  const mounted_events = transcript_events_by_kind(agent_session_snapshot(s), "RootMounted")
  log(len(mounted_events))
  log(mounted_events[0]["metadata"]["path"])
  log(mounted_events[0]["metadata"]["mount_mode"])
  log(mounted_events[0]["metadata"]["reason"])

  const updated = agent_session_add_root(s, {mounted_literal}, {{mount_mode: "sandboxed"}})
  log(updated.ok)
  log(agent_session_list_roots(s)["additional"][0]["mount_mode"])
  log(len(agent_session_list_roots(s)["additional"]))

  const removed = agent_session_remove_root(s, {mounted_literal})
  log(removed.ok)
  log(len(agent_session_list_roots(s)["additional"]))
  const missing = agent_session_remove_root(s, {mounted_literal})
  log(missing.ok)
}}
"#
    ));
    assert_eq!(
        lines,
        vec![
            "true".to_string(),
            "true".to_string(),
            primary_path,
            "1".to_string(),
            "extend".to_string(),
            "1".to_string(),
            mounted_path,
            "extend".to_string(),
            "shared".to_string(),
            "true".to_string(),
            "sandboxed".to_string(),
            "1".to_string(),
            "true".to_string(),
            "0".to_string(),
            "true".to_string(),
        ]
    );
}

#[test]
fn add_root_reports_missing_directory_in_result_envelope() {
    let primary = tempdir().expect("primary tempdir");
    let missing = primary.path().join("missing-root");
    let primary_path = primary.path().display().to_string();
    let missing_path = missing.display().to_string();
    let primary_literal = harn_string(&primary_path);
    let missing_literal = harn_string(&missing_path);
    let lines = out(&format!(
        r#"
pipeline main(task) {{
  const s = agent_session_open("anchor-roots-missing", {{
    workspace_anchor: {{
      primary: {primary_literal},
      anchored_at: "2026-05-24T00:00:00Z",
    }},
  }})
  const added = agent_session_add_root(s, {missing_literal})
  log(added.ok)
  log(contains(added.error ?? "", "must exist"))
}}
"#
    ));
    assert_eq!(lines, vec!["false", "true"]);
}
