//! Integration tests for `hostlib_tools_get_file_outline`.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;

use harn_hostlib::tools::permissions;
use harn_hostlib::{tools::ToolsCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use tempfile::TempDir;

fn registry() -> BuiltinRegistry {
    permissions::reset();
    permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::Dict(Rc::new(map))]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
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
}
