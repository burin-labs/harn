use super::*;

struct InvalidContractVmConfigurator {
    calls: Arc<AtomicUsize>,
}

impl VmConfigurator for InvalidContractVmConfigurator {
    fn configure(&self, vm: &mut Vm) -> Result<(), DispatchError> {
        let calls = self.calls.clone();
        vm.register_builtin("test_invalid_contract_value", move |_args, _output| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(VmValue::String("not-an-integer".into()))
        });
        Ok(())
    }
}

#[tokio::test]
async fn dispatch_validates_input_before_handler_and_output_before_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn inspect(count: int) -> int {
  return test_invalid_contract_value()
}
",
    )
    .expect("write script");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = DispatchCoreConfig::for_script(&script);
    config.vm_configurator = Arc::new(InvalidContractVmConfigurator {
        calls: calls.clone(),
    });
    let core = DispatchCore::new(config).expect("core");

    let mut invalid_input = replay_test_request(None);
    invalid_input.function = "inspect".to_string();
    invalid_input.arguments = CallArguments::Named(BTreeMap::from([(
        "count".to_string(),
        serde_json::json!("not-an-integer"),
    )]));
    let input_error = core
        .dispatch(invalid_input)
        .await
        .expect_err("invalid input must fail");
    assert!(matches!(input_error, DispatchError::Validation(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "handler must not run");

    let mut invalid_output = replay_test_request(None);
    invalid_output.function = "inspect".to_string();
    invalid_output.arguments = CallArguments::Named(BTreeMap::from([(
        "count".to_string(),
        serde_json::json!(1),
    )]));
    let output_error = core
        .dispatch(invalid_output)
        .await
        .expect_err("invalid output must fail");
    assert!(matches!(output_error, DispatchError::Contract(_)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "valid input reaches handler"
    );
}

#[tokio::test]
async fn module_initialization_throw_is_value_free_at_the_dispatch_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
fn initialize() {
  throw {message: "PRIVATE-CUSTOMER-DIAGNOSTIC-123456"}
}

let initialized = initialize()

pub fn inspect() -> int {
  return if initialized == nil { 1 } else { 2 }
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let mut request = replay_test_request(None);
    request.function = "inspect".to_string();

    let error = core
        .dispatch(request)
        .await
        .expect_err("module initialization must fail");
    assert_eq!(error.message(), "tool threw an undeclared value");
    assert!(!error.message().contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
}

#[tokio::test]
async fn dispatch_projects_variadic_parameters_as_arrays_for_every_argument_form() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn collect(prefix: string, ...values: int) -> dict {
  return {prefix: prefix, values: values}
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    assert!(
        core.catalog().function("collect").unwrap().params[1].rest,
        "export discovery preserves the variadic marker"
    );
    let mut vm = core
        .generation
        .instantiate(Arc::new(AtomicBool::new(false)));
    let exports = vm
        .load_prepared_module_exports_from_source(&script, core.generation.source())
        .await
        .expect("load compiled exports");
    assert!(
        exports["collect"].func.has_rest_param,
        "compiled export preserves the variadic marker"
    );
    assert_eq!(
        core.tool_catalog().tools[0].input_schema["properties"]["values"],
        serde_json::json!({"type": "array", "items": {"type": "integer"}})
    );

    let mut named = replay_test_request(None);
    named.function = "collect".to_string();
    named.arguments = CallArguments::Named(BTreeMap::from([
        ("prefix".to_string(), serde_json::json!("named")),
        ("values".to_string(), serde_json::json!([1, 2])),
    ]));
    assert_eq!(
        core.dispatch(named).await.expect("named dispatch").value,
        serde_json::json!({"prefix": "named", "values": [1, 2]})
    );

    let mut positional = replay_test_request(None);
    positional.function = "collect".to_string();
    positional.arguments = CallArguments::Positional(vec![
        serde_json::json!("positional"),
        serde_json::json!(3),
        serde_json::json!(4),
    ]);
    assert_eq!(
        core.dispatch(positional)
            .await
            .expect("positional dispatch")
            .value,
        serde_json::json!({"prefix": "positional", "values": [3, 4]})
    );
}
