use std::sync::Arc;

use harn_hostlib::{
    schemas, BuiltinRegistry, HostConditionObservation, HostConditionStatus,
    HostConditionsCapability, HostConditionsSnapshot, HostContentionQuestion, HostEnvironment,
    HostlibCapability, InjectedHostConditionsSource, HOST_CONDITIONS_SCHEMA_VERSION,
};
use harn_vm::VmValue;

fn injected_snapshot() -> HostConditionsSnapshot {
    HostConditionsSnapshot {
        schema_version: HOST_CONDITIONS_SCHEMA_VERSION,
        observed_at_ms: 123,
        environment: HostEnvironment::Virtualized,
        sample_cost_us: 7,
        questions: vec![
            HostConditionObservation::observed(HostContentionQuestion::MemoryOrIoContended, 0.25),
            HostConditionObservation::observed(HostContentionQuestion::PromisedCpu, 0.0),
            HostConditionObservation::not_observable(
                HostContentionQuestion::NominalSpeed,
                "guest cannot see host thermal state",
            ),
            HostConditionObservation::unavailable(
                HostContentionQuestion::AcceleratorShared,
                "control-plane accelerator allocation read failed",
            ),
        ],
    }
}

#[test]
fn injected_source_uses_the_same_builtin_and_distinguishes_all_states() {
    let capability = HostConditionsCapability::with_source(Arc::new(
        InjectedHostConditionsSource::new(injected_snapshot()),
    ));
    let mut registry = BuiltinRegistry::new();
    capability.register_builtins(&mut registry);
    let builtin = registry
        .find("hostlib_host_conditions_sample")
        .expect("sample builtin");
    let result = (builtin.handler)(&[VmValue::dict([("schema_version", VmValue::Int(1))])])
        .expect("injected sample");
    let schema = schemas::lookup("host_conditions", "sample", schemas::SchemaKind::Response)
        .expect("response schema");
    let schema: serde_json::Value = serde_json::from_str(schema).expect("schema JSON");
    let schema = harn_vm::json_to_vm_value(&schema);
    let schema =
        harn_vm::schema::canonicalize_json_schema(&schema).expect("canonical response schema");
    harn_vm::schema::validate_value_against_canonical_schema(&result, &schema, true)
        .expect("response matches its published schema");
    let VmValue::Dict(snapshot) = result else {
        panic!("snapshot should be a dict");
    };
    assert_eq!(
        snapshot.get("environment").map(VmValue::display),
        Some("virtualized".to_string())
    );
    let Some(VmValue::List(questions)) = snapshot.get("questions") else {
        panic!("questions should be a list");
    };
    let statuses: Vec<_> = questions
        .iter()
        .map(|question| {
            let VmValue::Dict(question) = question else {
                panic!("question should be a dict");
            };
            let contention = match question.get("contention").expect("contention") {
                VmValue::Float(value) => Some(*value),
                VmValue::Nil => None,
                other => panic!("unexpected contention value: {}", other.display()),
            };
            (
                question
                    .get("status")
                    .map(VmValue::display)
                    .expect("status"),
                contention,
            )
        })
        .collect();
    assert_eq!(statuses[0], ("observed".to_string(), Some(0.0)));
    assert_eq!(statuses[1], ("not_observable".to_string(), None));
    assert_eq!(statuses[2], ("unavailable".to_string(), None));
    assert_eq!(statuses[3], ("observed".to_string(), Some(0.25)));
}

#[test]
fn source_contract_rejects_an_observed_answer_without_a_value() {
    let mut snapshot = injected_snapshot();
    snapshot.questions[0].status = HostConditionStatus::Observed;
    snapshot.questions[0].contention = None;
    let capability = HostConditionsCapability::with_source(Arc::new(
        InjectedHostConditionsSource::new(snapshot),
    ));
    let mut registry = BuiltinRegistry::new();
    capability.register_builtins(&mut registry);
    let builtin = registry
        .find("hostlib_host_conditions_sample")
        .expect("sample builtin");
    let error = (builtin.handler)(&[VmValue::dict([("schema_version", VmValue::Int(1))])])
        .expect_err("invalid source response must fail closed");
    assert!(error
        .to_string()
        .contains("observed but has no contention value"));
}

#[test]
fn request_version_is_required_and_closed() {
    let capability = HostConditionsCapability::default();
    let mut registry = BuiltinRegistry::new();
    capability.register_builtins(&mut registry);
    let builtin = registry
        .find("hostlib_host_conditions_sample")
        .expect("sample builtin");
    assert!((builtin.handler)(&[VmValue::dict(Vec::<(&str, VmValue)>::new())]).is_err());
    assert!((builtin.handler)(&[VmValue::dict([("schema_version", VmValue::Int(2),)])]).is_err());
}
