use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::require_args;

fn primitive_schema(type_name: &str) -> VmValue {
    VmValue::dict([("type", VmValue::string(type_name))])
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_any_witness() -> Schema<unknown>", category = "json")]
fn schema_any_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("any"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_string_witness() -> Schema<string>", category = "json")]
fn schema_string_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("string"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_int_witness() -> Schema<int>", category = "json")]
fn schema_int_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("int"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_float_witness() -> Schema<float>", category = "json")]
fn schema_float_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("float"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_bool_witness() -> Schema<bool>", category = "json")]
fn schema_bool_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("bool"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "__schema_nil_witness() -> Schema<nil>", category = "json")]
fn schema_nil_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(primitive_schema("nil"))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "<T> __schema_literal_witness(value: T) -> Schema<T>", category = "json")]
fn schema_literal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_literal")?;
    Ok(VmValue::dict([("const", args[0].clone())]))
}

#[harn_builtin(exposure = "stdlib_internal", effects = [], sig = "<T> __schema_refine_witness(schema: Schema<T>, constraint: dict) -> Schema<T>", category = "json")]
fn schema_refine_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_refine")?;
    Ok(VmValue::dict([(
        "all_of",
        VmValue::List(std::sync::Arc::new(vec![args[0].clone(), args[1].clone()])),
    )]))
}

const BUILTINS: &[&VmBuiltinDef] = &[
    &SCHEMA_ANY_IMPL_DEF,
    &SCHEMA_STRING_IMPL_DEF,
    &SCHEMA_INT_IMPL_DEF,
    &SCHEMA_FLOAT_IMPL_DEF,
    &SCHEMA_BOOL_IMPL_DEF,
    &SCHEMA_NIL_IMPL_DEF,
    &SCHEMA_LITERAL_IMPL_DEF,
    &SCHEMA_REFINE_IMPL_DEF,
];

pub(super) fn register_builtins(vm: &mut Vm) {
    for def in BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stdlib_internal_requires_embedded_stdlib_authority() {
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let contract = harn_builtin_registry::builtin_entry("__schema_any_witness")
            .expect("schema witness must be projected into the compiler manifest");
        assert_eq!(
            contract.contract.exposure,
            harn_builtin_meta::BuiltinExposure::StdlibInternal
        );
        let source = "let witness = __schema_any_witness()";
        let program = harn_parser::check_source_strict(source).expect("type-clean program");

        let err = crate::Compiler::new()
            .compile(&program)
            .expect_err("ordinary source must not call stdlib internals");
        assert!(
            err.message.contains("call the public stdlib function"),
            "unexpected diagnostic: {}",
            err.message
        );

        crate::Compiler::new_embedded_stdlib()
            .compile(&program)
            .expect("embedded stdlib may call its private primitive");
    }
}
