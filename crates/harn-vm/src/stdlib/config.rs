use crate::stdlib::macros::harn_builtin;
use crate::value::{DictMap, VmDictExt, VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_config_builtins);

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "parse_env_file(text: string) -> Result<dict<string, string>, {line: int, message: string}>",
    category = "config",
    doc = "Parse literal KEY=VALUE lines. Comments, export prefixes, and matching outer quotes are supported. No shell execution, variable expansion, or escape decoding occurs. Duplicate keys use the last value. Errors report a line and reason without input text."
)]
fn parse_env_file_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = match args.first() {
        Some(VmValue::String(text)) => text.as_str(),
        _ => {
            return Err(VmError::Thrown(VmValue::string(
                "parse_env_file requires a string",
            )))
        }
    };
    Ok(match crate::config::env_file::parse_env_file(text) {
        Ok(values) => VmValue::enum_variant(
            "Result",
            "Ok",
            vec![VmValue::dict(
                values
                    .into_iter()
                    .map(|(key, value)| (key, VmValue::string(value))),
            )],
        ),
        Err(error) => {
            let mut value = DictMap::new();
            value.insert("line".into(), VmValue::Int(error.line as i64));
            value.put_str("message", error.message);
            VmValue::enum_variant("Result", "Err", vec![VmValue::dict(value)])
        }
    })
}
