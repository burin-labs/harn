//! Seeds declared inside a body: only their scope is rewritten, and any
//! spelling in that scope the proof cannot bind refuses the whole rename.
// Fixture sources spell implicit format captures on purpose.
#![allow(clippy::literal_string_with_formatting_args)]

use super::*;

/// One file, one seed on `line`, and the file text the rename must leave.
struct Case {
    path: &'static str,
    source: &'static str,
    line: i64,
    kind: &'static str,
    expected: Outcome,
}

enum Outcome {
    /// The whole file after the rename.
    Applied(&'static str),
    /// `ambiguous_symbol` with this detail, and an unchanged file.
    Refused(&'static str),
}
use Outcome::{Applied, Refused};

fn run_case(case: &Case) {
    let dir = tempdir().unwrap();
    let path = dir.path().join(case.path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, case.source).unwrap();
    // A same-named file-level declaration in another file must stay untouched.
    let decoy_path = case.path.replace("main", "decoy");
    let decoy_source = match case.kind {
        "Type" if case.path.ends_with(".rs") => "pub struct Input;\n",
        "Type" => "export type Input = string;\n",
        _ if case.path.ends_with(".rs") => "pub fn helper() -> i32 { 9 }\n",
        _ if case.path.ends_with(".py") => "def helper():\n    return 9\n",
        _ => "export const helper = () => 9;\n",
    };
    fs::write(dir.path().join(&decoy_path), decoy_source).unwrap();
    let capability = build_index(dir.path());
    let name = if case.kind == "Type" {
        "Input"
    } else {
        "helper"
    };
    let symbol = dict(&[
        ("name", vm_string(name)),
        ("path", vm_string(case.path)),
        ("line", VmValue::Int(case.line)),
        ("kind", vm_string(case.kind)),
    ]);
    let result = rename(&capability, symbol, "renamed", "workspace");
    let after = fs::read_to_string(&path).unwrap();
    match case.expected {
        Applied(expected) => {
            assert_eq!(
                s(field(&result, "result")),
                "applied",
                "{}: {result:?}",
                case.source
            );
            assert_eq!(after, expected, "{}", case.source);
        }
        Refused(detail) => {
            assert_eq!(
                s(field(&result, "result")),
                "ambiguous_symbol",
                "{}: {result:?}",
                case.source
            );
            assert!(
                s(field(&result, "details")).contains(detail),
                "{}: {result:?}",
                case.source
            );
            assert_eq!(after, case.source);
        }
    }
    assert_eq!(
        fs::read_to_string(dir.path().join(&decoy_path)).unwrap(),
        decoy_source
    );
}

#[test]
fn a_body_local_seed_renames_only_its_own_scope() {
    let cases = [
        // TypeScript callback const beside a same-named sibling, file-level
        // const and object key.
        Case {
            path: "main.test.ts",
            source: "export const helper = (n: number) => n;\ndescribe('a', () => {\n  const helper = (input: number) => input + 1;\n  const pick = { helper: 1 };\n  type Out = ReturnType<typeof helper>;\n  expect(helper(1)).toBe(pick.helper);\n});\ndescribe('b', () => {\n  const helper = (input: number) => input * 2;\n  helper(3);\n});\nexport const used = helper(0);\n",
            line: 3,
            kind: "Function",
            expected: Applied("export const helper = (n: number) => n;\ndescribe('a', () => {\n  const renamed = (input: number) => input + 1;\n  const pick = { helper: 1 };\n  type Out = ReturnType<typeof renamed>;\n  expect(renamed(1)).toBe(pick.helper);\n});\ndescribe('b', () => {\n  const helper = (input: number) => input * 2;\n  helper(3);\n});\nexport const used = helper(0);\n"),
        },
        // TypeScript function declaration directly in a function body.
        Case {
            path: "main.ts",
            source: "export function helper() { return 0; }\nexport function outer() {\n  function helper() { return 1; }\n  return helper();\n}\n",
            line: 3,
            kind: "Function",
            expected: Applied("export function helper() { return 0; }\nexport function outer() {\n  function renamed() { return 1; }\n  return renamed();\n}\n"),
        },
        // TypeScript block-local type alias.
        Case {
            path: "main.ts",
            source: "type Input = string;\nexport function a() {\n  type Input = number;\n  const x: Input = 1;\n  return x;\n}\nexport function b() {\n  type Input = boolean;\n  const y: Input = true;\n  return y;\n}\n",
            line: 3,
            kind: "Type",
            expected: Applied("type Input = string;\nexport function a() {\n  type renamed = number;\n  const x: renamed = 1;\n  return x;\n}\nexport function b() {\n  type Input = boolean;\n  const y: Input = true;\n  return y;\n}\n"),
        },
        // Rust item in a function body, beside a same-named field.
        Case {
            path: "src/main.rs",
            source: "pub fn helper() -> i32 { 0 }\npub struct Point { pub helper: i32 }\npub fn outer(p: Point) -> i32 {\n    fn helper() -> i32 { 1 }\n    helper() + p.helper\n}\npub fn other() -> i32 { helper() }\n",
            line: 4,
            kind: "Function",
            expected: Applied("pub fn helper() -> i32 { 0 }\npub struct Point { pub helper: i32 }\npub fn outer(p: Point) -> i32 {\n    fn renamed() -> i32 { 1 }\n    renamed() + p.helper\n}\npub fn other() -> i32 { helper() }\n"),
        },
        // Rust private item of an inline test module.
        Case {
            path: "src/main.rs",
            source: "pub fn helper() -> i32 { 0 }\n#[cfg(test)]\nmod tests {\n    fn helper() -> i32 { 2 }\n    #[test]\n    fn uses() {\n        assert_eq!(helper(), 2);\n    }\n}\n",
            line: 4,
            kind: "Function",
            expected: Applied("pub fn helper() -> i32 { 0 }\n#[cfg(test)]\nmod tests {\n    fn renamed() -> i32 { 2 }\n    #[test]\n    fn uses() {\n        assert_eq!(renamed(), 2);\n    }\n}\n"),
        },
        // Python nested def beside an attribute and a keyword of that name.
        Case {
            path: "main.py",
            source: "def helper():\n    return 0\n\n\ndef outer(value, obj, call):\n    if value:\n        def helper():\n            return value\n    return helper() + obj.helper + call(helper=1)\n",
            line: 7,
            kind: "Function",
            expected: Applied("def helper():\n    return 0\n\n\ndef outer(value, obj, call):\n    if value:\n        def renamed():\n            return value\n    return renamed() + obj.helper + call(helper=1)\n"),
        },
    ];
    for case in &cases {
        run_case(case);
    }
}

#[test]
fn a_body_local_seed_refuses_a_shadowing_or_unbound_spelling_in_its_scope() {
    let cases = [
        // Inner parameter shadows the seed.
        Case {
            path: "main.ts",
            source: "export function outer() {\n  const helper = (n: number) => n;\n  return [1].map((helper) => helper);\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // Shorthand property both names a key and reads the binding.
        Case {
            path: "main.ts",
            source: "export function outer() {\n  const helper = (n: number) => n;\n  return { helper };\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // A function declaration in a nested block hoists in sloppy code.
        Case {
            path: "main.js",
            source: "export function outer(flag) {\n  if (flag) {\n    function helper() { return 1; }\n  }\n  return helper();\n}\n",
            line: 3,
            kind: "Function",
            expected: Refused("separate declarations"),
        },
        // A same-named parameter shares the function's scope.
        Case {
            path: "main.ts",
            source: "export function outer(helper: number) {\n  function helper() { return 1; }\n  return helper();\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // Namespace members merge across declarations.
        Case {
            path: "main.ts",
            source: "namespace Lab {\n  const helper = () => 1;\n  export const used = helper();\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("separate declarations"),
        },
        // A generic parameter shadows the local type.
        Case {
            path: "main.ts",
            source: "export function a() {\n  type Input = number;\n  function g<Input>(x: Input): Input { return x; }\n  const y: Input = 1;\n  return g(y);\n}\n",
            line: 2,
            kind: "Type",
            expected: Refused("unresolved binding"),
        },
        // A let binding shadows a block item.
        Case {
            path: "src/main.rs",
            source: "pub fn outer() -> i32 {\n    fn helper() -> i32 { 1 }\n    let helper = 2;\n    helper\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // A user macro in the scope can declare a competing item.
        Case {
            path: "src/main.rs",
            source: "pub fn outer() -> i32 {\n    fn helper() -> i32 { 1 }\n    {\n        my_macros::declare!();\n        helper()\n    }\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("is incomplete (dynamic_name_access)"),
        },
        // A child module can name a private parent item.
        Case {
            path: "src/main.rs",
            source: "mod tests {\n    fn helper() -> i32 { 2 }\n    mod child;\n    pub fn uses() -> i32 { helper() }\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("may reference it"),
        },
        // A public module item is visible outside the module.
        Case {
            path: "src/main.rs",
            source: "pub fn helper() -> i32 { 0 }\npub mod tests {\n    pub fn helper() -> i32 { 2 }\n    pub fn uses() -> i32 { helper() }\n}\n",
            line: 3,
            kind: "Function",
            expected: Refused("separate declarations"),
        },
        // `nonlocal` rebinds the seed from a nested function.
        Case {
            path: "main.py",
            source: "def outer():\n    def helper():\n        return 1\n    def inner():\n        nonlocal helper\n        helper = None\n    return helper()\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // A class body inside the scope consults its metaclass namespace.
        Case {
            path: "main.py",
            source: "def outer(meta):\n    def helper():\n        return 1\n    class Inner(metaclass=meta):\n        value = helper()\n    return Inner\n",
            line: 2,
            kind: "Function",
            expected: Refused("unresolved binding"),
        },
        // A method is not a local binding of the class body's function.
        Case {
            path: "main.py",
            source: "def outer():\n    class Inner:\n        def helper(self):\n            return 1\n    return Inner().helper()\n",
            line: 3,
            kind: "Function",
            expected: Refused("separate declarations"),
        },
        // An enum member is an unqualified name inside its enum body.
        Case {
            path: "main.ts",
            source: "export function outer() {\n  const helper = () => 3;\n  enum E { helper = 2, double = helper * 2 }\n  return helper() + E.double;\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("declares a member of that name"),
        },
        // The same member with a quoted name.
        Case {
            path: "main.ts",
            source: "export function outer() {\n  const helper = () => 3;\n  enum E { 'helper' = 2, double = helper * 2 }\n  return helper() + E.double;\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("declares a member of that name"),
        },
        // A macro defined outside the scope expands to the seed at the call site.
        Case {
            path: "src/main.rs",
            source: "macro_rules! make { () => { Input } }\npub fn outer() {\n    struct Input;\n    let _v = make!();\n}\n",
            line: 3,
            kind: "Type",
            expected: Refused("may expand to it"),
        },
        // The same call nested in a std macro's token tree.
        Case {
            path: "src/main.rs",
            source: "macro_rules! make { () => { Input } }\npub fn outer() {\n    struct Input;\n    let _v = vec![make!()];\n}\n",
            line: 3,
            kind: "Type",
            expected: Refused("may expand to it"),
        },
        // A format string captures the seed implicitly.
        Case {
            path: "src/main.rs",
            source: "pub fn outer() -> String {\n    fn helper() -> i32 { 1 }\n    let _ = helper();\n    format!(\"{helper:p}\")\n}\n",
            line: 2,
            kind: "Function",
            expected: Refused("may capture `helper` implicitly"),
        },
    ];
    for case in &cases {
        run_case(case);
    }
}

#[test]
fn a_file_level_rust_seed_refuses_an_implicit_format_capture() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("src/main.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = "pub fn helper() -> i32 { 1 }\npub fn show() -> String {\n    let _ = helper();\n    format!(\"{helper:p}\")\n}\n";
    fs::write(&path, source).unwrap();
    let capability = build_index(dir.path());
    let symbol = dict(&[
        ("name", vm_string("helper")),
        ("path", vm_string("src/main.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol, "renamed", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert!(
        s(field(&result, "details")).contains("may capture `helper` implicitly"),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
    // Without the capture the same file renames.
    let plain = source.replace("format!(\"{helper:p}\")", "String::new()");
    fs::write(&path, &plain).unwrap();
    let capability = build_index(dir.path());
    let symbol = dict(&[
        ("name", vm_string("helper")),
        ("path", vm_string("src/main.rs")),
        ("kind", vm_string("Function")),
    ]);
    let result = rename(&capability, symbol, "renamed", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
}
