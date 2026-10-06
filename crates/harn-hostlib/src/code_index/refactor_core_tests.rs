use super::*;

use std::fs;
use tempfile::tempdir;

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
    let header = "if a and\nb:\n    pass\n";
    assert!(first_syntax_error(header, Language::Python).is_some());
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
                 \x20   if a:\n        return total\n    else:\n        return more, text, items\n\
                 \n\
                 \n\
                 class C(\n    object,\n):\n    x = {\"k\": 1,\n         \"j\": 2}\n";
    assert_eq!(first_syntax_error(valid, Language::Python), None);
}
