use std::time::Duration;

use crate::VmError;

use super::tests_runtime::run_harn_result_async;

#[tokio::test(flavor = "current_thread")]
async fn parallel_settle_propagates_explicit_process_exit() {
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(run_harn_result_async(
            r"pipeline default(task) {
  const result = parallel settle [1] { _ ->
    exit(7)
  }
  return result
}",
        ))
        .await;

    assert!(matches!(result, Err(VmError::ProcessExit(7))));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn detached_spawn_propagates_explicit_process_exit_to_parent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(run_harn_result_async(
                r"pipeline default(task) {
  const child = spawn {
    exit(11)
  }
  sleep(1s)
  return 0
}",
            ));
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(2)).await;

            let result = handle.await.expect("join VM task");
            assert!(matches!(result, Err(VmError::ProcessExit(11))));
        })
        .await;
}
