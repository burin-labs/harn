use harn_builtin_macros::harn_builtin;
use harn_vm_core::{AsyncBuiltinCtx, VmBuiltinHandler, VmError, VmValue};

#[harn_builtin(exposure = "pure", effects = [], sig = "core_registration_probe(value: int) -> int")]
fn registration_probe(args: &[VmValue], _output: &mut String) -> Result<VmValue, VmError> {
    match args {
        [VmValue::Int(value)] => Ok(VmValue::Int(value + 1)),
        _ => Err(VmError::Runtime("expected one int".into())),
    }
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "core_async_registration_probe() -> nil",
    kind = "async"
)]
async fn async_registration_probe(
    _ctx: AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

#[test]
fn downstream_builtins_link_to_core_without_a_native_module_layout() {
    let definitions = harn_vm_core::all_builtin_defs();
    let definition = definitions
        .iter()
        .find(|definition| definition.sig.name == "core_registration_probe")
        .expect("the macro must link its definition into the core registry");
    assert_eq!(
        definition.contract,
        harn_vm_core::builtin_support::BuiltinContract::PURE
    );
    let VmBuiltinHandler::Sync(handler) = definition.handler else {
        panic!("the linked definition must carry its handler");
    };
    let mut output = String::new();
    assert!(matches!(
        handler(&[VmValue::Int(41)], &mut output).unwrap(),
        VmValue::Int(42)
    ));
    assert!(handler(&[], &mut output).is_err());
    assert!(definitions.iter().any(|definition| {
        definition.sig.name == "core_async_registration_probe"
            && matches!(definition.handler, VmBuiltinHandler::Async(_))
    }));
}
