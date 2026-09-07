use std::sync::Arc;
use std::time::Instant;

use crate::{DispatchCore, DispatchCoreConfig, DispatchRuntime, NoReplayCache};

#[test]
#[ignore = "manual startup attribution against an external script inventory"]
fn repeated_script_startup_profile() {
    let directory = std::env::var_os("PROFILE_SCRIPT_DIR").expect("PROFILE_SCRIPT_DIR");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .expect("script inventory")
        .map(|entry| entry.expect("script entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "harn")
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 85, "exact source inventory");
    let iterations = std::env::var("PROFILE_ITERATIONS")
        .ok()
        .map(|value| value.parse::<usize>().expect("iteration count"))
        .unwrap_or(1);
    for iteration in 0..iterations {
        let base = tempfile::tempdir().expect("runtime root");
        let started = Instant::now();
        let mut executors = Vec::new();
        let mut source_modules = 0;
        let mut workers = 0;
        let mut executor_us = 0;
        for (index, path) in paths.iter().enumerate() {
            let mut config = DispatchCoreConfig::for_script(path);
            config.base_dir = base.path().join(index.to_string());
            config.replay_cache = Arc::new(NoReplayCache);
            config.trusted_host_dispatch = true;
            let core = DispatchCore::new(config).expect("prepared core");
            let receipt = core.generation_receipt();
            source_modules += receipt.source_modules;
            workers += receipt.worker_count;
            let executor_start = Instant::now();
            executors.push(DispatchRuntime::start("PROFILE", Arc::new(core)));
            executor_us += executor_start.elapsed().as_micros();
        }
        assert_eq!(source_modules, 85);
        assert_eq!(workers, 85);
        eprintln!("{{\"startup_phase\":\"inventory\",\"iteration\":{iteration},\"scripts\":{},\"source_modules\":{source_modules},\"workers\":{workers},\"executor_us\":{executor_us},\"total_us\":{}}}", paths.len(), started.elapsed().as_micros());
        drop(executors);
    }
}
