use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::register_vm_stdlib;
use crate::{VmError, VmValue};

use super::Vm;

fn execute_with_limits(source: &str, limits: RuntimeLimits) -> (Vm, Result<VmValue, VmError>) {
    let chunk = crate::compile_source(source).expect("source compiles");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.runtime_limits = limits;
                let result = vm.execute(&chunk).await;
                (vm, result)
            })
            .await
    })
}

fn limits_with_max_frames(max_vm_frames: usize) -> RuntimeLimits {
    RuntimeLimits {
        max_vm_frames,
        ..RuntimeLimits::default()
    }
}

fn assert_stack_overflow(error: &VmError) {
    let message = error.to_string();
    assert!(
        message.contains("Stack overflow: too many nested calls"),
        "expected stack overflow, got {message}"
    );
}

#[test]
fn non_tail_recursion_fails_with_bounded_vm_stack_trace() {
    let limits = limits_with_max_frames(16);
    let (vm, result) = execute_with_limits(
        r#"
pipeline main() {
  fn dive(n: int) -> int {
    if n <= 0 {
      return 0
    }
    return 1 + dive(n - 1)
  }
  dive(128)
}
"#,
        limits,
    );

    let error = result.expect_err("non-tail recursion must hit VM frame limit");
    assert_stack_overflow(&error);
    assert!(
        vm.error_stack_trace.len() <= limits.max_vm_frames,
        "stack trace grew past VM frame limit: {:?}",
        vm.error_stack_trace
    );
    assert!(
        vm.error_stack_trace
            .iter()
            .any(|(name, _, _, _)| name == "dive"),
        "stack trace should identify the recursive function: {:?}",
        vm.error_stack_trace
    );
}

#[test]
fn tail_recursive_countdown_runs_beyond_frame_limit() {
    let (vm, result) = execute_with_limits(
        r#"
pipeline main() {
  fn countdown(n: int, acc: int) -> int {
    if n <= 0 {
      return acc
    }
    return countdown(n - 1, acc + 1)
  }
  log(countdown(200, 0))
}
"#,
        limits_with_max_frames(8),
    );

    result.expect("tail recursion should reuse the current frame");
    assert_eq!(vm.output().trim(), "[harn] 200");
}

#[test]
fn tracked_step_tail_recursion_uses_explicit_frames() {
    let (vm, result) = execute_with_limits(
        r#"
@step(name: "tracked_countdown")
fn tracked_countdown(n: int) -> int {
  if n <= 0 {
    return 0
  }
  return tracked_countdown(n - 1)
}

pipeline main() {
  tracked_countdown(64)
}
"#,
        limits_with_max_frames(12),
    );

    let error = result.expect_err("tracked functions must preserve lifecycle frame boundaries");
    assert_stack_overflow(&error);
    assert!(
        vm.error_stack_trace
            .iter()
            .any(|(name, _, _, _)| name == "tracked_countdown"),
        "stack trace should identify the tracked recursive function: {:?}",
        vm.error_stack_trace
    );
}

#[test]
fn callback_methods_do_not_accumulate_frames_per_item() {
    let (vm, result) = execute_with_limits(
        r#"
pipeline main() {
  let xs = range(0, 256).to_list()
  let mapped = xs.map({ x -> x + 1 })
  let filtered = mapped.filter({ x -> x % 64 == 0 })
  let dict = {a: 1, b: 2, c: 3, d: 4}
    .map_values({ v -> v + 10 })
    .filter({ v -> v > 12 })
  let set_out = set(xs)
    .map({ x -> x % 11 })
    .filter({ x -> x < 3 })

  log(len(mapped))
  log(filtered[0])
  log(dict.count())
  log(set_out.count())
}
"#,
        limits_with_max_frames(8),
    );

    result.expect("callback-heavy collection dispatch should stay within the frame budget");
    assert_eq!(
        vm.output().trim(),
        "[harn] 256\n[harn] 64\n[harn] 2\n[harn] 3"
    );
}

#[test]
fn recursive_callback_body_uses_same_frame_budget() {
    let (_vm, result) = execute_with_limits(
        r#"
pipeline main() {
  fn recurse(n: int) -> int {
    if n <= 0 {
      return 0
    }
    return 1 + recurse(n - 1)
  }
  range(0, 3).to_list().map({ x -> recurse(64) })
}
"#,
        limits_with_max_frames(12),
    );

    let error = result.expect_err("recursive callback bodies should hit the VM frame budget");
    assert_stack_overflow(&error);
}
