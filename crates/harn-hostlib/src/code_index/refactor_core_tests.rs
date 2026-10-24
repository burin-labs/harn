use super::*;

use std::fs;
use tempfile::tempdir;

#[test]
fn non_java_seed_selection_keeps_suffix_paths_and_legacy_name_ambiguity() {
    let (_, state) = index(&[(
        "src/lib/a.ts",
        "export function getById() { return 1; }\nexport const result = getById();\n",
    )]);
    for path in ["src/lib/a.ts", "lib/a.ts", "a.ts"] {
        assert!(
            matches!(
                resolve_seed_selected(&state.symbols, path, "getById", Some(1), Some(999), None),
                SeedLookup::One(_)
            ),
            "{path}"
        );
    }
    assert!(matches!(
        resolve_seed_selected(&state.symbols, "missing.ts", "getById", None, None, None),
        SeedLookup::One(_)
    ));
    assert!(
        matches!(
            resolve_seed(&state.symbols, "missing.ts", "getById", None, None),
            SeedLookup::One(_)
        ),
        "move and signature callers keep the legacy fallback"
    );
    let (_, state) = index(&[(
        "a.ts",
        "class First { load() {} } class Second { load() {} }",
    )]);
    let SeedLookup::Many(candidates) =
        resolve_seed_selected(&state.symbols, "a.ts", "load", Some(1), Some(40), None)
    else {
        panic!("a new column must not narrow the legacy non-Java resolver")
    };
    assert_eq!(candidates.len(), 2);
    assert!(matches!(
        resolve_seed_selected(
            &state.symbols,
            "a.ts",
            "Second.load",
            Some(1),
            Some(40),
            None
        ),
        SeedLookup::None
    ));
}

#[test]
fn refactor_proof_matches_written_images_and_is_absent_by_default() {
    use crate::test_support::*;
    use crate::{BuiltinRegistry, HostlibCapability};
    let cases = [
        ("rename_symbol", "export function oldName(x: number): number { return x + 1; }\nconst y = oldName(2);\n", vec![
            ("symbol_ref", dict(&[("name", vm_string("oldName")), ("path", vm_string("a.ts"))])),
            ("new_name", vm_string("newName")), ("scope", vm_string("workspace")), ("backend", vm_string("syntax_tree")),
        ]),
        ("move_symbol", "export function oldName(x: number): number { return x + 1; }\n", vec![
            ("symbol", vm_string("oldName")), ("path", vm_string("a.ts")), ("to_path", vm_string("b.ts")),
        ]),
        ("change_signature", "export function oldName(x: number, unused: number): number { return x + 1; }\nconst y = oldName(2, 3);\n", vec![
            ("symbol_ref", dict(&[("name", vm_string("oldName")), ("path", vm_string("a.ts"))])),
            ("params", VmValue::list(vec![dict(&[("name", vm_string("x")), ("from", vm_string("x"))])])),
        ]),
        ("extract_function", "export function oldName(x: number): number {\n    const doubled = x * 2;\n    return doubled + 1;\n}\n", vec![
            ("path", vm_string("a.ts")), ("start_line", VmValue::Int(2)), ("end_line", VmValue::Int(2)),
            ("new_name", vm_string("doubleValue")),
        ]),
    ];
    for (method, source, request) in cases {
        for enabled in [false, true] {
            let (dir, state) = index(&[("a.ts", source)]);
            let capability = crate::code_index::CodeIndexCapability::new();
            *capability.shared().lock().unwrap() = Some(state);
            let mut registry = BuiltinRegistry::new();
            capability.register_builtins(&mut registry);
            let mut request = request.clone();
            if enabled {
                request.push(("proof", VmValue::Bool(true)));
            }
            let result = (registry
                .find(&format!("hostlib_code_index_{method}"))
                .unwrap()
                .handler)(&[dict(&request)])
            .unwrap();
            assert_eq!(
                s(field(&result, "result")),
                "applied",
                "{method}: {result:?}"
            );
            assert!(matches!(field(&result, "applied"), VmValue::Bool(true)));
            let VmValue::Dict(result_dict) = &result else {
                panic!("result missing")
            };
            assert_eq!(result_dict.contains_key("proof"), enabled, "{method}");
            if !enabled {
                continue;
            }
            let proof = field(&result, "proof");
            let VmValue::List(files) = field(proof, "files") else {
                panic!("proof files missing")
            };
            let VmValue::List(touched) = field(&result, "touched_files") else {
                panic!("touched files missing")
            };
            assert_eq!(files.len(), touched.len(), "{method}");
            for (file, touched) in files.iter().zip(touched.iter()) {
                let path = s(field(file, "path"));
                assert_eq!(path, s(field(touched, "path")));
                assert_eq!(s(field(field(file, "syntax"), "status")), "passed");
                let written = fs::read_to_string(dir.path().join(&path)).unwrap();
                assert_eq!(
                    sha256_hex(written.as_bytes()),
                    s(field(touched, "after_sha256"))
                );
                let VmValue::List(sites) = field(file, "sites") else {
                    panic!("sites missing")
                };
                for site in sites.iter() {
                    let VmValue::Int(line) = field(site, "after_line") else {
                        panic!("line missing")
                    };
                    let from_anchor = written
                        .split('\n')
                        .skip(*line as usize - 1)
                        .collect::<Vec<_>>()
                        .join("\n");
                    assert!(
                        from_anchor.starts_with(&s(field(site, "after"))),
                        "{method}: {site:?}"
                    );
                }
            }
            assert_eq!(s(field(field(proof, "semantics"), "status")), "not_run");
        }
    }
}

fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, IndexState) {
    let dir = tempdir().unwrap();
    for (path, body) in files {
        let abs = dir.path().join(path);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, body).unwrap();
    }
    let (state, _outcome) = IndexState::build_from_root(dir.path());
    (dir, state)
}

fn seed(state: &IndexState, path: &str, name: &str, kind: NodeKind) -> NodeId {
    match resolve_seed(&state.symbols, path, name, None, Some(kind)) {
        SeedLookup::One(id) => id,
        SeedLookup::None => panic!("no seed `{name}` in {path}"),
        SeedLookup::Many(candidates) => panic!("ambiguous seed: {candidates:?}"),
    }
}

/// One line per site: `path:line:col kind [qualifier] | enclosing text`.
fn render(state: &IndexState, seed_id: NodeId) -> String {
    let found = reference_sites("test", state, seed_id, None).expect("query runs");
    assert!(found.skipped.is_empty(), "skipped: {:?}", found.skipped);
    let mut out = String::new();
    for site in &found.sites {
        let source = fs::read_to_string(state.root.join(&site.path)).unwrap();
        let qualifier = site
            .qualifier
            .as_deref()
            .map(|q| format!(" [{q}]"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}:{}:{} {}{} | {}\n",
            site.path,
            site.span.start_row + 1,
            site.span.start_col + 1,
            site.kind.as_str(),
            qualifier,
            source
                .get(site.enclosing.clone())
                .expect("enclosing span on a char boundary"),
        ));
    }
    out
}

#[test]
fn rust_reference_sites_classify_grouped_use_tree_and_call_shapes() {
    let (_dir, state) = index(&[
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub fn helper(x: u32) -> u32 {\n    x\n}\n\npub fn other() {}\n",
        ),
        (
            "src/b.rs",
            "use crate::a::{helper, other};\n\
             use crate::a;\n\
             \n\
             pub fn run(value: Thing) -> u32 {\n\
             \x20   let direct = helper(1);\n\
             \x20   let qualified = a::helper(2);\n\
             \x20   let method = value.helper();\n\
             \x20   let pointer = helper;\n\
             \x20   other();\n\
             \x20   direct + qualified + method + pointer(3)\n\
             }\n\
             // helper in a comment\n\
             const NOTE: &str = \"helper\";\n",
        ),
    ]);
    let helper = seed(&state, "src/a.rs", "helper", NodeKind::Function);
    assert_eq!(
        render(&state, helper),
        "src/b.rs:1:16 import | use crate::a::{helper, other};\n\
         src/b.rs:5:18 call | helper(1)\n\
         src/b.rs:6:24 qualified_call [a] | a::helper(2)\n\
         src/b.rs:7:24 method_call [value] | value.helper()\n\
         src/b.rs:8:19 value_reference | helper\n"
    );
}

#[test]
fn python_reference_sites_tell_module_calls_from_method_calls() {
    let (_dir, state) = index(&[
        ("util.py", "def fetch(url):\n    return url\n"),
        (
            "app.py",
            "import util\n\
             from util import fetch\n\
             \n\
             \n\
             def main(client):\n\
             \x20   fetch(\"a\")\n\
             \x20   util.fetch(\"b\")\n\
             \x20   client.fetch(\"c\")\n\
             \x20   handler = fetch\n\
             \x20   return handler\n",
        ),
    ]);
    let fetch = seed(&state, "util.py", "fetch", NodeKind::Function);
    assert_eq!(
        render(&state, fetch),
        "app.py:2:18 import | from util import fetch\n\
         app.py:6:5 call | fetch(\"a\")\n\
         app.py:7:10 qualified_call [util] | util.fetch(\"b\")\n\
         app.py:8:12 method_call [client] | client.fetch(\"c\")\n\
         app.py:9:15 value_reference | fetch\n"
    );
}

#[test]
fn typescript_reference_sites_cover_named_and_namespace_imports() {
    let (_dir, state) = index(&[
        (
            "src/math.ts",
            "export function sum(a: number, b: number): number {\n  return a + b;\n}\n",
        ),
        (
            "src/main.ts",
            "import { sum } from \"./math\";\n\
             import * as math from \"./math\";\n\
             \n\
             export function total(acc: { sum(x: number): number }): number {\n\
             \x20 const direct = sum(1, 2);\n\
             \x20 const qualified = math.sum(3, 4);\n\
             \x20 const method = acc.sum(5);\n\
             \x20 const ref = sum;\n\
             \x20 return direct + qualified + method + ref(0, 0);\n\
             }\n",
        ),
    ]);
    let sum = seed(&state, "src/math.ts", "sum", NodeKind::Function);
    assert_eq!(
        render(&state, sum),
        "src/main.ts:1:10 import | import { sum } from \"./math\";\n\
         src/main.ts:5:18 call | sum(1, 2)\n\
         src/main.ts:6:26 qualified_call [math] | math.sum(3, 4)\n\
         src/main.ts:7:22 method_call [acc] | acc.sum(5)\n\
         src/main.ts:8:15 value_reference | sum\n"
    );
}

#[test]
fn type_positions_are_type_references() {
    let (_dir, state) = index(&[
        ("src/lib.rs", "pub mod shapes;\npub mod user;\n"),
        (
            "src/shapes.rs",
            "pub struct Widget {\n    pub size: u32,\n}\n",
        ),
        (
            "src/user.rs",
            "use crate::shapes::Widget;\n\
             pub fn build() -> Widget {\n\
             \x20   Widget { size: 1 }\n\
             }\n",
        ),
    ]);
    let widget = seed(&state, "src/shapes.rs", "Widget", NodeKind::Type);
    assert_eq!(
        render(&state, widget),
        "src/user.rs:1:20 import | use crate::shapes::Widget;\n\
         src/user.rs:2:19 type_reference | Widget\n\
         src/user.rs:3:5 type_reference | Widget\n"
    );
}

#[test]
fn calls_inside_string_interpolations_are_reference_sites() {
    let (_dir, state) = index(&[
        ("util.py", "def fetch(url):\n    return url\n"),
        (
            "app.py",
            "from util import fetch\n\
             \n\
             \n\
             def main():\n\
             \x20   return f\"got {fetch('a')} not fetch\"\n",
        ),
        (
            "src/sum.ts",
            "export function sum(a: number): number {\n  return a;\n}\n",
        ),
        (
            "src/main.ts",
            "import { sum } from \"./sum\";\n\
             export const label = `total ${sum(1)} not sum`;\n",
        ),
    ]);
    let fetch = seed(&state, "util.py", "fetch", NodeKind::Function);
    assert_eq!(
        render(&state, fetch),
        "app.py:1:18 import | from util import fetch\n\
         app.py:5:19 call | fetch('a')\n"
    );
    let sum = seed(&state, "src/sum.ts", "sum", NodeKind::Function);
    assert_eq!(
        render(&state, sum),
        "src/main.ts:1:10 import | import { sum } from \"./sum\";\n\
         src/main.ts:2:31 call | sum(1)\n"
    );
}

#[test]
fn python_line_breaks_outside_brackets_do_not_parse() {
    // tree-sitter-python reads this as `b = a + print(b)` without an error.
    let broken = "def f(a):\n    b = a +\n    print(b)\n";
    assert_eq!(
        first_syntax_error(broken, Language::Python).as_deref(),
        Some("line 2 ends inside a statement without brackets or `\\`")
    );
    // Every header line is checked, including clauses nested under their
    // statement.
    for header in [
        "if a and\nb:\n    pass\n",
        "if a:\n    pass\nelif a and\nb:\n    pass\n",
        "try:\n    pass\nexcept a or\nb:\n    pass\n",
        "match a:\n    case 1 if a and\nb:\n        pass\n",
        // A backslash joins lines only when the break follows it directly;
        // tree-sitter itself rejects trailing whitespace after it.
        "x = 1 + \\   \n    2\n",
        "x = 1 + \\\n\n    2\n",
    ] {
        assert!(
            first_syntax_error(header, Language::Python).is_some(),
            "{header}"
        );
    }
    // Every legal way to continue a line.
    let valid = "import os\n\
                 from os import (\n    path,\n    sep,\n)\n\
                 \n\
                 \n\
                 @staticmethod\n\
                 def f(a,\n      b):\n\
                 \x20   # body comment\n\
                 \x20   total = (a +\n             b)\n\
                 \x20   more = a + \\\n        b\n\
                 \x20   text = \"\"\"one\ntwo\"\"\"\n\
                 \x20   items = [\n        a,  # first\n        b,\n    ]\n\
                 \x20   if a:\n        return total\n    elif (b and\n          a):\n        pass\n    else:\n        return more, text, items\n\
                 \x20   try:\n        pass\n    except (ValueError,\n            KeyError):\n        pass\n    else:\n        pass\n    finally:\n        pass\n\
                 \x20   for x in items:\n        pass\n    else:\n        pass\n\
                 \n\
                 \n\
                 class C(\n    object,\n):\n    x = {\"k\": 1,\n         \"j\": 2}\n";
    assert_eq!(first_syntax_error(valid, Language::Python), None);
}
