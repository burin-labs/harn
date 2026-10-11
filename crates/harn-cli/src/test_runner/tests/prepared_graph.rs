use super::*;

#[tokio::test]
async fn prepared_test_graph_retains_bytecode_beyond_the_ordinary_vm_cache() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    for module_count in [16_usize, 640] {
        let temp = TempTestDir::new();
        let cache_dir = temp.path().join("bytecode-cache");
        let _cache_dir = ScopedEnvVar::set(
            harn_vm::bytecode_cache::CACHE_DIR_ENV,
            &cache_dir.to_string_lossy(),
        );
        let _cache_enabled = ScopedEnvVar::set(harn_vm::bytecode_cache::CACHE_ENABLED_ENV, "1");
        let mut source = String::new();
        for index in 0..module_count {
            temp.write(
                &format!("suite/module_{index}.harn"),
                &format!("let calls = 0\npub fn value_{index}() {{ calls += 1; return calls }}\n"),
            );
            source.push_str(&format!(
                "import {{ value_{index} }} from \"./module_{index}\"\n"
            ));
        }
        source.push_str(
            "pipeline test_first(_task) { assert_eq(value_0(), 1); assert_eq(value_0(), 2) }\n\
             pipeline test_second(_task) { assert_eq(value_0(), 1); assert_eq(value_0(), 2) }\n",
        );
        temp.write("suite/test_graph.harn", &source);
        let session = TestRunSession::default();
        let options = RunOptions {
            parallel: true,
            jobs: Some(2),
            ..RunOptions::new(5_000)
        };

        let summary = run_tests_with_session(&temp.path().join("suite"), &options, &session).await;

        assert_eq!(summary.passed, 2, "{module_count}: {:?}", summary.results);
        assert_eq!(summary.skipped, 0);
        let stats = session.stats();
        assert_eq!(stats.evictions, 0, "{module_count}: {stats:?}");
        assert_eq!(
            stats.insertions, module_count as u64,
            "{module_count}: {stats:?}"
        );
        assert_eq!(stats.entries, module_count, "{module_count}: {stats:?}");
        assert!(stats.hits >= (module_count * 2) as u64, "{stats:?}");
        assert!(summary.results.iter().all(|result| {
            result
                .phases
                .is_some_and(|phases| phases.modules.modules_compiled == 0)
        }));
    }
}
