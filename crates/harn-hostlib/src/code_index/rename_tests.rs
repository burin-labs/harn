use super::*;

use std::fs;
use tempfile::tempdir;

use crate::code_index::CodeIndexCapability;

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    VmValue::dict(map)
}

fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("missing field"),
        _ => panic!("expected dict, got {value:?}"),
    }
}

fn s(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn list_len(value: &VmValue) -> usize {
    match value {
        VmValue::List(list) => list.len(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn build_index(root: &Path) -> CodeIndexCapability {
    let capability = CodeIndexCapability::new();
    let shared = capability.shared();
    let mut guard = shared.lock().expect("mutex");
    let (state, _outcome) = IndexState::build_from_root(root);
    *guard = Some(state);
    drop(guard);
    capability
}

fn rename(
    capability: &CodeIndexCapability,
    symbol_ref: VmValue,
    new_name: &str,
    scope: &str,
) -> VmValue {
    let entries = vec![
        ("symbol_ref", symbol_ref),
        ("new_name", vm_string(new_name)),
        ("scope", vm_string(scope)),
    ];
    run(&capability.shared(), &[dict(&entries)]).expect("rename runs")
}

fn replace(
    capability: &CodeIndexCapability,
    symbol_ref: VmValue,
    replacement_text: &str,
    scope: &str,
) -> VmValue {
    let entries = vec![
        ("symbol_ref", symbol_ref),
        ("replacement_text", vm_string(replacement_text)),
        ("scope", vm_string(scope)),
    ];
    run(&capability.shared(), &[dict(&entries)]).expect("replace runs")
}

#[test]
fn rust_workspace_rename_rewrites_definitions_and_call_sites() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
            root.join("src/lib.rs"),
            "pub struct Widget {\n    pub size: u32,\n}\n\nimpl Widget {\n    pub fn new() -> Widget { Widget { size: 0 } }\n}\n",
        )
        .unwrap();
    fs::write(
            root.join("src/main.rs"),
            "use crate::Widget;\nfn main() {\n    let w: Widget = Widget::new();\n    let _ = w.size;\n}\n",
        )
        .unwrap();

    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    assert_eq!(list_len(field(&result, "touched_files")), 2);
    let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(lib.contains("struct Gadget"));
    assert!(lib.contains("impl Gadget"));
    assert!(!lib.contains("Widget"));
    assert!(main.contains("use crate::Gadget;"));
    assert!(main.contains("let w: Gadget = Gadget::new();"));
}

#[test]
fn rename_skips_string_literals_and_comments() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
            root.join("src/lib.rs"),
            "// Widget is the doc word\npub struct Widget { size: u32 }\nfn label() -> &'static str { \"Widget\" }\n",
        )
        .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert!(lib.contains("struct Gadget"));
    // Both the comment word "Widget" and the string literal "Widget"
    // remain because they're not identifier-context tokens.
    assert!(lib.contains("// Widget is the doc word"));
    assert!(lib.contains("\"Widget\""));
}

#[test]
fn rename_detects_shadow_conflict_and_skips_write() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let original_main = "pub struct Widget {}\npub struct Gadget {}\nfn main() { let _ = Widget {}; let _ = Gadget {}; }\n";
    fs::write(root.join("src/main.rs"), original_main).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/main.rs")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "conflict");
    assert!(list_len(field(&result, "conflicts")) >= 1);
    let on_disk = fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert_eq!(on_disk, original_main, "rename must not write on conflict");
}

#[test]
fn workspace_rename_does_not_change_an_unrelated_same_named_type() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let target = "pub struct Reading { pub value: i32 }\n";
    let unrelated = "pub struct Reading { pub archived: bool }\n";
    fs::write(root.join("src/target.rs"), target).unwrap();
    fs::write(root.join("src/archive.rs"), unrelated).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub mod target;\npub mod archive;\n",
    )
    .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Reading")),
        ("path", vm_string("src/target.rs")),
        ("kind", vm_string("Type")),
    ]);

    let result = rename(&capability, symbol_ref, "Measurement", "workspace");
    assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
    assert_eq!(
        fs::read_to_string(root.join("src/target.rs")).unwrap(),
        target
    );
    assert_eq!(
        fs::read_to_string(root.join("src/archive.rs")).unwrap(),
        unrelated
    );
}

#[test]
fn dry_run_does_not_modify_disk() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let original = "fn alpha() { println!(\"hi\"); }\nfn caller() { alpha(); }\n";
    fs::write(root.join("src/lib.rs"), original).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("alpha")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = run(
        &capability.shared(),
        &[dict(&[
            ("symbol_ref", symbol_ref),
            ("new_name", vm_string("beta")),
            ("scope", vm_string("workspace")),
            ("dry_run", VmValue::Bool(true)),
        ])],
    )
    .expect("rename runs");
    assert_eq!(s(field(&result, "result")), "applied");
    let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(on_disk, original, "dry_run must leave disk untouched");
}

#[test]
fn typescript_workspace_rename() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/widget.ts"),
        "export class Widget {\n  size = 0;\n  resize(n: number) { this.size = n; }\n}\n",
    )
    .unwrap();
    fs::write(
            root.join("src/main.ts"),
            "import { Widget } from \"./widget\";\nconst w = new Widget();\nw.resize(7);\nconsole.log(w.size);\n",
        )
        .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/widget.ts")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let widget = fs::read_to_string(root.join("src/widget.ts")).unwrap();
    let main = fs::read_to_string(root.join("src/main.ts")).unwrap();
    assert!(widget.contains("class Gadget"));
    assert!(main.contains("import { Gadget }"));
    assert!(main.contains("new Gadget()"));
}

#[test]
fn python_workspace_rename() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(
        root.join("pkg/widget.py"),
        "class Widget:\n    def __init__(self):\n        self.size = 0\n",
    )
    .unwrap();
    fs::write(
        root.join("pkg/main.py"),
        "from .widget import Widget\nw = Widget()\nprint(w.size)\n",
    )
    .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("pkg/widget.py")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let widget = fs::read_to_string(root.join("pkg/widget.py")).unwrap();
    let main = fs::read_to_string(root.join("pkg/main.py")).unwrap();
    assert!(widget.contains("class Gadget"));
    assert!(main.contains("from .widget import Gadget"));
    assert!(main.contains("w = Gadget()"));
}

#[test]
fn go_workspace_rename() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("widget")).unwrap();
    fs::write(
            root.join("widget/widget.go"),
            "package widget\n\ntype Widget struct{ Size int }\n\nfunc New() *Widget { return &Widget{Size: 0} }\n",
        )
        .unwrap();
    fs::write(
            root.join("main.go"),
            "package main\n\nimport \"./widget\"\n\nfunc main() {\n    w := widget.New()\n    _ = w\n}\n",
        )
        .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("widget/widget.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let widget = fs::read_to_string(root.join("widget/widget.go")).unwrap();
    assert!(widget.contains("type Gadget struct"));
    assert!(widget.contains("*Gadget"));
    assert!(!widget.contains("Widget"));
}

#[test]
fn swift_file_rename() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::write(
            root.join("Widget.swift"),
            "class Widget {\n  var size = 0\n  func resize(_ n: Int) { self.size = n }\n}\n\nlet w = Widget()\nw.resize(3)\n",
        )
        .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("Widget.swift")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "file");
    assert_eq!(s(field(&result, "result")), "applied");
    let swift = fs::read_to_string(root.join("Widget.swift")).unwrap();
    assert!(swift.contains("class Gadget"));
    assert!(swift.contains("let w = Gadget()"));
}

#[test]
fn harn_workspace_rename() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
            root.join("src/task.harn"),
            "pub struct Task {\n  title: string\n}\n\nimpl Task {\n  fn summary(task: Task) -> string {\n    return task.title\n  }\n}\n\nfn summarize(task: Task) -> string {\n  return task.title\n}\n",
        )
        .unwrap();

    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Task")),
        ("path", vm_string("src/task.harn")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Job", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let source = fs::read_to_string(root.join("src/task.harn")).unwrap();
    assert!(source.contains("struct Job"));
    assert!(source.contains("impl Job"));
    assert!(source.contains("task: Job"));
    assert!(source.contains("return task.title"));
    assert!(!source.contains("Task"));
}

#[test]
fn ambiguous_symbol_without_disambiguator() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.rs"),
        "fn helper() {}\nfn helper2() { helper(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/b.rs"),
        "fn helper() {}\nfn helper3() { helper(); }\n",
    )
    .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("helper")),
        ("path", vm_string("src/missing.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol_ref, "renamed", "workspace");
    assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
}

#[test]
fn rejects_invalid_new_name() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("alpha")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol_ref, "1bad", "file");
    assert_eq!(s(field(&result, "result")), "invalid_identifier");
}

#[test]
fn staged_fs_session_routes_writes_through_overlay() {
    // With a staged-fs session in `staged` mode, the rename's
    // touched files must NOT land on the working tree — the
    // session's staging layer holds them until commit. After
    // commit, the files reflect the rename.
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let original = "pub fn alpha() {}\nfn caller() { alpha(); }\n";
    std::fs::write(root.join("src/lib.rs"), original).unwrap();
    let capability = build_index(&root);
    // The temporary root already has a unique name, so staged manifests
    // cannot collide without reading the wall clock.
    let session_id = format!(
        "rename-test-{}",
        root.file_name()
            .expect("temporary root name")
            .to_string_lossy()
    );

    crate::fs::configure_session_root(&session_id, &root);
    crate::fs::set_mode(&session_id, crate::fs::FsMode::Staged, Some(&root)).expect("set_mode");

    let symbol_ref = dict(&[
        ("name", vm_string("alpha")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = run(
        &capability.shared(),
        &[dict(&[
            ("symbol_ref", symbol_ref),
            ("new_name", vm_string("beta")),
            ("scope", vm_string("workspace")),
            ("session_id", vm_string(&session_id)),
        ])],
    )
    .expect("rename runs");
    assert_eq!(s(field(&result, "result")), "applied");

    // Working tree must be untouched while changes sit in the
    // staging overlay.
    let on_disk_pre = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(
        on_disk_pre, original,
        "staged session must not flush to disk before commit"
    );

    // Status should report one pending write.
    let status = crate::fs::staged_status(&session_id).expect("status");
    assert_eq!(
        status.pending_writes.len(),
        1,
        "expected one pending file; got {status:?}"
    );

    // Commit flushes the rename through.
    let commit = crate::fs::commit_staged(&session_id, &[]).expect("commit");
    assert!(
        commit.failed_paths_with_reasons.is_empty(),
        "commit reported failed paths: {:?}",
        commit.failed_paths_with_reasons
    );
    let on_disk_post = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert!(on_disk_post.contains("fn beta()"));
    assert!(on_disk_post.contains("beta();"));
    assert!(!on_disk_post.contains("alpha"));
}

// === replace mode (#edit-precision): arbitrary-text replacement of a
// symbol's true-identifier occurrences across files in one atomic call. ===

#[test]
fn replace_mode_rewrites_call_sites_across_files_with_arbitrary_text() {
    // An API migration: every call to `old_api` becomes `client.fetch` —
    // NOT a valid bare identifier, so rename mode would reject it, but
    // replace mode swaps all true-identifier occurrences across both files
    // in one shot while leaving the string literal untouched.
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
            root.join("src/lib.rs"),
            "pub fn old_api() -> u32 { 0 }\n// old_api is deprecated\nfn helper() -> &'static str { \"old_api\" }\n",
        )
        .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "use crate::old_api;\nfn main() {\n    let _ = old_api();\n}\n",
    )
    .unwrap();

    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("old_api")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = replace(&capability, symbol_ref, "client_fetch", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    assert_eq!(list_len(field(&result, "touched_files")), 2);

    let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
    // Definition + call sites rewritten.
    assert!(lib.contains("pub fn client_fetch()"));
    assert!(main.contains("use crate::client_fetch;"));
    assert!(main.contains("let _ = client_fetch();"));
    // String literal and comment text are preserved verbatim.
    assert!(lib.contains("// old_api is deprecated"));
    assert!(lib.contains("\"old_api\""));
}

#[test]
fn replace_mode_bypasses_identifier_gate_but_keeps_syntax_validation() {
    // `client.fetch` is not a valid identifier. Rename mode would reject it
    // up front with `invalid_identifier`. Replace mode bypasses that gate —
    // it gets as far as splicing every occurrence (including the definition
    // `fn client.fetch`, which does not parse) and is then caught by the
    // SAME syntax-validation safety net, returning `syntax_error` (not
    // `invalid_identifier`). This proves: (a) the identifier gate is
    // bypassed in replace mode, and (b) validation still protects disk.
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let original =
        "fn fetch_thing(n: u32) -> u32 { n }\nfn caller() {\n    let _ = fetch_thing(1);\n}\n";
    fs::write(root.join("src/lib.rs"), original).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("fetch_thing")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = replace(&capability, symbol_ref, "client.fetch", "file");
    let tag = s(field(&result, "result"));
    assert_eq!(
        tag, "syntax_error",
        "replace mode must skip the identifier gate yet still validate syntax"
    );
    assert_ne!(tag, "invalid_identifier");
    let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(on_disk, original, "syntax_error must not write");
}

#[test]
fn replace_mode_ignores_shadow_conflicts() {
    // `Gadget` already exists; a RENAME of Widget->Gadget would abort with
    // a shadow conflict, but REPLACE mode intentionally allows it (the
    // caller is doing a deliberate textual replacement).
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
            root.join("src/main.rs"),
            "pub struct Widget {}\npub struct Gadget {}\nfn main() { let _ = Widget {}; let _ = Gadget {}; }\n",
        )
        .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/main.rs")),
        ("kind", vm_string("Type")),
    ]);
    let result = replace(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "applied",
        "replace mode must not raise a shadow conflict"
    );
}

#[test]
fn replace_mode_aborts_when_result_does_not_parse() {
    // Replacing the identifier with a syntactically broken token must abort
    // with no write — the same syntax-validation safety net as rename mode.
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let original = "fn thing() {}\nfn caller() {\n    let _ = thing();\n}\n";
    fs::write(root.join("src/lib.rs"), original).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("thing")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    // `@@@` is not valid Rust; the spliced file fails to re-parse.
    let result = replace(&capability, symbol_ref, "@@@", "file");
    assert_eq!(s(field(&result, "result")), "syntax_error");
    let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(on_disk, original, "syntax_error must not write");
}

#[test]
fn replace_mode_rejects_empty_replacement_text() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn thing() {}\n").unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("thing")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let err = run(
        &capability.shared(),
        &[dict(&[
            ("symbol_ref", symbol_ref),
            ("replacement_text", vm_string("")),
            ("scope", vm_string("file")),
        ])],
    );
    assert!(err.is_err(), "empty replacement_text must be rejected");
}

#[test]
fn replace_mode_dry_run_does_not_write() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let original = "fn alpha() {}\nfn caller() { alpha(); }\n";
    fs::write(root.join("src/lib.rs"), original).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("alpha")),
        ("path", vm_string("src/lib.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = run(
        &capability.shared(),
        &[dict(&[
            ("symbol_ref", symbol_ref),
            ("replacement_text", vm_string("alpha2")),
            ("scope", vm_string("file")),
            ("dry_run", VmValue::Bool(true)),
        ])],
    )
    .expect("replace runs");
    assert_eq!(s(field(&result, "result")), "applied");
    match field(&result, "match_count") {
        VmValue::Int(n) => assert_eq!(*n, 2, "both occurrences planned"),
        other => panic!("expected int match_count, got {other:?}"),
    }
    let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(on_disk, original, "dry_run must leave disk untouched");
}

#[test]
fn rename_refuses_a_file_that_does_not_parse_and_writes_nothing() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let util = "def fetch(url):\n    got = url +\n    print(got)\n";
    let app = "from util import fetch\n\nfetch(\"a\")\n";
    // Mentions the name only in a comment, so it is never rewritten and its
    // own syntax error must not block the rename.
    let notes = "# fetch is documented elsewhere\nbroken = (\n";
    fs::write(root.join("util.py"), util).unwrap();
    fs::write(root.join("app.py"), app).unwrap();
    fs::write(root.join("notes.py"), notes).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("fetch")),
        ("path", vm_string("util.py")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol_ref.clone(), "load", "workspace");
    assert_eq!(s(field(&result, "result")), "syntax_error");
    assert!(s(field(&result, "details")).contains("`util.py` does not parse before the edit"));
    assert_eq!(fs::read_to_string(root.join("util.py")).unwrap(), util);
    assert_eq!(fs::read_to_string(root.join("app.py")).unwrap(), app);

    fs::write(root.join("util.py"), "def fetch(url):\n    return url\n").unwrap();
    let capability = build_index(root);
    let result = rename(&capability, symbol_ref, "load", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    assert!(fs::read_to_string(root.join("app.py"))
        .unwrap()
        .contains("load(\"a\")"));
    assert_eq!(fs::read_to_string(root.join("notes.py")).unwrap(), notes);
}
