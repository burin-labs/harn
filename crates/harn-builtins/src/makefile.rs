use harn_vm::stdlib::macros::harn_builtin;
use harn_vm_core::value::args::Args;
use harn_vm_core::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(super) fn register_makefile_builtins);

/// Default Make input filenames in the order Make selects them.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "makefile_filenames() -> list<string>",
    category = "project"
)]
fn makefile_filenames_impl(_values: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::list(
        harn_makefile::FILENAMES
            .into_iter()
            .map(VmValue::string)
            .collect(),
    ))
}

/// Parse literal targets and bounded recipe previews without evaluating Make.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "makefile_parse(text: string) -> list<{name: string, command: string, recipe_preview: list<string>, recipe_line_count: int}>",
    category = "project"
)]
fn makefile_parse_impl(values: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let args = Args::new("makefile_parse", values);
    let source = args.string(0, "text")?;
    let targets = harn_makefile::parse(source);
    let json = serde_json::to_value(targets).expect("Make declarations contain only JSON values");
    Ok(harn_vm_core::schema::json_to_vm_value(&json))
}
