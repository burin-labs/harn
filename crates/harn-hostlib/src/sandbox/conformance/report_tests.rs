use super::*;

#[test]
fn only_a_typed_mechanism_refusal_is_retained_as_unavailable() {
    use harn_vm::process_sandbox::{SandboxMechanism, SandboxMechanismAvailability};
    let refusal = SandboxMechanismUnavailable::new(
        SandboxMechanism::MacosSandboxExec,
        SandboxMechanismAvailability::AbsentOnHost,
        SandboxProfile::Worktree,
    );
    let text = refusal.to_string();
    let child = refused_child(HostlibError::SandboxMechanismUnavailable {
        builtin: "hostlib_tools_run_command",
        refusal: Box::new(refusal.clone()),
    });
    assert_eq!(child.unavailable_mechanism, Some(refusal));
    assert!(child.spawn_refused && child.exit.is_none() && child.stdout.is_empty());
    let mut report = complete();
    report.cases[0].unavailable_mechanism = child.unavailable_mechanism;
    assert!(
        !report.holds(),
        "a typed unavailable receipt cannot qualify as confinement"
    );
    let unrelated = refused_child(HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: text,
    });
    assert!(unrelated.spawn_refused);
    assert_eq!(
        unrelated.unavailable_mechanism, None,
        "matching prose is not typed evidence"
    );
}

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
                unavailable_mechanism: None,
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
    report.cases[0].observed = None;
    assert!(
        !report.holds(),
        "a verdict without an observation cannot pass"
    );
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
