//! Integration tests for `hostlib_tools_{read_file,write_file,delete_file,list_directory}`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use harn_hostlib::tools::permissions;
use harn_hostlib::{tools::ToolsCapability, BuiltinRegistry, HostlibCapability, HostlibError};
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

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn read_file_returns_utf8_payload_with_size() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("hello.txt");
    fs::write(&file, "hello world").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(&file)))])).unwrap();

    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_ref() == "utf-8"));
    assert!(
        matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_ref() == "hello world")
    );
    assert!(matches!(dict_get(&result, "size"), VmValue::Int(11)));
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(false)
    ));
}

#[test]
fn read_file_offset_and_limit_apply() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("payload.txt");
    fs::write(&file, "0123456789").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("offset", VmValue::Int(2)),
        ("limit_bytes", VmValue::Int(4)),
    ]))
    .unwrap();

    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_ref() == "2345"));
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(true)
    ));
}

#[test]
fn read_file_binary_returns_base64() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("bin.dat");
    fs::write(&file, [0u8, 1, 2, 3, 0xff]).unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("encoding", vm_string("binary")),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_ref() == "base64"));
}

#[test]
fn read_file_invalid_utf8_falls_back_to_base64() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notutf8.bin");
    fs::write(&file, [0xff, 0xfe, 0xfd]).unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(&file)))])).unwrap();
    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_ref() == "base64"));
}

#[test]
fn read_file_errors_when_missing() {
    let dir = TempDir::new().unwrap();
    let reg = registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("nope"))),
    )]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));
}

#[test]
fn write_file_creates_file_and_parent_directories() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("nested/deep/note.txt");

    let reg = registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&nested))),
        ("content", vm_string("welcome")),
    ]))
    .unwrap();

    assert!(matches!(dict_get(&result, "created"), VmValue::Bool(true)));
    assert!(matches!(
        dict_get(&result, "bytes_written"),
        VmValue::Int(7)
    ));
    assert_eq!(fs::read_to_string(&nested).unwrap(), "welcome");
}

#[test]
fn write_file_respects_overwrite_false() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("locked.txt");
    fs::write(&file, "existing").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("new")),
        ("overwrite", VmValue::Bool(false)),
    ]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));
    assert_eq!(fs::read_to_string(&file).unwrap(), "existing");
}

#[test]
fn write_file_base64_decodes_payload() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("binary.bin");

    let reg = registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("aGVsbG8=")), // "hello"
        ("encoding", vm_string("base64")),
    ]))
    .unwrap();
    assert!(matches!(
        dict_get(&result, "bytes_written"),
        VmValue::Int(5)
    ));
    assert_eq!(fs::read(&file).unwrap(), b"hello");
}

#[test]
fn delete_file_removes_existing_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("doomed.txt");
    fs::write(&file, "x").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_delete_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(&file)))])).unwrap();
    assert!(matches!(dict_get(&result, "removed"), VmValue::Bool(true)));
    assert!(!file.exists());
}

#[test]
fn delete_file_missing_returns_removed_false() {
    let dir = TempDir::new().unwrap();
    let reg = registry();
    let entry = reg.find("hostlib_tools_delete_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("absent"))),
    )]))
    .unwrap();
    assert!(matches!(dict_get(&result, "removed"), VmValue::Bool(false)));
}

#[test]
fn delete_file_directory_requires_recursive() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("inner.txt"), "x").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_delete_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("a"))),
    )]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));

    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&dir.path().join("a")))),
        ("recursive", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "removed"), VmValue::Bool(true)));
}

#[test]
fn list_directory_returns_entries_sorted() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    fs::create_dir(dir.path().join("c_dir")).unwrap();
    fs::write(dir.path().join(".hidden"), "h").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(dir.path())))])).unwrap();
    let entries = match dict_get(&result, "entries") {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    };
    let names: Vec<String> = entries
        .iter()
        .map(|v| match dict_get(v, "name") {
            VmValue::String(s) => s.to_string(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(names, vec!["a.txt", "b.txt", "c_dir"]);
}

#[test]
fn list_directory_include_hidden_works() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("visible"), "v").unwrap();
    fs::write(dir.path().join(".hidden"), "h").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(dir.path()))),
        ("include_hidden", VmValue::Bool(true)),
    ]))
    .unwrap();
    if let VmValue::List(rows) = dict_get(&result, "entries") {
        assert_eq!(rows.len(), 2);
    } else {
        panic!("expected entries list");
    }
}

#[test]
fn list_directory_respects_max_entries_and_marks_truncated() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
    }

    let reg = registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(dir.path()))),
        ("max_entries", VmValue::Int(2)),
    ]))
    .unwrap();
    if let VmValue::List(rows) = dict_get(&result, "entries") {
        assert_eq!(rows.len(), 2);
    }
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(true)
    ));
}
