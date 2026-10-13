use super::*;

fn complete() -> ConformanceReport {
    ConformanceReport {
        backend: "fixture".to_string(),
        filesystem_mechanism: "fixture".to_string(),
        enforcing: true,
        cases: ConformanceCase::ALL
            .iter()
            .map(|case| CaseReport {
                case: case.id(),
                route: case.route(),
                expected: Some(Observation::Admitted),
                observed: Some(Observation::Admitted),
                target: "fixture".to_string(),
                verdict: Verdict::Conforms,
                detail: String::new(),
            })
            .collect(),
    }
}

#[test]
fn holds_requires_the_complete_case_census_and_measured_verdicts() {
    let mut report = complete();
    assert!(report.holds());
    report.cases.clear();
    assert!(!report.holds(), "an empty report is not a measurement");
    report = complete();
    report.cases.pop();
    assert!(!report.holds(), "a partial report cannot pass");
    report = complete();
    report.cases[1] = report.cases[0].clone();
    assert!(!report.holds(), "a duplicate cannot replace a missing case");
    for verdict in [
        Verdict::Escaped {
            target: "fixture".to_string(),
        },
        Verdict::NotMeasured {
            reason: "fixture".to_string(),
        },
        Verdict::NotEnforced {
            target: "fixture".to_string(),
        },
    ] {
        report = complete();
        report.cases[0].verdict = verdict;
        assert!(!report.holds());
    }
    report = complete();
    for case in &mut report.cases {
        case.verdict = Verdict::NotApplicable {
            reason: "fixture".to_string(),
        };
    }
    assert!(!report.holds(), "no case was measured");
}
