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
const sibling_started = harness.runtime.atomic(0)
try {
  parallel 2 { i ->
    if i == 0 {
      while harness.runtime.atomic_get(sibling_started) == 0 {
        harness.runtime.yield_now()
      }
      throw "boom"
    }
    harness.runtime.atomic_set(sibling_started, 1)
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
const sibling_started = harness.runtime.atomic(0)
try {
  parallel each ["fail", "slow"] { item ->
    if item == "fail" {
      while harness.runtime.atomic_get(sibling_started) == 0 {
        harness.runtime.yield_now()
      }
      throw "each boom"
    }
    harness.runtime.atomic_set(sibling_started, 1)
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

/// A clock whose sleep makes a cancellation observable the moment the sleep
/// begins, and then never completes.
///
/// The tests below need the cancellation to be noticed by the blocking
/// operation itself rather than by the poll between operations, because those
/// are the two observers whose disagreement is the defect. Arranging that with
/// a timer would make the test a race, and a test written to remove a timing
/// dependency must not introduce one. Setting the flag from inside the sleep
/// puts the cancellation exactly where it is needed, with no duration to tune:
/// the sleep future never resolves, so the operation's own cancel poll is the
/// only thing that can end it.
#[derive(Debug)]
struct CancelWhenSleepStarts {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    signal: Option<Arc<std::sync::Mutex<Option<String>>>>,
}

#[async_trait::async_trait]
impl harn_clock::Clock for CancelWhenSleepStarts {
    fn now_utc(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::UNIX_EPOCH
    }

    fn monotonic_ms(&self) -> i64 {
        0
    }

    async fn sleep(&self, _duration: Duration) {
        if let Some(slot) = &self.signal {
            *slot.lock().unwrap() = Some("SIGTERM".to_string());
        }
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        std::future::pending::<()>().await;
    }

    async fn sleep_until_utc(&self, _deadline: time::OffsetDateTime) {
        std::future::pending::<()>().await;
    }
}

// Which observer of a cancellation actually runs the interrupt handlers.
//
// A cancellation is noticed independently in several places. The between-ops
// poll dispatches handlers; so does the op wrapper's timed-out branch. The
// body of a blocking op does not: the harness clock's sleep runs its own 10ms
// poll and returns the cancelled error directly, reaching neither dispatch
// site. Whichever observer wins the race therefore decides whether a handler
// the program registered runs at all.
//
// This drives the shape the SIGTERM conformance case uses - a registered
// handler, then a blocking sleep cancelled part way through - and asserts the
// handler ran. It exists to pin that arrangement rather than leave it to the
// race, and to say which observer fired when it is the one that skips
// dispatch.
#[test]
fn test_handler_runs_when_a_blocking_op_observes_the_cancel_itself() {
    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal_token = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let signal_writer = std::sync::Arc::clone(&signal_token);
    let cancel_writer = std::sync::Arc::clone(&cancel_token);

    let source = r#"
pipeline t(harness: Harness) {
  const outcome = try {
    harness.clock.sleep_ms(30000)
    "slept"
  } catch (e) {
    "caught"
  }
  harness.stdio.log(outcome)
}
"#;

    let (output, _) = run_harn_with_setup(source, move |vm| {
        vm.register_builtin("term_marker", |_, out| {
            out.push_str("[harn] term\n");
            Ok(VmValue::Nil)
        });
        let term_options = VmValue::dict(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("SIGTERM"),
            )])),
        )]));
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("term_marker")),
            Some(&term_options),
        )
        .unwrap();
        vm.install_interrupt_signal_token(signal_token);
        vm.install_cancel_token(cancel_token);
        vm.set_harness(crate::Harness::with_clock(Arc::new(
            CancelWhenSleepStarts {
                cancel: cancel_writer,
                signal: Some(signal_writer),
            },
        )));
    })
    .unwrap();

    assert!(
        output.contains("caught"),
        "the blocking sleep should have been interrupted, got: {output}"
    );
    assert!(
        output.contains("term"),
        "the registered SIGTERM handler never ran, so the observer that skips \
         dispatch won the race. Output: {output}"
    );
}

#[test]
fn test_handler_runs_when_the_cancelled_op_is_not_caught() {
    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal_token = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let signal_writer = std::sync::Arc::clone(&signal_token);
    let cancel_writer = std::sync::Arc::clone(&cancel_token);
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran_in_handler = std::sync::Arc::clone(&ran);

    // The same cancellation as the caught case, with the catch removed. The
    // handler dispatch that actually runs in the caught case happens on the
    // poll AFTER the op threw, so it depends on the program continuing to
    // execute. Nothing here continues.
    let source = r#"
pipeline t(harness: Harness) {
  harness.clock.sleep_ms(30000)
  harness.stdio.log("slept")
}
"#;

    let _ = run_harn_with_setup(source, move |vm| {
        vm.register_builtin("term_marker", move |_, out| {
            ran_in_handler.store(true, std::sync::atomic::Ordering::SeqCst);
            out.push_str("[harn] term\n");
            Ok(VmValue::Nil)
        });
        let term_options = VmValue::dict(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("SIGTERM"),
            )])),
        )]));
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("term_marker")),
            Some(&term_options),
        )
        .unwrap();
        vm.install_interrupt_signal_token(signal_token);
        vm.install_cancel_token(cancel_token);
        vm.set_harness(crate::Harness::with_clock(Arc::new(
            CancelWhenSleepStarts {
                cancel: cancel_writer,
                signal: Some(signal_writer),
            },
        )));
    });

    assert!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        "a registered SIGTERM handler did not run when the cancelled operation \
         was not caught: the only observer that dispatches runs after the \
         throw, so an uncaught cancel skips the handler entirely"
    );
}

#[test]
fn test_unfiltered_handler_runs_on_a_cancel_carrying_no_signal() {
    // A host stopping a session cancels without any signal. The common
    // registration - a cleanup hook with no options at all - is written for
    // that case, so it has to run on it. It used to default to SIGINT, which
    // made the default registration silently specific to a signal that a
    // session stop never carries.
    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_writer = std::sync::Arc::clone(&cancel_token);
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran_in_handler = std::sync::Arc::clone(&ran);

    let source = r#"
pipeline t(harness: Harness) {
  harness.clock.sleep_ms(30000)
  harness.stdio.log("slept")
}
"#;

    let _ = run_harn_with_setup(source, move |vm| {
        vm.register_builtin("cleanup_marker", move |_, out| {
            ran_in_handler.store(true, std::sync::atomic::Ordering::SeqCst);
            out.push_str("[harn] cleanup\n");
            Ok(VmValue::Nil)
        });
        // No options: no filter.
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("cleanup_marker")),
            None,
        )
        .unwrap();
        // A cancel token and no signal token at all: nothing can supply a name.
        vm.install_cancel_token(cancel_token);
        vm.set_harness(crate::Harness::with_clock(Arc::new(
            CancelWhenSleepStarts {
                cancel: cancel_writer,
                signal: None,
            },
        )));
    });

    assert!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        "an unfiltered on_interrupt handler did not run on a cancellation that \
         carried no signal, which is how a host stops a session"
    );
}

#[test]
fn test_filtered_handler_is_skipped_when_no_signal_was_delivered() {
    // The other side of the same rule, so that "runs on everything" cannot be
    // the accidental reading of the fix. A handler that named a signal has
    // nothing to match against here, and must not be run on a guess.
    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_writer = std::sync::Arc::clone(&cancel_token);
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran_in_handler = std::sync::Arc::clone(&ran);

    let source = r#"
pipeline t(harness: Harness) {
  harness.clock.sleep_ms(30000)
  harness.stdio.log("slept")
}
"#;

    let _ = run_harn_with_setup(source, move |vm| {
        vm.register_builtin("int_only_marker", move |_, out| {
            ran_in_handler.store(true, std::sync::atomic::Ordering::SeqCst);
            out.push_str("[harn] int\n");
            Ok(VmValue::Nil)
        });
        let int_options = VmValue::dict(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("SIGINT"),
            )])),
        )]));
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("int_only_marker")),
            Some(&int_options),
        )
        .unwrap();
        vm.install_cancel_token(cancel_token);
        vm.set_harness(crate::Harness::with_clock(Arc::new(
            CancelWhenSleepStarts {
                cancel: cancel_writer,
                signal: None,
            },
        )));
    });

    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "a handler filtered to SIGINT ran on a cancellation that carried no \
         signal, so a name was invented for it"
    );
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

/// A cancellation first observed inside an async builtin still runs the
/// program's interrupt handlers.
///
/// The cancellation is armed from inside the builtin, after the machine's
/// last between-operations poll. The builtin is therefore the first observer
/// by construction, with no window in which the poll could dispatch instead.
/// An earlier version of this test armed the cancellation before the program
/// started, so the poll dispatched and the test passed with the production
/// dispatch deleted.
///
/// The program does not catch, so it stops at that builtin and no later poll
/// can run the handler either. Deleting the dispatch from the frame that
/// awaits the builtin makes this fail.
#[test]
fn test_handler_runs_when_an_async_builtin_observes_the_cancel() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let arm = std::sync::Arc::clone(&cancel_token);
    // Counted outside the machine, because the program stops on the
    // cancellation and its output buffer does not come back with the error.
    let handler_runs = std::sync::Arc::new(AtomicUsize::new(0));
    let continued = std::sync::Arc::new(AtomicUsize::new(0));
    let handler_counter = std::sync::Arc::clone(&handler_runs);
    let continued_counter = std::sync::Arc::clone(&continued);

    let source = r"
pipeline t(harness: Harness) {
  cancel_from_inside()
  mark_continued()
}
";

    let result = run_harn_with_setup(source, move |vm| {
        vm.register_builtin("cancel_marker", move |_, _| {
            handler_counter.fetch_add(1, Ordering::SeqCst);
            Ok(VmValue::Nil)
        });
        vm.register_builtin("mark_continued", move |_, _| {
            continued_counter.fetch_add(1, Ordering::SeqCst);
            Ok(VmValue::Nil)
        });
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(arcstr::ArcStr::from("cancel_marker")),
            None,
        )
        .unwrap();
        vm.install_cancel_token(std::sync::Arc::clone(&cancel_token));

        // Arms the cancellation only once the builtin is running, then
        // observes it itself, exactly as a blocking builtin does.
        vm.register_async_builtin("cancel_from_inside", move |_ctx, _args| {
            let arm = std::sync::Arc::clone(&arm);
            async move {
                arm.store(true, Ordering::SeqCst);
                Err(crate::cancellation::cancelled_error(
                    crate::cancellation::HandlerDispatch::NotDispatched(
                        crate::cancellation::NotDispatchedReason::NoMachineInScope,
                    ),
                ))
            }
        });
    });

    match result {
        Err(error) => assert!(
            crate::cancellation::is_cancellation(&error),
            "the program must stop on the cancellation, not another error: {error:?}"
        ),
        Ok((output, _)) => panic!("the cancellation must not be swallowed: {output}"),
    }

    assert_eq!(
        continued.load(Ordering::SeqCst),
        0,
        "the program must stop at the cancelled builtin, so no later poll can \
         dispatch on its behalf"
    );
    assert_eq!(
        handler_runs.load(Ordering::SeqCst),
        1,
        "the handler must run exactly once for a cancellation first observed \
         inside a builtin"
    );
}

/// The trap this design exists to avoid, pinned as a test rather than prose.
///
/// A child machine inherits the cancellation token but is constructed with an
/// empty handler list. So a dispatch placed inside the builtin would find
/// nothing to run and report success, which is indistinguishable from having
/// run the handlers. If a future change makes a child carry handlers, this
/// test fails and the dispatch can move closer to the observer.
#[test]
fn test_a_child_machine_carries_the_cancel_token_and_no_handlers() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut parent = Vm::new();
                register_vm_stdlib(&mut parent);
                parent.register_builtin("never_runs", |_, out| {
                    out.push_str("[harn] child dispatched\n");
                    Ok(VmValue::Nil)
                });
                parent
                    .register_interrupt_handler(
                        VmValue::BuiltinRef(arcstr::ArcStr::from("never_runs")),
                        None,
                    )
                    .unwrap();
                parent.install_cancel_token(std::sync::Arc::new(
                    std::sync::atomic::AtomicBool::new(true),
                ));

                assert!(
                    parent.has_unfiltered_interrupt_handler(),
                    "the parent is the machine the handler is registered on"
                );

                let mut child = parent.child_vm_inline();
                assert!(
                    child.is_cancel_requested(),
                    "a child observes the cancellation perfectly well"
                );
                assert!(
                    !child.has_unfiltered_interrupt_handler(),
                    "a child carries no handlers, which is why the builtin cannot dispatch"
                );

                let dispatched = child
                    .dispatch_handlers_for_observed_cancel()
                    .await
                    .expect("dispatching on a child is not an error, which is the trap");
                assert!(
                    !dispatched,
                    "dispatching on a child runs nothing and reports success"
                );
                assert!(
                    !child.output().contains("child dispatched"),
                    "nothing ran on the child"
                );
            })
            .await;
    });
}

/// A replayed cancellation must not run the handlers again.
///
/// Handlers ran when the run was live and their effects are already recorded,
/// so re-running them on replay repeats those effects. The replay fact is read
/// from its single owner at the same seam that dispatches, and the test enters
/// through that owner rather than setting a flag of its own. Removing the
/// replay check from the seam makes this test fail.
#[test]
fn test_a_replayed_cancellation_runs_no_handler() {
    let source = r#"
pipeline t(harness: Harness) {
  const ch = harness.runtime.channel("cancel-probe", 1)
  replay_probe()
  const outcome = try {
    harness.runtime.receive(ch)
    "received"
  } catch (e) {
    "caught"
  }
  harness.stdio.log(outcome)
}
"#;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.register_builtin("replay_probe", |_, out| {
                    out.push_str(&format!(
                        "[harn] probe is_replay={}\n",
                        crate::triggers::dispatcher::is_replay()
                    ));
                    Ok(VmValue::Nil)
                });
                vm.register_builtin("replay_marker", |_, out| {
                    out.push_str("[harn] handler ran\n");
                    Ok(VmValue::Nil)
                });
                vm.register_interrupt_handler(
                    VmValue::BuiltinRef(arcstr::ArcStr::from("replay_marker")),
                    None,
                )
                .unwrap();
                vm.install_cancel_token(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    true,
                )));

                crate::triggers::dispatcher::with_replay_scope(true, async {
                    // Positive control on the fixture itself: if the scope did
                    // not take, the test would be asserting nothing.
                    assert!(
                        crate::triggers::dispatcher::is_replay(),
                        "the replay scope must be visible to the code under test"
                    );
                    let _ = vm.execute(&chunk).await;
                })
                .await;
                vm.output().to_string()
            })
            .await
    });

    assert!(
        !output.contains("handler ran"),
        "a replayed cancellation must not re-run handlers: {output}"
    );
}
