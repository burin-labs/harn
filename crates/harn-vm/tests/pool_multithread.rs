#![recursion_limit = "256"]

use harn_vm::value::VmError;

fn run_on_multithread(source: &str) -> Result<Vec<String>, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let output = rt.block_on(async {
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        vm.execute(&chunk)
            .await
            .map_err(|error: VmError| format!("{error:?}"))?;
        Ok::<_, String>(vm.output().to_string())
    })?;
    Ok(output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect())
}

#[test]
fn pool_workers_run_on_multithread_runtime_without_localset() {
    let lines = run_on_multithread(
        r#"
import { pool_create, pool_wait } from "std/lifecycle/pool"

pipeline main(task) {
  let pool = pool_create({name: "multithread-no-localset", max_concurrent: 4})
  let handles = []
  for i in 0 to 8 exclusive {
    let seed = i
    handles = handles.push(pool.submit({ -> seed + 10 }))
  }
  let results = pool_wait(handles)
  let completed = 0
  for result in results {
    if result.status == "completed" && result.result >= 10 {
      completed = completed + 1
    }
  }
  log(completed)
}
"#,
    )
    .unwrap();

    assert_eq!(lines, vec!["8"]);
}

#[test]
fn pool_worker_inherits_registry_for_nested_waits() {
    let lines = run_on_multithread(
        r#"
import { pool_create, pool_wait } from "std/lifecycle/pool"

pipeline main(task) {
  let pool = pool_create({name: "worker-nested-wait", max_concurrent: 2})
  let inner = pool.submit({ -> "inner-ok" })
  let outer = pool.submit({ ->
    let done = pool_wait(inner)
    return done.result
  })
  let outer_done = pool_wait(outer)
  let inner_done = pool_wait(inner)
  log(outer_done.status)
  log(outer_done.result)
  log(inner_done.result)
}
"#,
    )
    .unwrap();

    assert_eq!(lines, vec!["completed", "inner-ok", "inner-ok"]);
}

#[test]
fn host_managed_pool_scope_diagnostic_is_public() {
    let err = run_on_multithread(
        r#"
import { pool_create } from "std/lifecycle/pool"

pipeline main(task) {
  pool_create({name: "tenant-backed", scope: "tenant"})
}
"#,
    )
    .unwrap_err();

    assert!(err.contains("host-managed"), "{err}");
    assert!(!err.contains("harn-cloud"), "{err}");
    assert!(!err.contains("#306"), "{err}");
}
