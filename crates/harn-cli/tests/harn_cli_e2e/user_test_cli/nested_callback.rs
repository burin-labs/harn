use crate::test_util::cli::binary_path;
use std::process::Command;

#[test]
fn nested_argument_with_an_empty_callback_preserves_later_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("callbacks.harn"),
        r#"
pub type State = {values: list<{state: string}>, pending: list<int>}
pub type Effects = {
  read: fn(string) -> unknown,
  post: fn(string) -> nil,
  save: fn(State) -> nil,
}

pub fn inner(effects: Effects, original: State) -> State {
  const next = original + {pending: []}
  effects.save(next)
  return next
}

pub fn outer(effects: Effects, state: State, now: int) -> State {
  require now > 0, "positive clock required"
  for value in state.values {
    if value.state != "new" { continue }
    effects.post("request")
  }
  return state
}

pub fn recover(effects: Effects, original: State, now: int) -> State {
  const state = outer(effects, inner(effects, original), now)
  for value in state.values {
    const _ = effects.read(value.state)
  }
  return state
}
"#,
    )
    .unwrap();
    let suite = temp.path().join("nested-callback-test.harn");
    std::fs::write(
        &suite,
        r#"
import { Effects, recover } from "./callbacks"

pipeline test_nested_callback_argument() {
  for result in ["failure", "cancelled", "skipped", nil] {
    const effects: Effects = {
      read: fn(value) { assert_eq(value, "ready"); return {result: result} },
      post: fn(_) { throw "unexpected request" },
      save: fn(_) {},
    }
    const state = recover(effects, {values: [{state: "ready"}], pending: [1]}, 7)
    assert_eq(state.pending, [])
    assert_eq(state.values, [{state: "ready"}])
  }
}
"#,
    )
    .unwrap();
    let report = temp.path().join("report.json");
    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .arg("test")
        .arg(&suite)
        .arg("--json-out")
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(report["summary"]["total"], 1);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["summary"]["skipped"], 0);
}
