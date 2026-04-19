//! Unit tests for `crate::llm::tools`: the fenceless TypeScript tool-call
//! parser, the schema → TypeScript renderer (TypeExpr + ComponentRegistry),
//! and the argument-normalizer compatibility shims.
//!
//! Declared as `#[cfg(test)] mod tests;` in `tools/mod.rs`, so `super::`
//! names either items defined directly in `mod.rs` or parser symbols
//! that `mod.rs` re-exports (`pub(crate) use parse::…`,
//! `pub(crate) use handle_local::…`) for callers outside the tools
//! module. Either way the flat `use super::{…}` below is accurate.

pub(super) use super::{
    apply_tool_search_native_injection, build_assistant_response_message,
    build_assistant_tool_message, build_tool_calling_contract_prompt, build_tool_result_message,
    collect_tool_schemas, collect_tool_schemas_with_registry, extract_deferred_tool_names,
    normalize_tool_args, parse_bare_calls_in_body, parse_native_json_tool_calls,
    parse_text_tool_calls_with_tools, validate_tool_args, vm_tools_to_native, ComponentRegistry,
    TEXT_RESPONSE_PROTOCOL_HELP,
};
pub(super) use crate::value::VmValue;
pub(super) use serde_json::json;
use std::collections::BTreeMap;
use std::rc::Rc;

mod contract_prompt;
mod core_parser;
mod heredoc_and_messages;
mod native_tools;
mod validation_and_tagged;

pub(super) fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    VmValue::Dict(Rc::new(map))
}

pub(super) fn vm_str(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

pub(super) fn vm_bool(b: bool) -> VmValue {
    VmValue::Bool(b)
}

pub(super) fn vm_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

/// Build a small tool registry containing an `edit` tool with rich schema
/// (enum action, required path, multiple fields). Returned as a VmValue so it
/// can be passed to `parse_text_tool_calls_with_tools`.
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
        ("parameters", VmValue::Dict(Rc::new(params))),
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
        ("parameters", VmValue::Dict(Rc::new(run_params))),
    ]);

    vm_dict(&[("tools", vm_list(vec![edit_tool, run_tool]))])
}

pub(super) fn known_tools_set() -> std::collections::BTreeSet<String> {
    ["edit", "read", "run", "lookup", "scaffold"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub(super) fn defer_loading_registry() -> VmValue {
    let mut eager_params = BTreeMap::new();
    eager_params.insert("path".to_string(), vm_str("string"));
    let eager = vm_dict(&[
        ("name", vm_str("look")),
        ("description", vm_str("Read file contents")),
        ("parameters", VmValue::Dict(Rc::new(eager_params))),
    ]);

    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", VmValue::Dict(Rc::new(deferred_params))),
        ("defer_loading", vm_bool(true)),
    ]);

    vm_list(vec![eager, deferred])
}
