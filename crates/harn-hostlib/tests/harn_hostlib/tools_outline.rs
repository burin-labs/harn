//! Integration tests for `hostlib_tools_get_file_outline`.

use std::fs;

use harn_hostlib::{tools::ToolsCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use tempfile::TempDir;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn outline_items(value: &VmValue) -> Vec<(String, String)> {
    match value {
        VmValue::Dict(d) => match d.get("items") {
            Some(VmValue::List(items)) => items
                .iter()
                .filter_map(|item| {
                    let dict = match item {
                        VmValue::Dict(d) => d,
                        _ => return None,
                    };
                    let name = match dict.get("name") {
                        Some(VmValue::String(s)) => s.to_string(),
                        _ => return None,
                    };
                    let kind = match dict.get("kind") {
                        Some(VmValue::String(s)) => s.to_string(),
                        _ => return None,
                    };
                    Some((kind, name))
                })
                .collect(),
            _ => panic!("missing items"),
        },
        _ => panic!("expected dict"),
    }
}

fn language(value: &VmValue) -> String {
    match value {
        VmValue::Dict(d) => match d.get("language") {
            Some(VmValue::String(s)) => s.to_string(),
            _ => panic!("missing language"),
        },
        _ => panic!(),
    }
}

#[test]
fn outline_picks_up_rust_items() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("sample.rs");
    fs::write(
        &file,
        "pub struct Foo;\nfn bar() {}\nimpl Foo { pub fn baz(&self) {} }\nenum E { A, B }\n",
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_get_file_outline").unwrap();
    let result =
        (entry.handler)(&dict_arg(&[("path", vm_string(&file.to_string_lossy()))])).unwrap();

    assert_eq!(language(&result), "rust");
    let items = outline_items(&result);
    let names: Vec<&str> = items.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"Foo"));
    assert!(names.contains(&"bar"));
    assert!(names.contains(&"E"));
}

#[test]
fn outline_supports_python() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("sample.py");
    fs::write(
        &file,
        "def hello():\n    pass\n\nclass Greeter:\n    def greet(self):\n        pass\n",
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_get_file_outline").unwrap();
    let result =
        (entry.handler)(&dict_arg(&[("path", vm_string(&file.to_string_lossy()))])).unwrap();

    assert_eq!(language(&result), "python");
    let items = outline_items(&result);
    let names: Vec<&str> = items.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"hello"));
    assert!(names.contains(&"Greeter"));
}

#[test]
fn outline_empty_for_unknown_extension() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes.weirdtext");
    fs::write(&file, "this is just prose, no structure").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_get_file_outline").unwrap();
    let result =
        (entry.handler)(&dict_arg(&[("path", vm_string(&file.to_string_lossy()))])).unwrap();
    let items = outline_items(&result);
    assert!(items.is_empty(), "got: {items:?}");
    assert_eq!(text(&result, "status").as_deref(), Some("unsupported"));
    assert_eq!(text(&result, "reason").as_deref(), Some("unknown_language"));
    assert_eq!(text(&result, "language"), None);
}

fn field(value: &VmValue, key: &str) -> VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).cloned().unwrap_or(VmValue::Nil),
        _ => panic!("expected dict"),
    }
}

fn text(value: &VmValue, key: &str) -> Option<String> {
    match field(value, key) {
        VmValue::String(s) => Some(s.to_string()),
        VmValue::Nil => None,
        other => panic!("{key} is {other:?}"),
    }
}

fn int(value: &VmValue, key: &str) -> i64 {
    match field(value, key) {
        VmValue::Int(n) => n,
        other => panic!("{key} is {other:?}"),
    }
}

#[test]
fn outline_nests_swift_members_with_end_rows() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("Widget.swift");
    fs::write(
        &file,
        "struct Widget {\n    let id: Int\n    func render() -> String {\n        return \"\"\n    }\n}\n",
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_get_file_outline").unwrap();
    let result =
        (entry.handler)(&dict_arg(&[("path", vm_string(&file.to_string_lossy()))])).unwrap();

    assert_eq!(language(&result), "swift");
    assert_eq!(text(&result, "status").as_deref(), Some("ok"));
    let VmValue::List(items) = field(&result, "items") else {
        panic!("items");
    };
    let widget = &items[0];
    assert_eq!(text(widget, "name").as_deref(), Some("Widget"));
    assert_eq!(int(widget, "end_row"), 5);
    let VmValue::List(children) = field(widget, "children") else {
        panic!("children");
    };
    let render = children
        .iter()
        .find(|c| text(c, "name").as_deref() == Some("render"))
        .expect("render nested under Widget");
    assert_eq!(int(render, "start_row"), 2);
    assert_eq!(int(render, "end_row"), 4);
}

#[test]
fn outline_reports_data_formats_as_unsupported() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.json");
    fs::write(&file, "{\"a\": 1}").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_get_file_outline").unwrap();
    let result =
        (entry.handler)(&dict_arg(&[("path", vm_string(&file.to_string_lossy()))])).unwrap();
    assert_eq!(language(&result), "json");
    assert_eq!(text(&result, "status").as_deref(), Some("unsupported"));
    assert_eq!(
        text(&result, "reason").as_deref(),
        Some("no_symbol_projection")
    );
}
