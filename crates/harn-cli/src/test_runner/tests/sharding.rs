use super::*;

#[test]
fn test_shard_validation_rejects_invalid_selection() {
    assert!(TestShard::new(1, 1).is_ok());
    assert!(TestShard::new(0, 2).is_err());
    assert!(TestShard::new(1, 0).is_err());
    assert!(TestShard::new(3, 2).is_err());
}

#[test]
fn select_shard_cases_balances_by_historical_duration() {
    let mk = |name: &str| test_case(name);
    let mut timings = BTreeMap::new();
    timings.insert("tests/a.harn::test_big".to_string(), 100);
    timings.insert("tests/a.harn::test_mid".to_string(), 60);
    timings.insert("tests/a.harn::test_small_a".to_string(), 40);
    timings.insert("tests/a.harn::test_small_b".to_string(), 20);

    let cases = vec![
        mk("test_big"),
        mk("test_mid"),
        mk("test_small_a"),
        mk("test_small_b"),
    ];
    let shard_one = select_shard_cases(cases.clone(), &timings, TestShard::new(1, 2).unwrap());
    let shard_two = select_shard_cases(cases, &timings, TestShard::new(2, 2).unwrap());

    let names_one = shard_one
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    let names_two = shard_two
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names_one, vec!["test_big", "test_small_b"]);
    assert_eq!(names_two, vec!["test_mid", "test_small_a"]);
}

fn test_case(name: &str) -> TestCase {
    let source = Arc::new(String::new());
    let program = Arc::new(Vec::new());
    TestCase {
        file: PathBuf::from("tests/a.harn"),
        name: name.to_string(),
        pipeline_name: name.to_string(),
        source: Arc::clone(&source),
        program: Arc::clone(&program),
        imported_enum_candidates: Arc::new(Vec::new()),
        serial_group: None,
        weight: 1,
        args: Vec::new(),
        fixture: None,
        file_fixture_value: None,
        compiled_entry: None,
        compiled_file_fixture_entry: None,
        trusted_host_dispatch: false,
    }
}

#[test]
fn dominant_case_is_reserved_on_its_own_shard_and_reported() {
    let cases = vec![
        test_case("dominant"),
        test_case("small_a"),
        test_case("small_b"),
    ];
    let timings = BTreeMap::from([
        ("tests/a.harn::dominant".to_string(), 500),
        ("tests/a.harn::small_a".to_string(), 40),
        ("tests/a.harn::small_b".to_string(), 30),
    ]);

    let selection = select_shard_cases(cases, &timings, TestShard::new(1, 2).unwrap());

    assert_eq!(selection.cases.len(), 1);
    assert_eq!(selection.cases[0].name, "dominant");
    assert_eq!(selection.plan.dominant_case.unwrap().name, "dominant");
}

#[test]
fn unknown_cases_fall_back_to_count_first_partitioning() {
    let mut heavy_unknown = test_case("unknown_heavy");
    heavy_unknown.weight = 100;
    let cases = vec![
        test_case("known"),
        heavy_unknown,
        test_case("unknown_light"),
    ];
    let timings = BTreeMap::from([("tests/a.harn::known".to_string(), 100)]);

    let shard_one = select_shard_cases(cases.clone(), &timings, TestShard::new(1, 2).unwrap());
    let shard_two = select_shard_cases(cases, &timings, TestShard::new(2, 2).unwrap());

    assert_eq!(shard_one.cases.len(), 2);
    assert_eq!(shard_two.cases.len(), 1);
    assert_eq!(
        shard_one.plan.unknown_cases + shard_two.plan.unknown_cases,
        2
    );
}

#[test]
fn stale_baseline_case_fails_loudly() {
    let baseline = TimingBaseline {
        environment: "ci".to_string(),
        weights_ms: BTreeMap::from([
            ("tests/a.harn::present".to_string(), 10),
            ("tests/a.harn::renamed".to_string(), 20),
        ]),
        max_regression_percent: 25,
    };

    let error = stale_baseline_error(&[test_case("present")], &baseline).unwrap();

    assert!(error.error.unwrap().contains("renamed"));
}

#[test]
fn cost_regression_fails_case_even_when_absolute_budget_passes() {
    let mut result = passing_result_with_timings(150, 140);
    result.name = "test_budget".to_string();
    let baseline = TimingBaseline {
        environment: "ci".to_string(),
        weights_ms: BTreeMap::from([("tests/test_budget.harn::test_budget".to_string(), 100)]),
        max_regression_percent: 25,
    };

    let regressions = enforce_cost_regressions(std::slice::from_mut(&mut result), &baseline);

    assert!(!result.passed);
    assert_eq!(regressions[0].increase_percent, 40);
}
