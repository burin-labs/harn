use super::*;

#[tokio::test]
async fn selected_test_file_entries_compile_once_and_run_in_fresh_vms() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write("harn.toml", "[check]\ntrusted_host_dispatch = true\n");
    temp.write(
        "suite/counter.harn",
        r"
let calls = 0

pub fn next_call() {
  calls = calls + 1
  return calls
}
",
    );
    temp.write(
        "suite/test_compile_once.harn",
        r#"
import { next_call } from "./counter.harn"

@test(cases: [
  {name: "one", args: [1]},
  {name: "two", args: [2]},
  {name: "three", args: [3]},
])
pipeline test_rows(value) {
  assert_eq(next_call(), 1)
  assert(value > 0)
}

pipeline test_sibling(_task) {
  assert_eq(next_call(), 1)
}
"#,
    );
    let session = TestRunSession::default();
    let mut options = RunOptions::new(5_000);
    options.trusted_host_dispatch = true;

    let summary = run_tests_with_session(
        &temp.path().join("suite/test_compile_once.harn"),
        &options,
        &session,
    )
    .await;

    assert_eq!(summary.passed, 4, "{:?}", summary.results);
    assert_eq!(summary.aggregate.test_files_compiled, 1);
    assert_eq!(summary.aggregate.test_entries_compiled, 2);
    assert_eq!(summary.aggregate.modules.modules_compiled, 1);
    assert!(summary.aggregate.test_file_compile_ms <= summary.aggregate.compile_ms);
    assert!(summary.results.iter().all(|result| result
        .phases
        .is_some_and(|phases| { phases.compile_ms == 0 && phases.modules.modules_compiled == 0 })));
    let stats = session.stats();
    assert_eq!(stats.test_files_compiled, 1);
    assert_eq!(stats.test_entries_compiled, 2);
    assert_eq!(stats.insertions, 1);
    assert!(stats.hits >= 4, "{stats:?}");

    let dev_session = TestRunSession::default();
    let dev_results = run_test_file_with_session(
        &temp.path().join("suite/test_compile_once.harn"),
        None,
        5_000,
        None,
        &[],
        &dev_session,
    )
    .await
    .unwrap();
    assert_eq!(dev_results.len(), 4);
    assert!(dev_results.iter().all(|result| result.passed));
    assert!(dev_results.iter().all(|result| result
        .phases
        .is_some_and(|phases| { phases.compile_ms == 0 && phases.modules.modules_compiled == 0 })));
    let dev_stats = dev_session.stats();
    assert_eq!(dev_stats.test_files_compiled, 1);
    assert_eq!(dev_stats.test_entries_compiled, 2);
    assert_eq!(dev_stats.insertions, 1);
    assert!(dev_stats.hits >= 4, "{dev_stats:?}");
}
