//! Smoke test for #567: drive every deterministic tool from a real
//! `.harn` script through the full lexer + parser + compiler + VM
//! pipeline.
//!
//! The standalone integration tests under `tests/tools_*.rs` exercise
//! handlers directly. This file proves the contract end-to-end: that a
//! Harn script can call `hostlib_enable("tools:deterministic")` and then
//! invoke each of the seven deterministic builtins through the normal
//! script surface.

use std::fs;

use harn_hostlib::tools::permissions;
use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_vm::{register_vm_stdlib, Compiler, Vm, VmValue};
use tempfile::TempDir;

fn run_harn(source: &str) -> (VmValue, String) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().expect("tokenize");
                let mut parser = Parser::new(tokens);
                let program = parser.parse().expect("parse");
                let chunk = Compiler::new().compile(&program).expect("compile");

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let _ = harn_hostlib::install_default(&mut vm);
                let result = vm.execute(&chunk).await.expect("execute");
                (result, vm.output().to_string())
            })
            .await
    })
}

#[test]
fn end_to_end_deterministic_tools_via_harn_script() {
    permissions::reset();

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("readable.txt"), "hello\nworld\n").unwrap();
    fs::write(root.join("a.rs"), "pub fn main() {}\n").unwrap();
    fs::write(root.join("b.rs"), "pub fn helper() {}\n").unwrap();
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("c.txt"), "another\n").unwrap();

    let root_str = root.to_string_lossy().replace('\\', "/");
    let new_file = format!("{}/created.txt", root_str);

    // The script:
    // 1. Enables the deterministic tools surface.
    // 2. Reads, lists, searches, outlines, writes, and deletes files.
    // 3. Stores intermediate results in a dict and returns it.
    let source = format!(
        r#"
let _enable = hostlib_enable("tools:deterministic")

let listed = hostlib_tools_list_directory({{ path: "{root}" }})
let read = hostlib_tools_read_file({{ path: "{root}/readable.txt" }})
let searched = hostlib_tools_search({{
    pattern: "fn",
    path: "{root}",
    glob: "*.rs",
    fixed_strings: true
}})
let outlined = hostlib_tools_get_file_outline({{ path: "{root}/a.rs" }})
let wrote = hostlib_tools_write_file({{ path: "{new_path}", content: "ok" }})
let deleted = hostlib_tools_delete_file({{ path: "{new_path}" }})

return {{
    enable: _enable,
    listed_count: len(listed.entries),
    read_size: read.size,
    read_content: read.content,
    matches: len(searched.matches),
    outline_first: outlined.items[0].name,
    bytes_written: wrote.bytes_written,
    removed: deleted.removed,
}}
"#,
        root = root_str,
        new_path = new_file,
    );

    let (result, _stdout) = run_harn(&source);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));

    assert!(matches!(get("listed_count"), VmValue::Int(4)));
    assert!(matches!(get("read_size"), VmValue::Int(12)));
    assert!(matches!(get("read_content"), VmValue::String(s) if s.as_ref() == "hello\nworld\n"));

    if let VmValue::Int(n) = get("matches") {
        assert!(*n >= 2, "expected at least 2 fn matches, got {n}");
    } else {
        panic!("expected Int matches");
    }

    assert!(matches!(get("outline_first"), VmValue::String(s) if s.as_ref() == "main"));
    assert!(matches!(get("bytes_written"), VmValue::Int(2)));
    assert!(matches!(get("removed"), VmValue::Bool(true)));

    // The new file must be gone now.
    assert!(!std::path::Path::new(&new_file).exists());
}

#[test]
fn end_to_end_gate_blocks_without_enable() {
    permissions::reset();

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_string_lossy().replace('\\', "/");

    // Note: no `hostlib_enable` call. The Harn script must catch the
    // structured `Thrown` dict and return it.
    let source = format!(
        r#"
try {{
    return hostlib_tools_list_directory({{ path: "{root}" }})
}} catch err {{
    return err
}}
"#,
        root = root,
    );

    let (result, _) = run_harn(&source);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected gate error dict, got {other:?}"),
    };
    let kind = dict.get("kind").expect("kind present");
    let message = dict.get("message").expect("message present");
    assert!(matches!(kind, VmValue::String(s) if s.as_ref() == "backend_error"));
    if let VmValue::String(msg) = message {
        assert!(
            msg.contains("hostlib_enable"),
            "gate error must mention hostlib_enable, got `{msg}`"
        );
    } else {
        panic!("expected message string");
    }
}

#[test]
fn end_to_end_git_via_harn_script() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: git not installed");
        return;
    }
    permissions::reset();

    let dir = TempDir::new().unwrap();
    let repo = dir.path();
    let run_git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["config", "user.email", "tester@example.com"]);
    run_git(&["config", "user.name", "Tester"]);
    run_git(&["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("hello.txt"), "hi\n").unwrap();
    run_git(&["add", "hello.txt"]);
    run_git(&["commit", "-q", "-m", "first"]);

    let repo_str = repo.to_string_lossy().replace('\\', "/");

    let source = format!(
        r#"
let _ = hostlib_enable("tools:deterministic")
let log = hostlib_tools_git({{ operation: "log", repo: "{repo}" }})
let branch = hostlib_tools_git({{ operation: "current_branch", repo: "{repo}" }})
return {{
    log_count: len(log.data),
    log_subject: log.data[0].subject,
    branch: branch.data,
}}
"#,
        repo = repo_str,
    );

    let (result, _) = run_harn(&source);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    assert!(matches!(dict.get("log_count").unwrap(), VmValue::Int(1)));
    assert!(matches!(
        dict.get("log_subject").unwrap(),
        VmValue::String(s) if s.as_ref() == "first"
    ));
    assert!(matches!(
        dict.get("branch").unwrap(),
        VmValue::String(s) if s.as_ref() == "main"
    ));
}
