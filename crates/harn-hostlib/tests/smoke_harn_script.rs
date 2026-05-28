#![recursion_limit = "256"]

//! Smoke test for driving every deterministic tool from a real `.harn`
//! script through the full lexer + parser + compiler + VM pipeline.
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
    let new_file = format!("{root_str}/created.txt");

    // The script:
    // 1. Enables the deterministic tools surface.
    // 2. Reads, lists, searches, outlines, writes, and deletes files.
    // 3. Stores intermediate results in a dict and returns it.
    let source = format!(
        r#"
let _enable = hostlib_enable("tools:deterministic")

let listed = hostlib_tools_list_directory({{ path: "{root_str}" }})
let read = hostlib_tools_read_file({{ path: "{root_str}/readable.txt" }})
let searched = hostlib_tools_search({{
    pattern: "fn",
    path: "{root_str}",
    glob: "*.rs",
    fixed_strings: true
}})
let outlined = hostlib_tools_get_file_outline({{ path: "{root_str}/a.rs" }})
let wrote = hostlib_tools_write_file({{ path: "{new_file}", content: "ok" }})
let deleted = hostlib_tools_delete_file({{ path: "{new_file}" }})

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
        let mut cmd = std::process::Command::new("git");
        for (key, _) in std::env::vars() {
            if key.starts_with("GIT_") {
                cmd.env_remove(&key);
            }
        }
        let output = cmd.arg("-C").arg(repo).args(args).output().unwrap();
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
let log = hostlib_tools_git({{ operation: "log", repo: "{repo_str}" }})
let branch = hostlib_tools_git({{ operation: "current_branch", repo: "{repo_str}" }})
return {{
    log_count: len(log.data),
    log_subject: log.data[0].subject,
    branch: branch.data,
}}
"#,
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

#[test]
fn end_to_end_fs_snapshot_and_auto_restore_via_harn_script() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let session = format!("smoke-snapshot-session-{}-{}", std::process::id(), n);
    let scope = format!("smoke-tc-{n}");
    permissions::reset();
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session.clone());
    let _tool_guard = harn_vm::agent_sessions::enter_current_tool_call(scope.clone());

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let original = "original-bytes";
    fs::write(root.join("target.txt"), original).unwrap();

    let root_str = root.to_string_lossy().replace('\\', "/");
    let target = format!("{root_str}/target.txt");

    // The script opens an auto-on-write snapshot keyed by the tool-call id,
    // performs a destructive `hostlib_tools_write_file`, then asks
    // `hostlib_fs_restore` to roll the workspace back to the pre-image.
    let source = format!(
        r#"
let _enable = hostlib_enable("tools:deterministic")
let snap = hostlib_fs_snapshot({{
    session_id: "{session}",
    scope_id: "{scope}",
    root: "{root_str}",
}})
let _wrote = hostlib_tools_write_file({{ path: "{target}", content: "clobbered" }})
let before_restore = hostlib_tools_read_file({{ path: "{target}" }})
let restored = hostlib_fs_restore({{
    session_id: "{session}",
    snapshot_id: "{scope}",
}})
let after_restore = hostlib_tools_read_file({{ path: "{target}" }})
let listed = hostlib_fs_list_snapshots({{ session_id: "{session}" }})
let _dropped = hostlib_fs_drop_snapshot({{
    session_id: "{session}",
    snapshot_id: "{scope}",
}})
let listed_after = hostlib_fs_list_snapshots({{ session_id: "{session}" }})

return {{
    snapshot_id: snap.snapshot_id,
    before_restore: before_restore.content,
    restored_count: len(restored.restored_paths),
    after_restore: after_restore.content,
    listed_count: len(listed.snapshots),
    listed_after_count: len(listed_after.snapshots),
}}
"#,
    );

    let (result, _) = run_harn(&source);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));

    assert!(matches!(get("snapshot_id"), VmValue::String(s) if s.as_ref() == scope));
    assert!(matches!(get("before_restore"), VmValue::String(s) if s.as_ref() == "clobbered"));
    assert!(matches!(get("restored_count"), VmValue::Int(1)));
    assert!(matches!(get("after_restore"), VmValue::String(s) if s.as_ref() == original));
    assert!(matches!(get("listed_count"), VmValue::Int(1)));
    assert!(matches!(get("listed_after_count"), VmValue::Int(0)));
}

#[test]
fn end_to_end_apply_node_via_harn_script() {
    permissions::reset();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let source_path = root.join("hello.rs");
    fs::write(
        &source_path,
        "fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n",
    )
    .unwrap();
    let path_str = source_path.to_string_lossy().replace('\\', "/");

    let path = &path_str;
    let script = format!(
        r#"
let _enable = hostlib_enable("tools:deterministic")
let preview = hostlib_ast_apply_node({{
    path: "{path}",
    query: "(function_item name: (identifier) @name (#eq? @name \"greet\") body: (block) @target)",
    replacement: "{{ format!(\"hi {{name}}!\") }}",
    dry_run: true,
}})
let applied = hostlib_ast_apply_node({{
    path: "{path}",
    query: "(function_item body: (block) @target)",
    replacement: "{{ format!(\"hi {{name}}!\") }}",
    select: "first",
}})
return {{
    preview_result: preview.result,
    preview_dry: preview.dry_run,
    preview_contains_bang: contains(preview.preview, "hi {{name}}!"),
    applied_result: applied.result,
    match_count: applied.match_count,
}}
"#,
    );

    let (result, _) = run_harn(&script);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));
    assert!(matches!(get("preview_result"), VmValue::String(s) if s.as_ref() == "applied"));
    assert!(matches!(get("preview_dry"), VmValue::Bool(true)));
    assert!(matches!(get("preview_contains_bang"), VmValue::Bool(true)));
    assert!(matches!(get("applied_result"), VmValue::String(s) if s.as_ref() == "applied"));
    assert!(matches!(get("match_count"), VmValue::Int(1)));

    // Disk content should reflect the second (non-dry-run) call.
    let on_disk = std::fs::read_to_string(&source_path).expect("read");
    assert!(
        on_disk.contains("hi {name}!"),
        "expected post-edit content on disk, got:\n{on_disk}"
    );
}

#[test]
fn end_to_end_insert_at_anchor_via_harn_script() {
    permissions::reset();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let source_path = root.join("hello.rs");
    fs::write(
        &source_path,
        "fn alpha() {\n    1\n}\n\nfn gamma() {\n    3\n}\n",
    )
    .unwrap();
    let path_str = source_path.to_string_lossy().replace('\\', "/");

    let path = &path_str;
    let script = format!(
        r#"
let _enable = hostlib_enable("tools:deterministic")
let preview = hostlib_ast_insert_at_anchor({{
    path: "{path}",
    query: "(function_item name: (identifier) @name (#eq? @name \"alpha\")) @anchor",
    position: "after",
    content: "fn beta() {{\n    2\n}}",
    dry_run: true,
}})
let applied = hostlib_ast_insert_at_anchor({{
    path: "{path}",
    query: "(function_item name: (identifier) @name (#eq? @name \"alpha\")) @anchor",
    position: "after",
    content: "fn beta() {{\n    2\n}}",
}})
return {{
    preview_result: preview.result,
    preview_dry: preview.dry_run,
    preview_position: preview.position,
    preview_contains_beta: contains(preview.preview, "fn beta()"),
    applied_result: applied.result,
    applied_position: applied.position,
}}
"#,
    );

    let (result, _) = run_harn(&script);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));
    assert!(matches!(get("preview_result"), VmValue::String(s) if s.as_ref() == "applied"));
    assert!(matches!(get("preview_dry"), VmValue::Bool(true)));
    assert!(matches!(get("preview_position"), VmValue::String(s) if s.as_ref() == "after"));
    assert!(matches!(get("preview_contains_beta"), VmValue::Bool(true)));
    assert!(matches!(get("applied_result"), VmValue::String(s) if s.as_ref() == "applied"));
    assert!(matches!(get("applied_position"), VmValue::String(s) if s.as_ref() == "after"));

    // Disk content should contain all three functions in order, and the
    // dry-run call should not have touched the file before the live one.
    let on_disk = std::fs::read_to_string(&source_path).expect("read");
    let alpha = on_disk.find("fn alpha()").expect("alpha present");
    let beta = on_disk.find("fn beta()").expect("beta present");
    let gamma = on_disk.find("fn gamma()").expect("gamma present");
    assert!(
        alpha < beta && beta < gamma,
        "expected alpha < beta < gamma on disk, got:\n{on_disk}"
    );
}

#[test]
fn end_to_end_dry_run_via_harn_script() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let source_path = root.join("greet.rs");
    let original = "fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n";
    fs::write(&source_path, original).unwrap();
    let path_str = source_path.to_string_lossy().replace('\\', "/");

    // Two ops in one plan: AST-precise replacement and a follow-up
    // text patch on the same file. The plan dispatcher should see the
    // first op's pending write when running the second, and produce a
    // single per-file diff that carries both edits.
    let path = &path_str;
    let script = format!(
        r#"
let bundle = hostlib_ast_dry_run({{
    plan: [
        {{
            op: "apply_node",
            path: "{path}",
            query: "(function_item body: (block) @target)",
            replacement: "{{ format!(\"hi {{name}}!\") }}",
            select: "first",
        }},
        {{
            op: "safe_text_patch",
            path: "{path}",
            old_text: "fn greet",
            new_text: "fn greeter",
        }},
    ],
}})
let diff = bundle.per_file_unified_diff[0]
return {{
    result: bundle.result,
    files_touched: bundle.summary.files_touched,
    ops_applied: bundle.summary.ops_applied,
    ops_rejected: bundle.summary.ops_rejected,
    has_unified_header: contains(diff.diff, "--- a/") && contains(diff.diff, "+++ b/"),
    has_hunk_header: contains(diff.diff, "@@ -"),
    rename_visible: contains(diff.diff, "fn greeter"),
    body_visible: contains(diff.diff, "hi {{name}}!"),
    lines_added: diff.lines_added,
    lines_removed: diff.lines_removed,
}}
"#,
    );

    let (result, _) = run_harn(&script);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));

    assert!(matches!(get("result"), VmValue::String(s) if s.as_ref() == "ok"));
    assert!(matches!(get("files_touched"), VmValue::Int(1)));
    assert!(matches!(get("ops_applied"), VmValue::Int(2)));
    assert!(matches!(get("ops_rejected"), VmValue::Int(0)));
    assert!(matches!(get("has_unified_header"), VmValue::Bool(true)));
    assert!(matches!(get("has_hunk_header"), VmValue::Bool(true)));
    assert!(matches!(get("rename_visible"), VmValue::Bool(true)));
    assert!(matches!(get("body_visible"), VmValue::Bool(true)));
    assert!(matches!(get("lines_added"), VmValue::Int(n) if *n >= 1));
    assert!(matches!(get("lines_removed"), VmValue::Int(n) if *n >= 1));

    // Critically: the on-disk file is byte-identical to the original.
    let on_disk = std::fs::read_to_string(&source_path).expect("read");
    assert_eq!(on_disk, original);
}

#[test]
fn dry_run_diff_is_git_apply_check_compatible() {
    // Validates the spec acceptance for issue #2510: the unified-diff
    // output round-trips through `git apply --check` against the
    // original tree. Skipped when `git` is not on PATH so the suite
    // stays usable in stripped CI images.
    if std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: `git` not on PATH");
        return;
    }

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let source_path = root.join("module.rs");
    let original = "fn alpha() {\n    let x = 1;\n}\n\nfn beta() {\n    let y = 2;\n}\n";
    fs::write(&source_path, original).unwrap();
    let path_str = source_path.to_string_lossy().replace('\\', "/");

    let path = &path_str;
    let script = format!(
        r#"
let bundle = hostlib_ast_dry_run({{
    plan: [
        {{
            op: "apply_node",
            path: "{path}",
            query: "(function_item name: (identifier) @name (#eq? @name \"beta\") body: (block) @target)",
            replacement: "{{ let y = 42; }}",
        }},
    ],
}})
return bundle.per_file_unified_diff[0].diff
"#,
    );

    let (result, _) = run_harn(&script);
    let diff_text = match &result {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected diff string, got {other:?}"),
    };
    assert!(!diff_text.is_empty(), "expected non-empty diff");

    // Initialize a tiny git repo and stage a file at the same path the
    // diff references, so `git apply --check` can resolve `a/<path>`
    // against the working tree.
    let repo = TempDir::new().unwrap();
    let repo_root = repo.path();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo_root)
        .status()
        .expect("git init");
    assert!(init.success());
    // `git apply` resolves `a/<path>` relative to the repo root, so
    // rewrite the diff headers to point at a flat in-repo file.
    let relative_diff = diff_text
        .replace(&format!("a/{path_str}"), "a/work.rs")
        .replace(&format!("b/{path_str}"), "b/work.rs");
    fs::write(repo_root.join("work.rs"), original).unwrap();
    let add = std::process::Command::new("git")
        .args(["add", "work.rs"])
        .current_dir(repo_root)
        .status()
        .expect("git add work.rs");
    assert!(add.success());

    let check = std::process::Command::new("git")
        .args(["apply", "--check", "-"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn git apply");
    {
        use std::io::Write;
        check
            .stdin
            .as_ref()
            .expect("stdin")
            .write_all(relative_diff.as_bytes())
            .unwrap();
    }
    let out = check.wait_with_output().expect("wait git apply");
    assert!(
        out.status.success(),
        "`git apply --check` rejected the diff:\nstdout={}\nstderr={}\ndiff=\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        relative_diff,
    );
}

#[test]
fn end_to_end_code_index_via_harn_script() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/index.ts"),
        "import { helper } from \"./util\";\nexport const ZetaToken = helper();\n",
    )
    .unwrap();
    fs::write(
        root.join("src/util.ts"),
        "export function helper() { return 'ZetaToken from util'; }\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().replace('\\', "/");

    let source = format!(
        r#"
let rebuild = hostlib_code_index_rebuild({{ root: "{root_str}" }})
let stats = hostlib_code_index_stats({{}})
let q = hostlib_code_index_query({{ needle: "ZetaToken", max_results: 10 }})
let imps = hostlib_code_index_imports_for({{ path: "src/index.ts" }})
let users = hostlib_code_index_importers_of({{ module: "src/util.ts" }})
return {{
    indexed: rebuild.files_indexed,
    files: stats.indexed_files,
    hits: len(q.results),
    first_path: q.results[0].path,
    imports_kind: imps.imports[0].kind,
    importer: users.importers[0],
}}
"#,
    );

    let (result, _) = run_harn(&source);
    let dict = match &result {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let get = |k: &str| dict.get(k).unwrap_or_else(|| panic!("missing {k}"));

    let VmValue::Int(indexed) = get("indexed") else {
        panic!("indexed");
    };
    assert!(*indexed >= 2);
    let VmValue::Int(hits) = get("hits") else {
        panic!("hits");
    };
    assert!(*hits >= 2, "expected at least 2 hits, got {hits}");
    assert!(matches!(
        get("imports_kind"),
        VmValue::String(s) if s.as_ref() == "import"
    ));
    assert!(matches!(
        get("importer"),
        VmValue::String(s) if s.as_ref() == "src/index.ts"
    ));
}
