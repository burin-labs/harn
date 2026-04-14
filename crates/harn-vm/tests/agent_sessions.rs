//! Integration tests for first-class sessions.

use harn_vm::value::VmError;

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

#[test]
fn open_mints_and_is_idempotent() {
    let lines = out(r#"
pipeline main(task) {
  let a = agent_session_open()
  let b = agent_session_open(a)
  log(a == b)
  log(agent_session_exists(a))
}
"#);
    assert_eq!(lines, vec!["true", "true"]);
}

#[test]
fn inject_then_length_and_snapshot() {
    let lines = out(r#"
pipeline main(task) {
  let s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "hello"})
  agent_session_inject(s, {role: "assistant", content: "hi"})
  log(agent_session_length(s))
  let snap = agent_session_snapshot(s)
  log(len(snap["messages"]))
}
"#);
    assert_eq!(lines, vec!["2", "2"]);
}

#[test]
fn reset_clears_history_preserves_id() {
    let lines = out(r#"
pipeline main(task) {
  let s = agent_session_open()
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
fn fork_is_independent_in_both_directions() {
    let lines = out(r#"
pipeline main(task) {
  let src = agent_session_open()
  agent_session_inject(src, {role: "user", content: "shared"})
  let dst = agent_session_fork(src)
  log(agent_session_length(dst))

  agent_session_inject(src, {role: "user", content: "src-only"})
  agent_session_inject(dst, {role: "user", content: "dst-only-1"})
  agent_session_inject(dst, {role: "user", content: "dst-only-2"})

  log(agent_session_length(src))
  log(agent_session_length(dst))
  log(src == dst)
}
"#);
    assert_eq!(lines, vec!["1", "2", "3", "false"]);
}

#[test]
fn trim_retains_last_n() {
    let lines = out(r#"
pipeline main(task) {
  let s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "a"})
  agent_session_inject(s, {role: "user", content: "b"})
  agent_session_inject(s, {role: "user", content: "c"})
  agent_session_inject(s, {role: "user", content: "d"})
  let kept = agent_session_trim(s, 2)
  log(kept)
  log(agent_session_length(s))
  let snap = agent_session_snapshot(s)
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
  let s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "only"})
  log(agent_session_trim(s, 100))
}
"#);
    assert_eq!(lines, vec!["1"]);
}

#[test]
fn close_removes_session() {
    let lines = out(r#"
pipeline main(task) {
  let s = agent_session_open()
  agent_session_close(s)
  log(agent_session_exists(s))
}
"#);
    assert_eq!(lines, vec!["false"]);
}

#[test]
fn inject_without_role_errors() {
    let err = run(r#"
pipeline main(task) {
  let s = agent_session_open()
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
  let snap = agent_session_snapshot("nope")
  log(snap == nil)
}
"#);
    assert_eq!(lines, vec!["false", "true"]);
}

#[test]
fn lru_eviction_kicks_in_at_cap() {
    harn_vm::reset_thread_local_state();
    harn_vm::agent_sessions::set_session_cap(3);
    let a = harn_vm::agent_sessions::open_or_create(Some("a".to_string()));
    let _b = harn_vm::agent_sessions::open_or_create(Some("b".to_string()));
    let _c = harn_vm::agent_sessions::open_or_create(Some("c".to_string()));
    // touch a so b becomes the least-recent
    harn_vm::agent_sessions::open_or_create(Some(a.clone()));
    let _d = harn_vm::agent_sessions::open_or_create(Some("d".to_string()));
    assert!(harn_vm::agent_sessions::exists("a"));
    assert!(!harn_vm::agent_sessions::exists("b"), "b should be evicted");
    assert!(harn_vm::agent_sessions::exists("c"));
    assert!(harn_vm::agent_sessions::exists("d"));
    harn_vm::agent_sessions::set_session_cap(harn_vm::agent_sessions::DEFAULT_SESSION_CAP);
}

#[test]
fn compact_unknown_key_errors() {
    let err = run(r#"
pipeline main(task) {
  let s = agent_session_open()
  agent_session_compact(s, {bogus: 1})
}
"#)
    .unwrap_err();
    assert!(err.contains("bogus"), "got: {err}");
}
