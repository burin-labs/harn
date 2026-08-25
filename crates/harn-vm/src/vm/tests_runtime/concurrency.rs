//! Structured concurrency: starting work, and stopping it.
//!
//! `parallel` and `parallel each` blocks with their fail-fast, settle, and
//! stream-break semantics (including that a failing branch cancels its slow
//! siblings and that the lowest-index error wins), spawn/await/cancel, the LIFO
//! signal-handler stack and interrupt handlers, and deadlines — which must
//! interrupt an async sleep and kill a blocking subprocess.

use crate::compiler::Compiler;
use crate::stdlib::register_vm_stdlib;
use crate::VmValue;
use harn_lexer::Lexer;
use harn_parser::Parser;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use super::harness::*;
use crate::vm::*;
#[test]
fn test_parallel_basic() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const results = parallel(3) { i -> i * 10 }\nharness.stdio.log(results) }",
    );
    assert_eq!(out, "[harn] [0, 10, 20]");
}

#[test]
fn test_parallel_no_variable() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const results = parallel(3) { 42 }\nharness.stdio.log(results) }",
    );
    assert_eq!(out, "[harn] [42, 42, 42]");
}

#[test]
fn test_parallel_each_basic() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const results = parallel each [1, 2, 3] { x -> x * x }\nharness.stdio.log(results) }",
    );
    assert_eq!(out, "[harn] [1, 4, 9]");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_fail_fast_cancels_slow_sibling() {
    // A branch error aborts in-flight siblings: the slow branch is cancelled
    // mid-sleep and never reaches its atomic_set, even though the pipeline
    // keeps running well past the sibling's would-be completion time.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r#"pipeline t(harness: Harness, task: unknown) {
const survived = harness.runtime.atomic(0)
try {
  parallel 2 { i ->
    if i == 0 {
      throw "boom"
    }
    harness.clock.sleep_ms(5000)
    harness.runtime.atomic_set(survived, 1)
    i
  }
} catch (e) {
  harness.stdio.log("caught: " + e)
}
harness.clock.sleep_ms(20000)
harness.stdio.log(harness.runtime.atomic_get(survived))
}"#,
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(30)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] caught: boom\n[harn] 0");
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_each_fail_fast_cancels_slow_sibling() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r#"pipeline t(harness: Harness, task: unknown) {
const survived = harness.runtime.atomic(0)
try {
  parallel each ["fail", "slow"] { item ->
    if item == "fail" {
      throw "each boom"
    }
    harness.clock.sleep_ms(5000)
    harness.runtime.atomic_set(survived, 1)
    item
  }
} catch (e) {
  harness.stdio.log("caught: " + e)
}
harness.clock.sleep_ms(20000)
harness.stdio.log(harness.runtime.atomic_get(survived))
}"#,
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(30)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] caught: each boom\n[harn] 0");
        })
        .await;
}

#[test]
fn test_parallel_fail_fast_skips_unstarted_branches() {
    // With max_concurrent: 1, the first branch's error means the queued
    // branches are never started at all — fully deterministic, no timing.
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const started = harness.runtime.atomic(0)
try {
  parallel each [1, 2, 3] with { max_concurrent: 1 } { n ->
    if n == 1 {
      throw "stop"
    }
    harness.runtime.atomic_add(started, 1)
    n
  }
} catch (e) {
  harness.stdio.log(e)
}
harness.stdio.log(harness.runtime.atomic_get(started))
}"#,
    );
    assert_eq!(out, "[harn] stop\n[harn] 0");
}

#[test]
fn test_parallel_fail_fast_reports_lowest_index_error() {
    // Both branches throw on their first poll, so both errors have settled
    // by the time the abort lands; the reported error must deterministically
    // be the lowest-source-index one (the `scope { }` convention), not
    // whichever happened to join first.
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
try {
  parallel each ["first", "second"] { word ->
    throw word
  }
} catch (e) {
  harness.stdio.log(e)
}
}"#,
    );
    assert_eq!(out, "[harn] first");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_settle_still_runs_all_branches() {
    // `parallel settle` keeps the draining semantics: a failing branch does
    // not cancel siblings, so both slow branches still complete.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r#"pipeline t(harness: Harness, task: unknown) {
const completed = harness.runtime.atomic(0)
const outcome = parallel settle [1, 2, 3] { item ->
  if item == 1 {
    throw "early failure"
  }
  harness.clock.sleep_ms(5000)
  harness.runtime.atomic_add(completed, 1)
  item * 10
}
harness.stdio.log(outcome.succeeded)
harness.stdio.log(outcome.failed)
harness.stdio.log(harness.runtime.atomic_get(completed))
}"#,
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(30)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] 2\n[harn] 1\n[harn] 2");
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_each_stream_break_cancels_remaining_work() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r"pipeline t(harness: Harness, task: unknown) {
const completed = harness.runtime.atomic(0)
const results = parallel each [1, 2, 3] with { max_concurrent: 1 } { item ->
  harness.clock.sleep_ms(1000)
  harness.runtime.atomic_add(completed, 1)
  return item
} as stream
for item in results {
  break
}
harness.clock.sleep_ms(3000)
harness.stdio.log(harness.runtime.atomic_get(completed))
}",
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(4)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] 1");
        })
        .await;
}

#[test]
fn test_spawn_await() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const handle = spawn { harness.stdio.log("spawned") }
const result = await(handle)
harness.stdio.log("done")
}"#,
    );
    assert_eq!(out, "[harn] spawned\n[harn] done");
}

#[test]
fn test_spawn_cancel() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const handle = spawn { harness.stdio.log("should be cancelled") }
cancel(handle)
harness.stdio.log("cancelled")
}"#,
    );
    assert_eq!(out, "[harn] cancelled");
}

#[test]
fn test_cancel_graceful_propagates_to_cpu_bound_spawn() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const handle = spawn {
  let i = 0
  while true {
    i = i + 1
  }
}
const result = cancel_graceful(handle, 100ms)
harness.stdio.log(is_err(result))
harness.stdio.log(contains(unwrap_err(result), "cancelled"))
}"#,
    );
    assert_eq!(out, "[harn] true\n[harn] true");
}

#[test]
fn test_std_signal_handlers_are_lifo_and_removable() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t(harness: Harness) {
  const first = on_interrupt({ -> harness.stdio.log("a") }, {once: false})
  const second = on_interrupt({ -> harness.stdio.log("b") }, {once: false})
  __signal_raise("SIGINT")
  off_interrupt(second)
  __signal_raise("SIGINT")
  harness.stdio.log(interrupted())
  off_interrupt(first.handle)
}
"#,
    );
    assert_eq!(out, "[harn] b\n[harn] a\n[harn] a\n[harn] true");
}

#[test]
fn test_with_interrupt_unregisters_after_throw() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t(harness: Harness) {
  try {
    with_interrupt({ -> harness.stdio.log("leaked") }, { -> throw "boom" }, {once: false})
  } catch (e) {
  }
  const raised = try {
    __signal_raise("SIGINT")
    "not interrupted"
  } catch (e) {
    "interrupted"
  }
  harness.stdio.log(raised)
}
"#,
    );
    assert_eq!(out, "[harn] interrupted");
}

#[test]
fn test_interrupt_handler_graceful_timeout_is_enforced() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t(harness: Harness) {
  on_interrupt({ ->
    let spin = 0
    while true { spin = spin + 1 }
  }, {graceful_timeout_ms: 0})
  const result = try {
    __signal_raise("SIGINT")
    "missed timeout"
  } catch (e) {
    e
  }
  harness.stdio.log(result)
}
"#,
    );
    assert_eq!(out, "[harn] kind:interrupted:handler_timeout");
}

#[test]
fn test_host_signal_token_dispatches_matching_signal() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut vm = Vm::new();
        vm.register_builtin("term_marker", |_, out| {
            out.push_str("[harn] term\n");
            Ok(VmValue::Nil)
        });
        vm.register_builtin("int_marker", |_, out| {
            out.push_str("[harn] int\n");
            Ok(VmValue::Nil)
        });
        let term_options = VmValue::dict(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("SIGTERM"),
            )])),
        )]));
        let int_options = VmValue::dict(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("SIGINT"),
            )])),
        )]));
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("term_marker")),
            Some(&term_options),
        )
        .unwrap();
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("int_marker")),
            Some(&int_options),
        )
        .unwrap();

        let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let signal_token = std::sync::Arc::new(std::sync::Mutex::new(Some("SIGTERM".to_string())));
        vm.install_interrupt_signal_token(signal_token);
        vm.install_cancel_token(cancel_token);

        assert!(vm.pending_scope_interrupt().await.is_none());
        assert_eq!(vm.output().trim_end(), "[harn] term");
    });
}

#[test]
fn test_spawn_returns_value() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const h = spawn { 42 }\nconst r = await(h)\nharness.stdio.log(r) }",
    );
    assert_eq!(out, "[harn] 42");
}

// --- Deadline tests ---

#[test]
fn test_deadline_success() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const result = deadline 5s { harness.stdio.log("within deadline")
42 }
harness.stdio.log(result)
}"#,
    );
    assert_eq!(out, "[harn] within deadline\n[harn] 42");
}

#[test]
fn test_deadline_exceeded() {
    let result = run_harn_result(
        r"pipeline t(harness: Harness, task: unknown) {
deadline 1ms {
  let i = 0
  while i < 1000000 { i = i + 1 }
}
}",
    );
    assert!(result.is_err());
}

#[test]
fn test_deadline_caught_by_try() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
try {
  deadline 1ms {
    let i = 0
    while i < 1000000 { i = i + 1 }
  }
} catch(e) {
  harness.stdio.log("caught")
}
}"#,
    );
    assert_eq!(out, "[harn] caught");
}

#[cfg(unix)]
#[test]
fn test_deadline_kills_blocking_exec_subprocess() {
    // Regression for the subprocess-lifecycle gap: `exec` is a *sync*
    // builtin, so the deadline `tokio::select!` cannot preempt it while it
    // blocks on the child. The cooperative `op_interrupt` context must kill
    // the child (group) at the deadline instead of letting the 30s sleep
    // run to completion and orphaning it.
    let started = std::time::Instant::now();
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) {
deadline 500ms {
  harness.process.exec("sh", "-c", "sleep 30")
}
}"#,
    );
    assert!(result.is_err(), "deadline must fire: {result:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "deadline must preempt the blocking 30s exec, took {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_deadline_interrupts_async_sleep_without_wall_clock() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r#"pipeline t(harness: Harness, task: unknown) {
try {
  deadline 50ms {
    harness.clock.sleep_ms(1000)
    harness.stdio.log("missed deadline")
  }
} catch(e) {
  harness.stdio.log("caught")
}
}"#,
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(50)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] caught");
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_cancel_during_await_aborts_spawned_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let source = r"pipeline t(harness: Harness, task: unknown) {
const handle = spawn {
  harness.clock.sleep_ms(1000)
  mark()
}
await(handle)
}";
            let mut lexer = Lexer::new(source);
            let tokens = lexer.tokenize().unwrap();
            let mut parser = Parser::new(tokens);
            let program = parser.parse().unwrap();
            let chunk = Compiler::new().compile(&program).unwrap();

            let marker = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let marker_for_builtin = marker.clone();
            let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let vm_cancel_token = cancel_token.clone();
            let handle = tokio::task::spawn_local(async move {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.register_builtin("mark", move |_, _| {
                    marker_for_builtin.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(VmValue::Nil)
                });
                vm.install_cancel_token(vm_cancel_token);
                let result = vm.execute(&chunk).await;
                (vm.output().to_string(), result)
            });

            tokio::task::yield_now().await;
            cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
            tokio::time::advance(Duration::from_millis(300)).await;
            let (output, result) = handle.await.expect("join VM task");
            assert!(output.is_empty());
            let error = result.expect_err("parent await should be cancelled");
            assert!(error.to_string().contains("kind:cancelled"));

            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert!(
                !marker.load(std::sync::atomic::Ordering::SeqCst),
                "spawned task should be aborted when parent await is cancelled"
            );
        })
        .await;
}
