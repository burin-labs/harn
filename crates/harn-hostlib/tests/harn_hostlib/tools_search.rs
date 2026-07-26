//! Integration tests for `hostlib_tools_search`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

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
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn matches_in(result: &VmValue) -> &Arc<Vec<VmValue>> {
    match result {
        VmValue::Dict(d) => match d.get("matches") {
            Some(VmValue::List(rows)) => rows,
            other => panic!("expected `matches` list, got {other:?}"),
        },
        other => panic!("expected dict result, got {other:?}"),
    }
}

fn bool_field(result: &VmValue, key: &str) -> bool {
    match result {
        VmValue::Dict(d) => match d.get(key) {
            Some(VmValue::Bool(value)) => *value,
            other => panic!("expected `{key}` bool, got {other:?}"),
        },
        other => panic!("expected dict result, got {other:?}"),
    }
}

fn string_field<'a>(result: &'a VmValue, key: &str) -> &'a str {
    match result {
        VmValue::Dict(d) => match d.get(key) {
            Some(VmValue::String(value)) => value,
            other => panic!("expected `{key}` string, got {other:?}"),
        },
        other => panic!("expected dict result, got {other:?}"),
    }
}

fn list_string_field<'a>(result: &'a VmValue, key: &str) -> Vec<&'a str> {
    match result {
        VmValue::Dict(d) => match d.get(key) {
            Some(VmValue::List(values)) => values
                .iter()
                .map(|value| match value {
                    VmValue::String(value) => value.as_str(),
                    other => panic!("expected string in `{key}`, got {other:?}"),
                })
                .collect(),
            other => panic!("expected `{key}` list, got {other:?}"),
        },
        other => panic!("expected dict result, got {other:?}"),
    }
}

fn assert_path_ends_with(path: &str, components: &[&str]) {
    let actual: Vec<String> = Path::new(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<String> = components
        .iter()
        .map(|component| component.to_string())
        .collect();
    assert!(
        actual.ends_with(&expected),
        "got path components {actual:?}, expected suffix {expected:?}"
    );
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
            assert_path_ends_with(s, &["hit.rs"]);
        }
    }
}

#[test]
fn search_glob_filter_matches_file_names_at_any_depth() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/hit.rs"), "fn target() {}\n").unwrap();
    fs::write(dir.path().join("src/ignored.txt"), "fn target() {}\n").unwrap();

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
            assert_path_ends_with(s, &["src", "hit.rs"]);
        }
    }
}

#[test]
fn search_star_glob_matches_files_at_any_depth() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("root.txt"), "fn target() {}\n").unwrap();
    fs::write(dir.path().join("src/nested.txt"), "fn target() {}\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("target")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("glob", vm_string("*")),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert_eq!(matches_in(&result).len(), 2);
}

#[test]
fn search_respects_exclude_globs() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("logs")).unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("logs/llm_transcript.jsonl"),
        "target from transcript\n",
    )
    .unwrap();
    fs::write(dir.path().join("src/release.txt"), "target from source\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("target")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
        (
            "exclude_globs",
            VmValue::List(Arc::new(vec![vm_string("logs/**")])),
        ),
    ]))
    .unwrap();
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    if let VmValue::Dict(d) = &rows[0] {
        if let Some(VmValue::String(s)) = d.get("path") {
            assert_path_ends_with(s, &["src", "release.txt"]);
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
fn search_clips_long_match_lines_around_hit_and_marks_truncated() {
    let dir = TempDir::new().unwrap();
    let content = format!("{}NEEDLE{}\n", "a".repeat(200), "b".repeat(200));
    fs::write(dir.path().join("long.txt"), content).unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("NEEDLE")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
        ("max_line_bytes", VmValue::Int(96)),
    ]))
    .unwrap();

    assert!(bool_field(&result, "truncated"));
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    let text = string_field(&rows[0], "text");
    assert!(
        text.len() <= 96,
        "expected clipped line within budget, got {} bytes: {text:?}",
        text.len()
    );
    assert!(
        text.contains("NEEDLE"),
        "clipped line must include the hit: {text:?}"
    );
    assert!(
        text.starts_with("[truncated] ... "),
        "expected clipped prefix marker: {text:?}"
    );
    assert!(
        text.ends_with(" ... [truncated]"),
        "expected clipped suffix marker: {text:?}"
    );
}

#[test]
fn search_clips_context_lines_and_marks_truncated() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("ctx.txt"),
        format!("{}\nMATCH\n{}\n", "before".repeat(80), "after".repeat(80)),
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("MATCH")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("context_before", VmValue::Int(1)),
        ("context_after", VmValue::Int(1)),
        ("max_line_bytes", VmValue::Int(96)),
    ]))
    .unwrap();

    assert!(bool_field(&result, "truncated"));
    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    let before = list_string_field(&rows[0], "context_before");
    let after = list_string_field(&rows[0], "context_after");
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    assert!(before[0].len() <= 96, "before context over budget");
    assert!(after[0].len() <= 96, "after context over budget");
    assert!(before[0].ends_with(" ... [truncated]"));
    assert!(after[0].ends_with(" ... [truncated]"));
}

#[test]
fn search_clips_long_lines_on_utf8_boundaries() {
    let dir = TempDir::new().unwrap();
    let content = format!("{}NEEDLE{}\n", "λ".repeat(200), "界".repeat(200));
    fs::write(dir.path().join("utf8.txt"), content).unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("NEEDLE")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
        ("max_line_bytes", VmValue::Int(97)),
    ]))
    .unwrap();

    let rows = matches_in(&result);
    assert_eq!(rows.len(), 1);
    let text = string_field(&rows[0], "text");
    assert!(text.len() <= 97, "UTF-8 clipped line over budget");
    assert!(text.contains("NEEDLE"));
    assert!(text.is_char_boundary(text.len()));
}

#[test]
fn search_rejects_invalid_max_line_bytes() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file.txt"), "hello\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let err = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("hello")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("max_line_bytes", VmValue::Int(8)),
    ]))
    .expect_err("too-small max_line_bytes must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("max_line_bytes"),
        "expected max_line_bytes error, got: {msg}"
    );
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
    // Project ignore files only apply inside a project. Without a `.git`
    // anchor the search degrades to Harn's built-in directory defaults, which
    // is what `search_outside_a_project_ignores_stray_project_files` pins.
    fs::create_dir_all(dir.path().join(".git")).unwrap();
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
fn search_glob_filter_does_not_reinclude_gitignored_paths() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
    fs::create_dir_all(dir.path().join("crates/burin-tui/src")).unwrap();
    fs::create_dir_all(dir.path().join("crates/burin-tui/target/debug")).unwrap();
    fs::write(
        dir.path().join("crates/burin-tui/src/lib.rs"),
        "needle from source\n",
    )
    .unwrap();
    fs::write(
        dir.path()
            .join("crates/burin-tui/target/debug/generated.rs"),
        "needle from build output\n",
    )
    .unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("needle")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("glob", vm_string("crates/burin-tui/**")),
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
    assert!(paths
        .iter()
        .any(|p| p.ends_with("crates/burin-tui/src/lib.rs")));
    // Product invariant: the agent-facing `path` is forward-slash-normalized
    // on every platform. `to_string_lossy()` would leak backslashes on
    // Windows (this regressed in #3914); the search handler now routes through
    // `to_agent_path`. Assert no native separators survive so the guarantee is
    // enforced on the Windows CI lane, not just Unix.
    assert!(
        paths.iter().all(|p| !p.contains('\\')),
        "search match paths must use `/` separators on all platforms, got {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|p| p.ends_with("crates/burin-tui/target/debug/generated.rs")),
        "gitignored build output should be skipped, got {paths:?}"
    );
    assert_eq!(rows.len(), 1);
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

/// A `.gitignore` sitting outside any checkout is not a project rule, so it
/// does not filter. The built-in directory defaults still do, which is what
/// keeps an unmanaged directory from dragging `node_modules` into a search.
#[test]
fn search_outside_a_project_ignores_stray_project_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".gitignore"), "stray.txt\n").unwrap();
    fs::write(dir.path().join("stray.txt"), "needle\n").unwrap();
    fs::create_dir_all(dir.path().join("node_modules")).unwrap();
    fs::write(dir.path().join("node_modules/dep.txt"), "needle\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("needle")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
    ]))
    .unwrap();
    let paths = match_paths(&result);
    assert!(
        paths.iter().any(|p| p.ends_with("stray.txt")),
        "a stray .gitignore must not filter, got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("node_modules")),
        "built-in defaults still apply, got {paths:?}"
    );
}

/// `ignore_policy: "none"` is the one spelling for a raw walk.
#[test]
fn search_ignore_policy_none_walks_everything() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("needle")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("fixed_strings", VmValue::Bool(true)),
        ("ignore_policy", vm_string("none")),
    ]))
    .unwrap();
    let paths = match_paths(&result);
    assert!(
        paths.iter().any(|p| p.ends_with("ignored.txt")),
        "{paths:?}"
    );
}

#[test]
fn search_rejects_an_unknown_ignore_policy() {
    let dir = TempDir::new().unwrap();
    let reg = registry();
    let entry = reg.find("hostlib_tools_search").unwrap();
    let error = (entry.handler)(&dict_arg(&[
        ("pattern", vm_string("needle")),
        ("path", vm_string(&dir.path().to_string_lossy())),
        ("ignore_policy", vm_string("gitignore")),
    ]))
    .expect_err("unknown level must be rejected");
    let rendered = error.to_string();
    assert!(rendered.contains("ignore_policy"), "{rendered}");
    assert!(rendered.contains("none, builtin, project"), "{rendered}");
}

fn match_paths(result: &VmValue) -> Vec<String> {
    matches_in(result)
        .iter()
        .map(|row| match row {
            VmValue::Dict(dict) => match dict.get("path") {
                Some(VmValue::String(text)) => text.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        })
        .collect()
}
