use super::*;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
}

pub(crate) fn vm_str(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

pub(crate) fn vm_bool(b: bool) -> VmValue {
    VmValue::Bool(b)
}

pub(crate) fn vm_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

/// Build a small tool registry containing an `edit` tool with rich schema
/// (enum action, required path, multiple fields). Returned as a VmValue so it
/// can be passed to `parse_text_tool_calls_with_tools`.
pub(crate) fn sample_tool_registry() -> VmValue {
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

pub(crate) fn known_tools_set() -> std::collections::BTreeSet<String> {
    ["edit", "read", "run", "lookup", "scaffold"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub(crate) fn defer_loading_registry() -> VmValue {
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
