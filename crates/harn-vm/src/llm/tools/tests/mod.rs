//! Unit tests for provider-native tool schema construction.
//!
//! Declared as `#[cfg(test)] mod tests;` in `tools/mod.rs`, so `super::`
pub(super) use super::{
    apply_tool_search_native_injection_typed, build_assistant_response_message,
    build_assistant_tool_message, collect_tool_schemas, extract_deferred_tool_names,
    normalize_tool_args, validate_tool_args, vm_tools_to_native,
};
pub(super) use crate::value::VmValue;
pub(super) use serde_json::json;
use std::collections::BTreeMap;

mod compact_tools;
mod native_tools;
mod non_dialect;

pub(super) fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    VmValue::dict(map)
}

pub(super) fn vm_str(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

pub(super) fn vm_bool(b: bool) -> VmValue {
    VmValue::Bool(b)
}

pub(super) fn vm_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

/// Build a small tool registry containing an `edit` tool with a detailed
/// schema (enum action, required path, multiple fields).
pub(super) fn sample_tool_registry() -> VmValue {
    // parameters dict
    let mut params = BTreeMap::new();
    params.insert(
        "action".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            (
                "enum",
                vm_list(vec![
                    vm_str("create"),
                    vm_str("patch"),
                    vm_str("replace_body"),
                ]),
            ),
            ("description", vm_str("Kind of edit.")),
        ]),
    );
    params.insert(
        "path".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("description", vm_str("Repo-relative path.")),
            (
                "examples",
                vm_list(vec![vm_str("internal/manifest/parser.go")]),
            ),
        ]),
    );
    params.insert(
        "content".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("File contents for create.")),
        ]),
    );
    params.insert(
        "new_body".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Replacement body for replace_body.")),
        ]),
    );
    params.insert(
        "function_name".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Existing function name.")),
        ]),
    );
    params.insert(
        "import_statement".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Import line for add_import.")),
        ]),
    );
    params.insert(
        "ops".to_string(),
        vm_dict(&[
            ("type", vm_str("list")),
            ("required", vm_bool(false)),
            ("description", vm_str("Atomic same-file batch edit ops.")),
        ]),
    );

    let edit_tool = vm_dict(&[
        ("name", vm_str("edit")),
        ("description", vm_str("Precise code edit.")),
        ("parameters", VmValue::dict(params)),
    ]);

    // run tool
    let mut run_params = BTreeMap::new();
    run_params.insert(
        "command".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("description", vm_str("Shell command to execute.")),
        ]),
    );
    let run_tool = vm_dict(&[
        ("name", vm_str("run")),
        ("description", vm_str("Run a shell command.")),
        ("parameters", VmValue::dict(run_params)),
    ]);

    vm_dict(&[("tools", vm_list(vec![edit_tool, run_tool]))])
}

pub(super) fn defer_loading_registry() -> VmValue {
    let mut eager_params = BTreeMap::new();
    eager_params.insert("path".to_string(), vm_str("string"));
    let eager = vm_dict(&[
        ("name", vm_str("look")),
        ("description", vm_str("Read file contents")),
        ("parameters", VmValue::dict(eager_params)),
    ]);

    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", VmValue::dict(deferred_params)),
        ("defer_loading", vm_bool(true)),
        ("namespace", vm_str("ops")),
    ]);

    vm_list(vec![eager, deferred])
}
