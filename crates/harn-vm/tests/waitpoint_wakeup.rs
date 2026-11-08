use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use harn_clock::PausedClock;
use harn_vm::event_log::{
    install_active_event_log, install_memory_for_current_thread, AnyEventLog, EventLog,
    SqliteEventLog, Topic,
};
use harn_vm::harness::Harness;

fn vm() -> (harn_vm::Vm, Arc<PausedClock>) {
    let (harness, clock) = Harness::test();
    let mut vm = harn_vm::new_vm();
    harn_runtime::register_vm_stdlib(&mut vm);
    vm.set_harness(harness);
    (vm, clock)
}

struct ResetLog;

impl Drop for ResetLog {
    fn drop(&mut self) {
        harn_vm::reset_thread_local_state();
    }
}

fn sqlite_log(path: &std::path::Path) -> Arc<AnyEventLog> {
    Arc::new(AnyEventLog::Sqlite(
        SqliteEventLog::open(path.to_path_buf(), 32).unwrap(),
    ))
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn completion_wakes_a_pending_fan_out_without_advancing_time() {
    harn_vm::reset_thread_local_state();
    let log = install_memory_for_current_thread(32);
    let (mut waiter, clock) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  const points = [
    harness.runtime.waitpoint_create("fan-0"),
    harness.runtime.waitpoint_create("fan-1"),
    harness.runtime.waitpoint_create("fan-2"),
    harness.runtime.waitpoint_create("fan-3"),
    harness.runtime.waitpoint_create("fan-4"),
  ]
  harness.runtime.waitpoint_complete("fan-0", 0)
  const results = harness.runtime.waitpoint_wait(points)
  for result in results {
    harness.stdio.println(result.value)
  }
}
"#,
    )
    .expect("waiter compiles");
    {
        let registered_at = tokio::time::Instant::now();
        let mut execution = Box::pin(waiter.execute(&chunk));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        for index in 1..5 {
            let topic = Topic::new(format!("waitpoint.state.fan-{index}")).unwrap();
            let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
            assert_eq!(events.last().unwrap().1.payload["status"], "open");
        }
        let (mut producer, _) = vm();
        let completion = harn_vm::compile_source(
            r#"
pipeline test(harness: Harness) {
  harness.runtime.waitpoint_complete("fan-4", 4)
  harness.runtime.waitpoint_complete("fan-3", 3)
  harness.runtime.waitpoint_complete("fan-2", 2)
  harness.runtime.waitpoint_complete("fan-1", 1)
}
"#,
        )
        .unwrap();
        producer.execute(&completion).await.unwrap();
        harn_clock::test_support::within("completed waitpoint fan-out", execution)
            .await
            .unwrap();
        assert_eq!(
            tokio::time::Instant::now(),
            registered_at,
            "completed waitpoints must wake their pending caller without a timer tick"
        );
    }
    assert_eq!(waiter.output().trim(), "0\n1\n2\n3\n4");
    assert_eq!(harn_clock::Clock::monotonic_ms(clock.as_ref()), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancellation_wakes_a_pending_wait_without_advancing_time() {
    harn_vm::reset_thread_local_state();
    let log = install_memory_for_current_thread(32);
    let (mut waiter, clock) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  const point = harness.runtime.waitpoint_create("cancel-pending")
  try {
    harness.runtime.waitpoint_wait(point)
  } catch (error) {
    harness.stdio.println(error.name)
  }
}
"#,
    )
    .unwrap();
    {
        let registered_at = tokio::time::Instant::now();
        let mut execution = Box::pin(waiter.execute(&chunk));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        let (mut producer, _) = vm();
        let cancellation = harn_vm::compile_source(
            r#"
pipeline test(harness: Harness) {
  harness.runtime.waitpoint_cancel("cancel-pending")
}
"#,
        )
        .unwrap();
        producer.execute(&cancellation).await.unwrap();
        let events = log
            .read_range(
                &Topic::new("waitpoint.state.cancel-pending").unwrap(),
                None,
                usize::MAX,
            )
            .await
            .unwrap();
        assert_eq!(events.last().unwrap().1.payload["status"], "cancelled");
        harn_clock::test_support::within("cancelled waitpoint", execution)
            .await
            .unwrap();
        assert_eq!(
            tokio::time::Instant::now(),
            registered_at,
            "cancellation must wake its pending caller without a timer tick"
        );
    }
    assert_eq!(waiter.output().trim(), "WaitpointCancelledError");
    assert_eq!(harn_clock::Clock::monotonic_ms(clock.as_ref()), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_uses_the_waiting_harness_clock() {
    harn_vm::reset_thread_local_state();
    install_memory_for_current_thread(32);
    let (mut waiter, clock) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  const point = harness.runtime.waitpoint_create("timeout-pending")
  try {
    harness.runtime.waitpoint_wait(point, {timeout: 50ms})
  } catch (error) {
    harness.stdio.println(error.name)
  }
}
"#,
    )
    .unwrap();
    {
        let mut execution = Box::pin(waiter.execute(&chunk));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        clock.advance(Duration::from_millis(49));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        clock.advance(Duration::from_millis(1));
        assert!(
            matches!(futures::poll!(execution.as_mut()), Poll::Ready(Ok(_))),
            "the timeout must fire exactly when the owning clock reaches its deadline"
        );
    }
    assert_eq!(waiter.output().trim(), "WaitpointTimeoutError");
    assert_eq!(harn_clock::Clock::monotonic_ms(clock.as_ref()), 50);
}

async fn complete_from_another_log(reader: &Arc<AnyEventLog>, writer: Arc<AnyEventLog>) {
    install_active_event_log(writer);
    let (mut producer, _) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  harness.runtime.waitpoint_complete("foreign-write", 9)
}
"#,
    )
    .unwrap();
    producer.execute(&chunk).await.unwrap();
    let events = reader
        .read_range(
            &Topic::new("waitpoint.state.foreign-write").unwrap(),
            None,
            usize::MAX,
        )
        .await
        .unwrap();
    assert_eq!(events.last().unwrap().1.payload["status"], "completed");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn durable_wait_observes_a_write_through_another_log_instance() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().unwrap();
    let _reset = ResetLog;
    let reader = sqlite_log(&dir.path().join("events.sqlite"));
    let writer = sqlite_log(&dir.path().join("events.sqlite"));
    install_active_event_log(reader.clone());
    let (mut waiter, clock) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  const point = harness.runtime.waitpoint_create("foreign-write")
  harness.stdio.println(harness.runtime.waitpoint_wait(point).value)
}
"#,
    )
    .unwrap();
    {
        let mut execution = Box::pin(waiter.execute(&chunk));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        complete_from_another_log(&reader, writer).await;
        // Both clocks are virtual. Advancing Tokio also lets the old polling
        // implementation supply a positive control for this durable path.
        clock.advance(Duration::from_millis(100));
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(
            matches!(futures::poll!(execution.as_mut()), Poll::Ready(Ok(_))),
            "a writer with an independent broadcast map must still be observed"
        );
    }
    assert_eq!(waiter.output().trim(), "9");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_rechecks_durable_state_before_reporting_expiry() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().unwrap();
    let _reset = ResetLog;
    let reader = sqlite_log(&dir.path().join("events.sqlite"));
    let writer = sqlite_log(&dir.path().join("events.sqlite"));
    install_active_event_log(reader.clone());
    let (mut waiter, clock) = vm();
    let chunk = harn_vm::compile_source(
        r#"
pipeline test(harness: Harness) {
  const point = harness.runtime.waitpoint_create("foreign-write")
  harness.stdio.println(harness.runtime.waitpoint_wait(point, {timeout: 50ms}).value)
}
"#,
    )
    .unwrap();
    {
        let mut execution = Box::pin(waiter.execute(&chunk));
        assert!(futures::poll!(execution.as_mut()).is_pending());
        clock.advance(Duration::from_millis(12));
        complete_from_another_log(&reader, writer).await;
        clock.advance(Duration::from_millis(38));
        tokio::time::advance(Duration::from_millis(50)).await;
        assert!(
            matches!(futures::poll!(execution.as_mut()), Poll::Ready(Ok(_))),
            "a committed completion must win over expiry before the next storage probe"
        );
    }
    assert_eq!(waiter.output().trim(), "9");
}
