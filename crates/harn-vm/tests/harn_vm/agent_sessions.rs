//! Integration tests for first-class sessions.

use harn_vm::value::VmError;
use tempfile::tempdir;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let source = format!("import {{ agent_loop }} from \"std/agent/loop\"\n{source}");
    let chunk = harn_vm::compile_source(&source)?;
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
pipeline main(harness: Harness, task: unknown) {
  const a = harness.agent.open()
  const b = harness.agent.open(a)
  harness.stdio.log(a == b)
  harness.stdio.log(harness.agent.exists(a))
}
");
    assert_eq!(lines, vec!["true", "true"]);
}

#[test]
fn inject_then_length_and_snapshot() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.inject(s, {role: "user", content: "hello"})
  harness.agent.inject(s, {role: "assistant", content: "hi"})
  harness.stdio.log(harness.agent.length(s))
  const snap = harness.agent.snapshot(s)
  harness.stdio.log(len(snap["messages"]))
  harness.stdio.log(snap["parent_id"] == nil)
  harness.stdio.log(len(snap["child_ids"]))
  harness.stdio.log(snap["branched_at_event_index"] == nil)
}
"#);
    assert_eq!(lines, vec!["2", "2", "true", "0", "true"]);
}

#[test]
fn reset_clears_history_preserves_id() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.inject(s, {role: "user", content: "a"})
  harness.agent.inject(s, {role: "user", content: "b"})
  harness.agent.reset(s)
  harness.stdio.log(harness.agent.length(s))
  harness.stdio.log(harness.agent.exists(s))
}
"#);
    assert_eq!(lines, vec!["0", "true"]);
}

#[test]
fn tool_format_contract_is_first_class_and_resettable() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open("tool-contract")
  harness.stdio.log(harness.agent.tool_format(s) == nil)
  harness.agent.claim_tool_format(s, "native")
  harness.stdio.log(harness.agent.tool_format(s))
  const conflict = try {
    harness.agent.claim_tool_format(s, "text")
  }
  harness.stdio.log(is_err(conflict))
  harness.agent.reset(s)
  harness.stdio.log(harness.agent.tool_format(s) == nil)
  harness.agent.claim_tool_format(s, "text")
  harness.stdio.log(harness.agent.tool_format(s))
}
"#);
    assert_eq!(lines, vec!["true", "native", "true", "true", "text"]);
}

#[test]
fn agent_loop_rejects_tool_format_switch_on_same_session() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({text: "first ##DONE##"})
  const s = harness.agent.open("agent-loop-tool-contract")
  const first = agent_loop(harness,
    "first turn",
    nil,
    {provider: "mock", model: "mock", session_id: s, max_iterations: 1, tool_format: "text"},
  )
  harness.stdio.log(first?.tools?.mode)
  harness.stdio.log(harness.agent.tool_format(s))
  const switched = try {
    agent_loop(harness,
      "second turn",
      nil,
      {provider: "mock", model: "mock", session_id: s, max_iterations: 1, tool_format: "native"},
    )
  }
  harness.stdio.log(is_err(switched))
  harness.stdio.log(contains(json_stringify(unwrap_err(switched)), "tool_format"))
}
"#);
    assert_eq!(lines, vec!["text", "text", "true", "true"]);
}

#[test]
fn fork_is_independent_in_both_directions() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const src = harness.agent.open()
  harness.agent.inject(src, {role: "user", content: "shared"})
  const dst = harness.agent.fork(src)
  const src_snap = harness.agent.snapshot(src)
  const dst_snap = harness.agent.snapshot(dst)
  const dst_ancestry = harness.agent.ancestry(dst)
  harness.stdio.log(harness.agent.length(dst))
  harness.stdio.log(dst_snap["parent_id"] == src)
  harness.stdio.log(len(src_snap["child_ids"]))
  harness.stdio.log(dst_ancestry["root_id"] == src)

  harness.agent.inject(src, {role: "user", content: "src-only"})
  harness.agent.inject(dst, {role: "user", content: "dst-only-1"})
  harness.agent.inject(dst, {role: "user", content: "dst-only-2"})

  harness.stdio.log(harness.agent.length(src))
  harness.stdio.log(harness.agent.length(dst))
  harness.stdio.log(src == dst)
}
"#);
    assert_eq!(lines, vec!["1", "true", "1", "true", "2", "3", "false"]);
}

#[test]
fn fork_carries_tool_format_contract() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const src = harness.agent.open("tool-fork-src")
  harness.agent.claim_tool_format(src, "native")
  const dst = harness.agent.fork(src, "tool-fork-dst")
  harness.stdio.log(harness.agent.tool_format(dst))
  const conflict = try {
    harness.agent.claim_tool_format(dst, "text")
  }
  harness.stdio.log(is_err(conflict))
}
"#);
    assert_eq!(lines, vec!["native", "true"]);
}

#[test]
fn fork_at_records_branch_index_and_root_lineage() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const root = harness.agent.open("root")
  harness.agent.inject(root, {role: "user", content: "a"})
  harness.agent.inject(root, {role: "assistant", content: "b"})
  harness.agent.inject(root, {role: "user", content: "c"})
  const branch = harness.agent.fork_at(root, 2, "branch")
  const snap = harness.agent.snapshot(branch)
  const ancestry = harness.agent.ancestry(branch)
  harness.stdio.log(harness.agent.length(branch))
  harness.stdio.log(snap["branched_at_event_index"])
  harness.stdio.log(ancestry["parent_id"] == root)
  harness.stdio.log(ancestry["root_id"] == root)
}
"#);
    assert_eq!(lines, vec!["2", "2", "true", "true"]);
}

#[test]
fn trim_retains_last_n() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.inject(s, {role: "user", content: "a"})
  harness.agent.inject(s, {role: "user", content: "b"})
  harness.agent.inject(s, {role: "user", content: "c"})
  harness.agent.inject(s, {role: "user", content: "d"})
  const kept = harness.agent.trim(s, 2)
  harness.stdio.log(kept)
  harness.stdio.log(harness.agent.length(s))
  const snap = harness.agent.snapshot(s)
  harness.stdio.log(snap["messages"][0]["content"])
  harness.stdio.log(snap["messages"][1]["content"])
}
"#);
    assert_eq!(lines, vec!["2", "2", "c", "d"]);
}

#[test]
fn trim_clamps_to_available() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.inject(s, {role: "user", content: "only"})
  harness.stdio.log(harness.agent.trim(s, 100))
}
"#);
    assert_eq!(lines, vec!["1"]);
}

#[test]
fn close_removes_session() {
    let lines = out(r"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.close(s)
  harness.stdio.log(harness.agent.exists(s))
}
");
    assert_eq!(lines, vec!["false"]);
}

#[test]
fn inject_without_role_errors() {
    let err = run(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.inject(s, {content: "oops"})
}
"#)
    .unwrap_err();
    assert!(err.to_lowercase().contains("role"), "got: {err}");
}

#[test]
fn operations_on_unknown_session_error() {
    for op in [
        r#"harness.agent.reset("does-not-exist")"#,
        r#"harness.agent.fork("does-not-exist")"#,
        r#"harness.agent.close("does-not-exist")"#,
        r#"harness.agent.trim("does-not-exist", 1)"#,
        r#"harness.agent.inject("does-not-exist", {role: "user"})"#,
        r#"harness.agent.length("does-not-exist")"#,
    ] {
        let src = format!("pipeline main(harness: Harness, task: unknown) {{ {op} }}");
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
pipeline main(harness: Harness, task: unknown) {
  harness.stdio.log(harness.agent.exists("nope"))
  const snap = harness.agent.snapshot("nope")
  harness.stdio.log(snap == nil)
  const ancestry = harness.agent.ancestry("nope")
  harness.stdio.log(ancestry == nil)
}
"#);
    assert_eq!(lines, vec!["false", "true", "true"]);
}

#[test]
fn fork_at_on_unknown_or_negative_keep_first_errors() {
    for op in [
        r#"harness.agent.fork_at("does-not-exist", 1)"#,
        r"
const s = harness.agent.open()
harness.agent.fork_at(s, -1)
",
    ] {
        let src = format!("pipeline main(harness: Harness, task: unknown) {{ {op} }}");
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
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open()
  harness.agent.compact(s, {bogus: 1})
}
")
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn open_pins_workspace_anchor_and_surfaces_in_snapshot() {
    let lines = out(r#"
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open(
    "anchor-open",
    {
      workspace_anchor: {
        primary: "/workspace/main",
        anchored_at: "2026-05-23T00:00:00Z",
      },
    },
  )
  harness.stdio.log(harness.agent.workspace_anchor(s)["primary"])
  const snap = harness.agent.snapshot(s)
  harness.stdio.log(snap["workspace_anchor"]["primary"])
  harness.stdio.log(snap["workspace_anchor"]["anchored_at"])
  harness.stdio.log(len(snap["workspace_anchor"]["additional_roots"]))
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
pipeline main(harness: Harness, task: unknown) {
  const s = harness.agent.open("anchor-set")
  harness.stdio.log(harness.agent.workspace_anchor(s) == nil)
  const changed = harness.agent.set_workspace_anchor(s, {
    primary: "/workspace/initial",
    anchored_at: "2026-05-23T00:00:00Z",
  })
  harness.stdio.log(changed)
  harness.stdio.log(harness.agent.workspace_anchor(s)["primary"])
  const same = harness.agent.set_workspace_anchor(s, {
    primary: "/workspace/initial",
    anchored_at: "2026-05-23T00:00:00Z",
  })
  harness.stdio.log(same)
  const cleared = harness.agent.set_workspace_anchor(s, nil)
  harness.stdio.log(cleared)
  harness.stdio.log(harness.agent.workspace_anchor(s) == nil)
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
pipeline main(harness: Harness, task: unknown) {
  const src = harness.agent.open("anchor-fork-src", {
    workspace_anchor: {
      primary: "/workspace/main",
      additional_roots: [
        {path: "/workspace/lib", mount_mode: "read_only", mounted_at: "2026-05-23T00:00:00Z"},
      ],
      anchored_at: "2026-05-23T00:00:00Z",
    },
  })
  const dst = harness.agent.fork(src, "anchor-fork-dst")
  const anchor = harness.agent.workspace_anchor(dst)
  harness.stdio.log(anchor["primary"])
  harness.stdio.log(len(anchor["additional_roots"]))
  harness.stdio.log(anchor["additional_roots"][0]["mount_mode"])
}
"#);
    assert_eq!(lines, vec!["/workspace/main", "1", "read_only"]);
}

#[test]
fn open_rejects_unknown_option_keys() {
    let err = run(r"
pipeline main(harness: Harness, task: unknown) {
  harness.agent.open(nil, {bogus: 1})
}
")
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn workspace_anchor_requires_primary() {
    let err = run(r#"
pipeline main(harness: Harness, task: unknown) {
  harness.agent.open(nil, {workspace_anchor: {anchored_at: "2026-05-23T00:00:00Z"}})
}
"#)
    .unwrap_err();
    assert!(err.contains("primary"), "got: {err}");
}

#[test]
fn workspace_anchor_rejects_unknown_mount_mode() {
    let err = run(r#"
pipeline main(harness: Harness, task: unknown) {
  harness.agent.open(nil, {
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
pipeline main(harness: Harness, task: unknown) {{
  const s = harness.agent.open("anchor-roots", {{
    workspace_policy: {{default_mount_mode: "extend"}},
    workspace_anchor: {{
      primary: {primary_literal},
      anchored_at: "2026-05-24T00:00:00Z",
    }},
  }})
  const added = harness.agent.add_root(s, {mounted_literal}, {{reason: "shared"}})
  harness.stdio.log(added.ok)
  harness.stdio.log(added.mounted_at != nil)
  const roots = harness.agent.list_roots(s)
  harness.stdio.log(roots["primary"])
  harness.stdio.log(len(roots["additional"]))
  harness.stdio.log(roots["additional"][0]["mount_mode"])
  const mounted_events = transcript_events_by_kind(harness.agent.snapshot(s), "RootMounted")
  harness.stdio.log(len(mounted_events))
  harness.stdio.log(mounted_events[0]["metadata"]["path"])
  harness.stdio.log(mounted_events[0]["metadata"]["mount_mode"])
  harness.stdio.log(mounted_events[0]["metadata"]["reason"])

  const updated = harness.agent.add_root(s, {mounted_literal}, {{mount_mode: "sandboxed"}})
  harness.stdio.log(updated.ok)
  harness.stdio.log(harness.agent.list_roots(s)["additional"][0]["mount_mode"])
  harness.stdio.log(len(harness.agent.list_roots(s)["additional"]))

  const removed = harness.agent.remove_root(s, {mounted_literal})
  harness.stdio.log(removed.ok)
  harness.stdio.log(len(harness.agent.list_roots(s)["additional"]))
  const missing = harness.agent.remove_root(s, {mounted_literal})
  harness.stdio.log(missing.ok)
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
pipeline main(harness: Harness, task: unknown) {{
  const s = harness.agent.open("anchor-roots-missing", {{
    workspace_anchor: {{
      primary: {primary_literal},
      anchored_at: "2026-05-24T00:00:00Z",
    }},
  }})
  const added = harness.agent.add_root(s, {missing_literal})
  harness.stdio.log(added.ok)
  harness.stdio.log(contains(added.error ?? "", "must exist"))
}}
"#
    ));
    assert_eq!(lines, vec!["false", "true"]);
}
