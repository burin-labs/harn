//! Integration tests for `hostlib_tools_search`.

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

fn matches_in(result: &VmValue) -> &Rc<Vec<VmValue>> {
    match result {
        VmValue::Dict(d) => match d.get("matches") {
            Some(VmValue::List(rows)) => rows,
            other => panic!("expected `matches` list, got {other:?}"),
        },
        other => panic!("expected dict result, got {other:?}"),
    }
}

#[test]
fn search_finds_literal_pattern() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    fs::write(dir.path().join("b.txt"), "alphabet\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("alpha")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .expect("search ok");
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 2);
    let texts: Vec<String> = rows
        .iter()
        .map(|row| match row {
            VmValue::Dict(d) => match d.get("text") {
                Some(VmValue::String(s)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        })
        .collect();
    assert!(texts.iter().any(|t| t == "alpha"));
    assert!(texts.iter().any(|t| t == "alphabet"));
}

#[test]
fn search_respects_glob_filter() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hit.rs"), "fn target() {}\n").unwrap();
    fs::write(dir.path().join("ignored.txt"), "fn target() {}\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("target")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("glob", vm_string("*.rs")),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .unwrap();
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    if let VmValue::Dict(d) = &rows[0] {
        if let Some(VmValue::String(s)) = d.get("path") {
            assert!(s.ends_with("hit.rs"), "got {s}");
        }
    }
}

#[test]
fn search_respects_max_matches_and_marks_truncated() {
    let dir = TempDir::new().unwrap();
    let mut buf = String::new();
    for i in 0..10 {
        buf.push_str(&format!("line{i} target\n"));
    }
    fs::write(dir.path().join("many.txt"), buf).unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("target")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("max_matches", VmValue::Int(3)),
    ]))
    .unwrap();
    if let VmValue::Dict(d) = &result {
        let truncated = matches!(d.get("truncated"), Some(VmValue::Bool(true)));
        assert!(truncated, "expected truncated flag set");
    }
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 3);
}

#[test]
fn search_returns_context_lines_when_requested() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("ctx.txt"),
        "line1\nline2\nMATCH\nline4\nline5\n",
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("MATCH")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("context_before", VmValue::Int(1)),
        ("context_after", VmValue::Int(1)),
    ]))
    .unwrap();
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    if let VmValue::Dict(d) = &rows[0] {
        if let Some(VmValue::List(before)) = d.get("context_before") {
            assert_eq!(before.len(), 1);
        } else {
            panic!("missing context_before");
        }
        if let Some(VmValue::List(after)) = d.get("context_after") {
            assert_eq!(after.len(), 1);
        } else {
            panic!("missing context_after");
        }
    }
}

#[test]
fn search_case_insensitive_flag_works() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file.txt"), "HELLO world\nhello world\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();

    let exact = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("hello")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert_eq!(matches_in(&exact).len(), 1);

    let insensitive = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("hello")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
        ("case_insensitive", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert_eq!(matches_in(&insensitive).len(), 2);
}

#[test]
fn search_rejects_invalid_regex() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file.txt"), "hello\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let err = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("(unclosed")),
        ("path", vm_string(&dir.path().to_string_lossy())),
    ]))
    .expect_err("invalid regex must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("invalid regex") || msg.contains("invalid parameter"),
        "got: {msg}"
    );
}

#[test]
fn search_respects_gitignore_unless_overridden() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
    fs::write(dir.path().join("included.txt"), "needle\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("needle")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .unwrap();
    let rows = matches_in(&result);
    let paths: Vec<String> = rows
        .iter()
        .map(|row| match row {
            VmValue::Dict(d) => match d.get("path") {
                Some(VmValue::String(s)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        })
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("included.txt")));
    assert!(
        !paths.iter().any(|p| p.ends_with("ignored.txt")),
        "gitignored file should be skipped, got {paths:?}"
    );
}

#[test]
fn search_gate_blocks_when_feature_disabled() {
    permissions::reset();
    let mut reg = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut reg);
    let entry = reg.find("hostlib_tools_search").unwrap();
    let err = (entry.handler)(&dict_arg(&[("pattern", vm_string("x"))])).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("hostlib_enable"),
        "expected gate message pointing at hostlib_enable, got `{msg}`"
    );
}
