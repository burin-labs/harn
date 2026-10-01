use super::*;

#[tokio::test]
async fn work_is_repeatable_per_case_in_serial_parallel_and_warm_sessions() {
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "counter.harn",
        "pub fn sum(n: int) -> int { let total = 0\n for i in range(0, n) { total = total + i }\n return total }\n",
    );
    temp.write(
        "test_work.harn",
        r#"
import { sum } from "./counter.harn"
pipeline test_small() { assert_eq(sum(10), 45) }
pipeline test_large() { assert_eq(sum(20), 190) }
pipeline test_child() {
  const handle = spawn { sum(20) }
  assert_eq(await(handle), 190)
}
pipeline test_failure() { assert_eq(sum(10), -1) }
"#,
    );
    let session = TestRunSession::default();
    let mut options = RunOptions::new(5_000);
    let path = temp.path().join("test_work.harn");
    let mut reference = None;
    for parallel in [false, false, true] {
        options.parallel = parallel;
        let summary = run_tests_with_session(&path, &options, &session).await;
        assert_eq!(summary.total, 4, "{:?}", summary.results);
        assert_eq!(summary.passed, 3, "{:?}", summary.results);
        assert_eq!(summary.failed, 1);
        let work: BTreeMap<_, _> = summary
            .results
            .iter()
            .map(|case| {
                let steps = case.work.expect("executed case has measured work").vm_steps;
                assert!(steps > 0);
                (case.name.clone(), steps)
            })
            .collect();
        assert!(work["test_large"] > work["test_small"]);
        assert!(work["test_child"] >= work["test_large"]);
        if let Some(reference) = &reference {
            assert_eq!(&work, reference);
        } else {
            reference = Some(work);
        }
    }
}
