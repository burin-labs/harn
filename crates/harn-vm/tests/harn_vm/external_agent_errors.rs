use harn_vm::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm, VmError};

fn run_source(source: &str) -> Result<String, String> {
    reset_thread_local_state();
    let chunk = compile_source(source)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|error: VmError| format!("{error:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

#[test]
fn external_agent_delegate_exposes_typed_invalid_request_to_harn() {
    let output = run_source(
        r#"
pipeline main(harness: Harness, task: unknown) {
  try {
    harness.agent.external_agent_delegate("review this change", {
      target: "",
      idempotency_key: "typed-error-test",
      budget: {max_tokens: 1},
    })
  } catch (error) {
    assert_eq(type_of(error), "dict", "external agent error value")
    assert_eq(error.error, "external_agent_error", "external agent error family")
    assert_eq(error.kind, "invalid_request", "external agent error kind")
    assert_eq(error.category, "invalid_request", "external agent error category")
    assert_eq(error.message, "external_agent_delegate: target is required", "external agent error message")
    harness.stdio.log("typed external agent error")
    return
  }
  throw "external agent delegation unexpectedly succeeded"
}
"#,
    )
    .expect("Harn source should catch the external agent failure");

    assert!(output.contains("[harn] typed external agent error"));
}
