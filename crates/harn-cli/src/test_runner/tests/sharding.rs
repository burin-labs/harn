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
    let source = Arc::new(String::new());
    let program = Arc::new(Vec::new());
    let mk = |name: &str| TestCase {
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
    };
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
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    let names_two = shard_two
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names_one, vec!["test_big", "test_small_b"]);
    assert_eq!(names_two, vec!["test_mid", "test_small_a"]);
}

#[tokio::test]
async fn parallel_scheduler_persists_timings_cache() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_timed.harn",
        r"
@test
pipeline test_first(task) {}

@test
pipeline test_second(task) {}
",
    );

    let opts = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
    assert_eq!(summary.passed, 2);
    let cache = temp.path().join("suite/.harn/test-timings.json");
    assert!(cache.exists(), "expected timings cache at {cache:?}");
    let stored: BTreeMap<String, u64> =
        serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
    assert!(
        stored.keys().any(|key| key.contains("test_first")),
        "expected timings for test_first in {stored:?}"
    );
    assert!(
        stored.keys().any(|key| key.contains("test_second")),
        "expected timings for test_second in {stored:?}"
    );
}

#[tokio::test]
async fn sequential_shards_share_an_immutable_timing_snapshot() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_sharded.harn",
        r"
@test
pipeline test_alpha(task) {}

@test
pipeline test_beta(task) {}

@test
pipeline test_gamma(task) {}

@test
pipeline test_delta(task) {}
",
    );

    let suite = temp.path().join("suite");
    let cache = suite.join(".harn/test-timings.json");
    let test_file = suite.join("test_sharded.harn").canonicalize().unwrap();
    let snapshot = BTreeMap::from([
        (timings_key(&test_file, "test_alpha"), 100),
        (timings_key(&test_file, "test_beta"), 40),
        (timings_key(&test_file, "test_gamma"), 20),
        (timings_key(&test_file, "test_delta"), 10),
    ]);
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(&cache, serde_json::to_string(&snapshot).unwrap()).unwrap();

    let mut selected = Vec::new();
    // Deliberately invoke siblings out of index order: selection must depend
    // only on the shared snapshot, not on which shard happened to finish first.
    for index in [3, 1, 2] {
        let opts = RunOptions {
            shard: Some(TestShard::new(index, 3).unwrap()),
            ..RunOptions::new(5_000)
        };
        let summary = run_tests_with_options(&suite, &opts).await;
        assert_eq!(summary.failed, 0, "{:?}", summary.results);
        selected.extend(summary.results.into_iter().map(|result| result.name));
    }

    selected.sort();
    assert_eq!(
        selected,
        ["test_alpha", "test_beta", "test_delta", "test_gamma"]
    );
    assert_eq!(load_timings_cache(&cache), snapshot);
}
