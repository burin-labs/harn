use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::*;

#[test]
fn interrupted_case_preserves_prior_verdict_and_unfinished_start() {
    let cases = [
        ConformanceCase::WorkspaceWriteAdmitted,
        ConformanceCase::OutsideWriteRefused,
    ];
    let receipts = RefCell::new(Vec::new());
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_cases(
            &cases,
            |case| {
                // The start must already be visible when execution begins.
                let completed = usize::from(case == cases[1]);
                assert_eq!(
                    receipts.borrow().last().unwrap(),
                    &format!(
                        "harn.sandbox_conformance_start case={} completed={completed} pending={}",
                        case.id(),
                        cases.len() - completed,
                    ),
                );
                assert!(case != cases[1], "interrupted probe");
                broken(case, "fixture setup failed".to_string())
            },
            &mut |progress: ConformanceProgress<'_>| {
                receipts.borrow_mut().push(progress.receipt_line());
            },
        )
    }));
    let error = result.expect_err("the second case interrupts the aggregate");
    assert_eq!(error.downcast_ref::<&str>(), Some(&"interrupted probe"));
    let receipts = receipts.into_inner();
    assert_eq!(receipts.len(), 3);
    assert_eq!(
        receipts[1],
        broken(cases[0], "fixture setup failed".to_string()).receipt_line(),
    );
    assert_eq!(
        receipts[2],
        "harn.sandbox_conformance_start case=fs.outside_write_refused completed=1 pending=1",
    );
}
