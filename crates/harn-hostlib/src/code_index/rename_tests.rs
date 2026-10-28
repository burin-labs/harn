use super::*;
use crate::test_support::*;

use std::fs;
use tempfile::tempdir;

use crate::code_index::CodeIndexCapability;

fn list_len(value: &VmValue) -> usize {
    match value {
        VmValue::List(list) => list.len(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn build_index(root: &Path) -> CodeIndexCapability {
    if root.join("src/lib.rs").exists() {
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"rename-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
    }
    let capability = CodeIndexCapability::new();
    let shared = capability.shared();
    let mut guard = shared.lock().expect("mutex");
    let (state, _outcome) = IndexState::build_from_root(root);
    *guard = Some(state);
    drop(guard);
    capability
}

#[test]
fn registered_positioned_rename_selects_exact_declarations_and_refuses_wrong_constraints() {
    use crate::{BuiltinRegistry, HostlibCapability};
    let dir = tempdir().unwrap();
    for (file, source) in [
        (
            "a.ts",
            "/* 😀 */ export function load() { return 1; }\nconst result = load();\n",
        ),
        (
            "a.rs",
            "/* 😀 */ pub fn load() -> i32 { 1 }\npub fn result() -> i32 { load() }\n",
        ),
    ] {
        let source_path = dir.path().join(file);
        fs::write(&source_path, source).unwrap();
        let capability = build_index(dir.path());
        {
            let shared = capability.shared();
            let guard = shared.lock().unwrap();
            assert_eq!(
                guard
                    .as_ref()
                    .unwrap()
                    .symbols
                    .iter_nodes()
                    .filter(|node| node.kind == NodeKind::Function && node.name == "load")
                    .count(),
                1
            );
        }
        let mut registry = BuiltinRegistry::new();
        capability.register_builtins(&mut registry);
        let offset = source.find("load").unwrap();
        let col = source.get(..offset).unwrap().encode_utf16().count() as i64 + 1;
        let request = |name: &str, path: &str, column: i64| {
            dict(&[
                (
                    "symbol_ref",
                    dict(&[
                        ("name", vm_string(name)),
                        ("path", vm_string(path)),
                        ("line", VmValue::Int(1)),
                        ("col", VmValue::Int(column)),
                        ("kind", vm_string("Function")),
                    ]),
                ),
                ("new_name", vm_string("loadById")),
                ("scope", vm_string("workspace")),
                ("backend", vm_string("syntax_tree")),
            ])
        };
        for (name, path, column) in [
            ("load", file, offset as i64 + 1),
            ("Unknown.load", file, col),
            ("load", "missing.ts", col),
        ] {
            let result =
                (registry.find(BUILTIN).unwrap().handler)(&[request(name, path, column)]).unwrap();
            assert_eq!(
                s(field(&result, "result")),
                "no_match",
                "{file}: {result:?}"
            );
            assert_eq!(fs::read_to_string(&source_path).unwrap(), source);
        }
        for column in [0, -1, i64::from(u32::MAX) + 1] {
            let result =
                (registry.find(BUILTIN).unwrap().handler)(&[request("load", file, column)]);
            assert!(
                matches!(result, Err(HostlibError::InvalidParameter { .. })),
                "{result:?}"
            );
            assert_eq!(fs::read_to_string(&source_path).unwrap(), source);
        }
        let result =
            (registry.find(BUILTIN).unwrap().handler)(&[request("load", file, col)]).unwrap();
        assert_eq!(s(field(&result, "result")), "applied", "{file}: {result:?}");
        assert_eq!(
            fs::read_to_string(&source_path).unwrap(),
            source.replace("load", "loadById")
        );
        fs::remove_file(&source_path).unwrap();
    }
}

#[test]
fn workspace_rename_refuses_unindexed_bindings_before_any_write() {
    let cases = [
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\n\ndef parse(Reading):\n    return Reading()\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from elsewhere import Thing as Reading\nvalue = Reading()\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\nvalue = object.Reading\n"),
        ("reading.ts", "export class Reading {}\n", "consumer.ts", "import { Reading } from './reading';\nfunction parse(Reading: any) { return Reading; }\n"),
        ("reading.ts", "export class Reading {}\n", "consumer.ts", "import { Other as Reading } from './elsewhere';\nconst value = Reading;\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\nfn parse(Reading: i32) -> i32 { Reading }\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\nfn parse() { let Reading = 1; let _ = Reading; }\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\nfn parse() { let _ = unrelated::Reading; }\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\nmod inner { fn parse() { let _ = Reading; } }\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import *\nvalue = Reading()\n"),
        ("Reading.py", "class Reading:\n    pass\n", "consumer.py", "from Reading import Reading\nvalue = Reading()\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\nclass Scope(type):\n    @classmethod\n    def __prepare__(mcls, name, bases):\n        return {\"Reading\": int}\nclass Consumer(metaclass=Scope):\n    value = Reading(\"1\")\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\nclass Scope(type):\n    @classmethod\n    def __prepare__(mcls, name, bases):\n        return {\"Reading\": int}\nclass Consumer(metaclass=Scope):\n    def method(self, value=Reading(\"1\")):\n        return value\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\ndef probe(ℝeading):\n    return Reading(\"1\")\n"),
        ("reading.py", "class Reading:\n    pass\n", "consumer.py", "from reading import Reading\nimport other\nvalue: other.Reading = Reading()\n"),
        ("reading.ts", "export class Reading {}\n", "consumer.ts", "import { Reading } from './reading';\nexport function pair<Reading>(v: Reading): Reading { return v; }\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\npub fn pair<Reading: Clone>(v: Reading) -> Reading { v }\n"),
        ("src/lib.rs", "pub struct Reading;\n", "src/consumer.rs", "use crate::Reading;\npub fn f() -> usize { my_crate::count!(Reading) }\n"),
    ];
    for (seed, original, consumer, unrelated) in cases {
        for replacement in [false, true] {
            let dir = tempdir().unwrap();
            for (path, text) in [(seed, original), (consumer, unrelated)] {
                fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
                fs::write(dir.path().join(path), text).unwrap();
            }
            let capability = build_index(dir.path());
            let symbol = dict(&[
                ("name", vm_string("Reading")),
                ("path", vm_string(seed)),
                ("kind", vm_string("Type")),
            ]);
            let result = if replacement {
                replace(&capability, symbol, "Measurement", "workspace")
            } else {
                rename(&capability, symbol, "Measurement", "workspace")
            };
            assert_eq!(
                s(field(&result, "result")),
                "ambiguous_symbol",
                "{consumer}: {unrelated}"
            );
            assert_eq!(fs::read_to_string(dir.path().join(seed)).unwrap(), original);
            assert_eq!(
                fs::read_to_string(dir.path().join(consumer)).unwrap(),
                unrelated
            );
        }
    }
}

#[test]
fn rename_refuses_macro_and_dynamic_binding_scopes_before_writes() {
    for (path, original) in [
        ("src/lib.rs", "fn Reading() -> i32 { 1 }\nfn probe() -> i32 { binding_macro::bind!(); Reading() }\n"),
        ("src/lib.rs", "fn Reading() -> i32 { 1 }\n#[binding_macro::bind]\nfn probe() -> i32 { Reading() }\n"),
        ("src/lib.rs", "fn Reading() -> i32 { 1 }\nfn probe() -> i32 { let r#Reading = || 7; Reading() }\n"),
        ("script.js", "function Reading() { return 1; }\nfunction probe() { eval(\"var Read\" + \"ing = () => 7;\"); return Reading(); }\n"),
        ("script.js", "function Reading() { return 1; }\nfunction probe() { ((eval))(\"var Read\" + \"ing = () => 7;\"); return Reading(); }\n"),
        ("script.js", "function Reading() { return 1; }\nwith ({[\"Reading\"]: () => 7}) { Reading(); }\n"),
        ("script.js", "function Reading() { return 1; }\nfunction probe() { const Read\\u0069ng = () => 7; return Reading(); }\n"),
    ] {
        for replacement in [false, true] {
            let dir = tempdir().unwrap();
            fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
            fs::write(dir.path().join(path), original).unwrap();
            let capability = build_index(dir.path());
            let symbol = dict(&[("name", vm_string("Reading")), ("path", vm_string(path)), ("kind", vm_string("Function"))]);
            let result = if replacement {
                replace(&capability, symbol, "Measurement", "workspace")
            } else {
                rename(&capability, symbol, "Measurement", "workspace")
            };
            assert_eq!(s(field(&result, "result")), "ambiguous_symbol", "{original}");
            assert_eq!(fs::read_to_string(dir.path().join(path)).unwrap(), original);
        }
    }
}

#[test]
fn rename_applies_through_std_macros_derives_and_type_positions() {
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a str, &'a [&'a str]);
    let cases: [Case<'_>; 3] = [
        (
            &[
                ("src/lib.rs", "#[derive(Debug, Clone)]\npub struct Reading { pub value: i32 }\nimpl Reading {\n    pub fn new(value: i32) -> Reading { Reading { value } }\n}\npub fn all() -> Vec<Reading> { vec![Reading::new(1)] }\npub fn show(r: &Reading) -> String { format!(\"{:?}\", r) }\n"),
                ("src/main.rs", "use rename_fixture::Reading;\n#[cfg(test)]\nmod tests {}\nfn main() {\n    let r: [Reading; 1] = [Reading::new(2)];\n    assert_eq!(r[0].value, Reading::new(2).value);\n    println!(\"{}\", \"Reading\");\n}\n"),
            ],
            "src/lib.rs",
            "src/main.rs",
            &["let r: [Measurement; 1] = [Measurement::new(2)];", "assert_eq!(r[0].value, Measurement::new(2).value);", "\"Reading\""],
        ),
        (
            &[
                ("reading.ts", "export class Reading {}\n"),
                ("consumer.ts", "import { Reading } from './reading';\nexport const all: Reading[] = [new Reading()];\nexport function pick(xs: Array<Reading>): Reading | undefined { return xs[0]; }\n"),
            ],
            "reading.ts",
            "consumer.ts",
            &["all: Measurement[] = [new Measurement()]", "pick(xs: Array<Measurement>): Measurement | undefined"],
        ),
        (
            &[
                ("reading.py", "class Reading:\n    pass\n"),
                ("consumer.py", "from typing import List\nfrom reading import Reading\n\ndef load() -> List[Reading]:\n    return [Reading()]\n\ndef first(xs: list[Reading]) -> Reading:\n    return xs[0]\n"),
            ],
            "reading.py",
            "consumer.py",
            &["def load() -> List[Measurement]:", "def first(xs: list[Measurement]) -> Measurement:"],
        ),
    ];
    for (files, seed, consumer, expected) in cases {
        let dir = tempdir().unwrap();
        for (path, text) in files {
            fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
            fs::write(dir.path().join(path), text).unwrap();
        }
        let capability = build_index(dir.path());
        let symbol = dict(&[
            ("name", vm_string("Reading")),
            ("path", vm_string(seed)),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol, "Measurement", "workspace");
        assert_eq!(s(field(&result, "result")), "applied", "{seed}: {result:?}");
        let rewritten = fs::read_to_string(dir.path().join(consumer)).unwrap();
        for snippet in expected {
            assert!(
                rewritten.contains(snippet),
                "{snippet} missing from:\n{rewritten}"
            );
        }
    }
}

#[test]
fn python_class_bases_and_method_bodies_keep_the_global_binding() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("reading.py"), "class Reading:\n    pass\n").unwrap();
    let consumer = "from reading import Reading\nclass Scope(type):\n    @classmethod\n    def __prepare__(mcls, name, bases):\n        return {\"Reading\": int}\nclass Consumer(Reading, metaclass=Scope):\n    def make(self):\n        return Reading()\n    make_lambda = lambda self: Reading()\n";
    fs::write(dir.path().join("consumer.py"), consumer).unwrap();
    let capability = build_index(dir.path());
    let symbol = dict(&[
        ("name", vm_string("Reading")),
        ("path", vm_string("reading.py")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol, "Measurement", "workspace");
    assert_eq!(s(field(&result, "result")), "applied");
    let rewritten = fs::read_to_string(dir.path().join("consumer.py")).unwrap();
    assert!(rewritten.contains("class Consumer(Measurement, metaclass=Scope)"));
    assert!(rewritten.contains("return Measurement()"));
    assert!(rewritten.contains("lambda self: Measurement()"));
    assert!(rewritten.contains("{\"Reading\": int}"));
}

#[test]
fn rename_refuses_a_stale_seed_before_writing_importers() {
    for current in ["class Other:\n    pass\n", "# Reading was removed\n"] {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("reading.py"), "class Reading:\n    pass\n").unwrap();
        let consumer = "from reading import Reading\nvalue = Reading()\n";
        fs::write(dir.path().join("consumer.py"), consumer).unwrap();
        let capability = build_index(dir.path());
        fs::write(dir.path().join("reading.py"), current).unwrap();
        let symbol = dict(&[
            ("name", vm_string("Reading")),
            ("path", vm_string("reading.py")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol, "Measurement", "workspace");
        assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
        assert_eq!(
            fs::read_to_string(dir.path().join("reading.py")).unwrap(),
            current
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("consumer.py")).unwrap(),
            consumer
        );
    }
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
        // These cases pin the syntax-tree engine; `lsp/rename_lsp_tests.rs`
        // covers the language-server backend.
        ("backend", vm_string("syntax_tree")),
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
            "use rename_fixture::Widget;\nfn main() {\n    let w: Widget = Widget::new();\n    let _ = w.size;\n}\n",
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
    assert!(main.contains("use rename_fixture::Gadget;"));
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
    let original = "fn alpha() { let _ = 1; }\nfn caller() { alpha(); }\n";
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
fn workspace_rename_leaves_prose_files_and_renames_the_code() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/widget.ts"),
        "export class Widget {}\nexport const w = new Widget();\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "# Usage\n\nCreate a `Widget` first.\n",
    )
    .unwrap();
    fs::write(
        root.join("index.html"),
        "<script type=\"module\">new Widget();</script>\n",
    )
    .unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("Widget")),
        ("path", vm_string("src/widget.ts")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "Gadget", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert!(fs::read_to_string(root.join("src/widget.ts"))
        .unwrap()
        .contains("new Gadget()"));
    assert!(fs::read_to_string(root.join("README.md"))
        .unwrap()
        .contains("`Widget`"));
    let warnings = format!("{:?}", field(&result, "warnings"));
    assert!(warnings.contains("README.md"), "{warnings}");
    // Markup can still use the old name, so the warning asks for a review.
    assert!(
        warnings.contains(
            "index.html (html file left unchanged; it may still reference `Widget`, so review it)"
        ),
        "{warnings}"
    );
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
fn go_workspace_rename_detects_name_collision_in_sibling_without_old_name() {
    for sibling_declaration in [
        "type DocumentLibrary struct { Name string }",
        "var DocumentLibrary int",
        "const DocumentLibrary = 1",
        "func DocumentLibrary() {}",
    ] {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("lab")).unwrap();
        let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
        let sibling = format!("package lab\n\n{sibling_declaration}\n");
        fs::write(root.join("lab/store.go"), declaration).unwrap();
        fs::write(root.join("lab/other.go"), &sibling).unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("DocumentStore")),
            ("path", vm_string("lab/store.go")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
        assert_eq!(
            s(field(&result, "result")),
            "conflict",
            "{sibling_declaration}: {result:?}"
        );
        assert!(list_len(field(&result, "conflicts")) >= 1);
        assert_eq!(
            fs::read_to_string(root.join("lab/store.go")).unwrap(),
            declaration
        );
        assert_eq!(
            fs::read_to_string(root.join("lab/other.go")).unwrap(),
            sibling
        );
    }
}

#[test]
fn go_rename_commit_refuses_a_package_change_and_writes_when_unchanged() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let seed_path = root.join("store.go");
    fs::write(&seed_path, declaration).unwrap();
    let (state, _) = IndexState::build_from_root(&root);
    let (shadows, proof) =
        super::super::rename_go::inspect(&root, "store.go", "DocumentLibrary", None).unwrap();
    assert!(shadows.is_empty());
    let tree = ast_api::parse_tree(declaration, Language::Go).unwrap();
    let start = declaration.find("DocumentStore").unwrap();
    let node = tree
        .root_node()
        .named_descendant_for_byte_range(start, start + "DocumentStore".len())
        .unwrap();
    let edit = EditSpan {
        span: super::super::refactor_core::IdentifierSpan::of(node),
        before: "DocumentStore".into(),
        after: "DocumentLibrary".into(),
    };
    let plan = plan_file(
        "store.go".into(),
        Language::Go,
        declaration.into(),
        vec![edit],
        true,
    )
    .unwrap();
    let env = ResponseEnv {
        symbol_name: "DocumentStore",
        new_name: "DocumentLibrary",
        symbol_path: "store.go",
        symbol_line: None,
        symbol_kind: Some(NodeKind::Type),
        scope: Scope::Workspace,
        backend: BackendReport::default(),
        stale: Vec::new(),
        proof: false,
        preserved: None,
        graph: &state.symbols,
    };
    let added = root.join("other.go");
    fs::write(&added, "package lab\ntype DocumentLibrary struct{}\n").unwrap();
    let call = HostCallContext::current();
    let plans = vec![plan];
    let result = commit(&env, &call, &root, &plans, None, Some(&proof))
        .unwrap()
        .unwrap_err();
    assert_eq!(s(field(&result, "result")), "refused");
    assert_eq!(fs::read_to_string(&seed_path).unwrap(), declaration);
    fs::remove_file(&added).unwrap();
    let failed = commit(&env, &call, &root, &plans, None, Some(&proof))
        .unwrap()
        .unwrap();
    assert!(failed.is_empty());
    assert_eq!(
        fs::read_to_string(&seed_path).unwrap(),
        declaration.replace("DocumentStore", "DocumentLibrary")
    );
}

#[test]
fn go_rename_allows_same_name_in_external_test_package() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let external = "package lab_test\n\ntype DocumentLibrary struct { Name string }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/other_test.go"), external).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration.replace("DocumentStore", "DocumentLibrary")
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/other_test.go")).unwrap(),
        external
    );
}

#[test]
fn go_rename_allows_same_spelling_in_a_sibling_field() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\ntype Settings struct { DocumentLibrary int }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/other.go"), sibling).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration.replace("DocumentStore", "DocumentLibrary")
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/other.go")).unwrap(),
        sibling
    );
}

#[test]
fn go_workspace_rename_refuses_shadowed_sibling_type_before_writes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\nfunc Read(s *DocumentStore) int { return s.Count }\nfunc Identity[DocumentStore any](s DocumentStore) DocumentStore { return s }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/use.go"), sibling).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/use.go")).unwrap(),
        sibling
    );
}

#[test]
fn go_workspace_rename_refuses_local_name_shadow_before_writes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\nfunc Read(s *DocumentStore) int { return s.Count }\nfunc Shadow() int { DocumentStore := 7; return DocumentStore }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/use.go"), sibling).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/use.go")).unwrap(),
        sibling
    );
}

#[test]
fn go_workspace_rename_refuses_unproven_build_variant_before_writes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let variant =
        "//go:build windows\n\npackage lab\n\nfunc Read(s *DocumentStore) int { return s.Count }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/use_windows.go"), variant).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/use_windows.go")).unwrap(),
        variant
    );
}

#[test]
fn go_rename_detects_collision_in_a_new_staged_sibling() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\ntype DocumentLibrary struct { Name string }\n";
    let seed_path = root.join("lab/store.go");
    let sibling_path = root.join("lab/new.go");
    fs::write(&seed_path, declaration).unwrap();
    let capability = build_index(&root);
    let session_id = format!(
        "go-rename-new-{}",
        root.file_name().unwrap().to_string_lossy()
    );
    crate::fs::configure_session_root(&session_id, &root);
    crate::fs::set_mode(&session_id, crate::fs::FsMode::Staged, Some(&root)).unwrap();
    crate::fs::stage_write_or_none(
        BUILTIN,
        &sibling_path,
        sibling.as_bytes(),
        false,
        true,
        Some(&session_id),
    )
    .unwrap()
    .expect("sibling was staged");
    let result = run(
        &capability.shared(),
        &[dict(&[
            (
                "symbol_ref",
                dict(&[
                    ("name", vm_string("DocumentStore")),
                    ("path", vm_string("lab/store.go")),
                    ("kind", vm_string("Type")),
                ]),
            ),
            ("new_name", vm_string("DocumentLibrary")),
            ("scope", vm_string("workspace")),
            ("backend", vm_string("syntax_tree")),
            ("session_id", vm_string(&session_id)),
        ])],
    )
    .unwrap();
    assert_eq!(s(field(&result, "result")), "conflict", "{result:?}");
    assert_eq!(fs::read_to_string(&seed_path).unwrap(), declaration);
    assert!(!sibling_path.exists());
    assert_eq!(
        crate::fs::read(&sibling_path, Some(&session_id))
            .unwrap()
            .unwrap(),
        sibling.as_bytes()
    );
    assert_eq!(
        crate::fs::staged_status(&session_id)
            .unwrap()
            .pending_writes
            .len(),
        1
    );
}

#[test]
fn go_workspace_rename_uses_the_staged_seed_package_before_writes() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let staged_declaration = "package other\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\nfunc Read(s *DocumentStore) int { return s.Count }\n";
    let seed_path = root.join("lab/store.go");
    fs::write(&seed_path, declaration).unwrap();
    fs::write(root.join("lab/use.go"), sibling).unwrap();
    let capability = build_index(&root);
    let session_id = format!(
        "go-rename-staged-{}",
        root.file_name().unwrap().to_string_lossy()
    );
    crate::fs::configure_session_root(&session_id, &root);
    crate::fs::set_mode(&session_id, crate::fs::FsMode::Staged, Some(&root)).unwrap();
    crate::fs::stage_write_or_none(
        BUILTIN,
        &seed_path,
        staged_declaration.as_bytes(),
        false,
        true,
        Some(&session_id),
    )
    .unwrap()
    .expect("seed edit was staged");
    let result = run(
        &capability.shared(),
        &[dict(&[
            (
                "symbol_ref",
                dict(&[
                    ("name", vm_string("DocumentStore")),
                    ("path", vm_string("lab/store.go")),
                    ("kind", vm_string("Type")),
                ]),
            ),
            ("new_name", vm_string("DocumentLibrary")),
            ("scope", vm_string("workspace")),
            ("backend", vm_string("syntax_tree")),
            ("session_id", vm_string(&session_id)),
        ])],
    )
    .unwrap();
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&seed_path).unwrap(), declaration);
    assert_eq!(
        fs::read_to_string(root.join("lab/use.go")).unwrap(),
        sibling
    );
    assert_eq!(
        crate::fs::read(&seed_path, Some(&session_id))
            .unwrap()
            .unwrap(),
        staged_declaration.as_bytes()
    );
    assert_eq!(
        crate::fs::staged_status(&session_id)
            .unwrap()
            .pending_writes
            .len(),
        1
    );
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
        ("path", vm_string(".")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol_ref, "renamed", "workspace");
    assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
    let VmValue::List(warnings) = field(&result, "warnings") else {
        panic!("warnings must be a list");
    };
    assert_eq!(warnings.len(), 2);
    for warning in warnings.iter() {
        assert_eq!(
            s(field(warning, "symbol")),
            s(field(warning, "qualified_name"))
        );
    }
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
        "use rename_fixture::old_api;\nfn main() {\n    let _ = old_api();\n}\n",
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
    assert!(main.contains("use rename_fixture::client_fetch;"));
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
