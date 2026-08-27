use harn_vm::{
    a2a::A2aClientError, compile_source, external_agent::ExternalAgentError, register_vm_stdlib,
    reset_thread_local_state, Vm, VmError,
};

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
fn a2a_client_errors_keep_their_external_agent_kind() {
    let cases = [
        (
            A2aClientError::Discovery("invalid card".into()),
            ExternalAgentError::Discovery("invalid card".into()),
        ),
        (
            A2aClientError::Denied("request denied".into()),
            ExternalAgentError::Denied("request denied".into()),
        ),
        (
            A2aClientError::Timeout("request timed out".into()),
            ExternalAgentError::Timeout("request timed out".into()),
        ),
        (
            A2aClientError::Cancelled("request cancelled".into()),
            ExternalAgentError::Cancelled("request cancelled".into()),
        ),
    ];

    for (source, expected) in cases {
        let actual = ExternalAgentError::from(source);
        assert_eq!(
            std::mem::discriminant(&actual),
            std::mem::discriminant(&expected)
        );
        assert_eq!(actual.to_string(), expected.to_string());
    }
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

#[test]
fn external_agent_delegate_structures_argument_validation_errors() {
    let output = run_source(
        r#"
pipeline main(harness: Harness, task: unknown) {
  try {
    harness.agent.external_agent_delegate("   ", {})
  } catch (error) {
    assert_eq(type_of(error), "dict", "external agent validation error value")
    assert_eq(error.error, "external_agent_error", "external agent validation error family")
    assert_eq(error.kind, "invalid_request", "external agent validation error kind")
    assert_eq(error.category, "invalid_request", "external agent validation error category")
    assert_eq(error.message, "__external_agent_delegate: task is required", "external agent validation error message")
    harness.stdio.log("typed external agent validation error")
    return
  }
  throw "external agent argument validation unexpectedly succeeded"
}
"#,
    )
    .expect("Harn source should catch the external agent validation failure");

    assert!(output.contains("[harn] typed external agent validation error"));
}
