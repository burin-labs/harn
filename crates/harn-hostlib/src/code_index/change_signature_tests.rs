use super::*;
use crate::test_support::*;

use std::fs;
use tempfile::tempdir;

use crate::code_index::CodeIndexCapability;

fn string(s: &str) -> VmValue {
    VmValue::string(s)
}

fn text(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn items(value: &VmValue) -> Vec<VmValue> {
    match value {
        VmValue::List(list) => list.iter().cloned().collect(),
        other => panic!("expected list, got {other:?}"),
    }
}

/// A workspace on disk with a built index.
struct Workspace {
    dir: tempfile::TempDir,
    capability: CodeIndexCapability,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempdir().unwrap();
        for (path, body) in files {
            let abs = dir.path().join(path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        let capability = CodeIndexCapability::new();
        let (state, _outcome) = IndexState::build_from_root(dir.path());
        *capability.shared().lock().unwrap() = Some(state);
        Self { dir, capability }
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    fn snapshot(&self) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = walk(self.dir.path())
            .into_iter()
            .map(|path| {
                let rel = path
                    .strip_prefix(self.dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                (rel, fs::read_to_string(&path).unwrap())
            })
            .collect();
        files.sort();
        files
    }

    /// Call the builtin. `params` entries are `(key, value)` pairs.
    fn change(&self, name: &str, path: &str, params: &[&[(&str, &str)]]) -> VmValue {
        self.change_with(name, path, params, &[])
    }

    fn change_with(
        &self,
        name: &str,
        path: &str,
        params: &[&[(&str, &str)]],
        extra: &[(&str, VmValue)],
    ) -> VmValue {
        let params: Vec<VmValue> = params
            .iter()
            .map(|entry| {
                let pairs: Vec<(&str, VmValue)> =
                    entry.iter().map(|(k, v)| (*k, string(v))).collect();
                dict(&pairs)
            })
            .collect();
        let mut request = vec![
            (
                "symbol_ref",
                dict(&[("name", string(name)), ("path", string(path))]),
            ),
            ("params", VmValue::list(params)),
        ];
        request.extend(extra.iter().cloned());
        run(&self.capability.shared(), &[dict(&request)]).expect("change_signature runs")
    }

    /// Call the builtin and return the error it raises.
    fn change_err(&self, name: &str, path: &str, params: &[&[(&str, &str)]]) -> String {
        let params: Vec<VmValue> = params
            .iter()
            .map(|entry| {
                let pairs: Vec<(&str, VmValue)> =
                    entry.iter().map(|(k, v)| (*k, string(v))).collect();
                dict(&pairs)
            })
            .collect();
        let request = dict(&[
            (
                "symbol_ref",
                dict(&[("name", string(name)), ("path", string(path))]),
            ),
            ("params", VmValue::list(params)),
        ]);
        match run(&self.capability.shared(), &[request]) {
            Ok(value) => panic!("expected an error, got {value:?}"),
            Err(err) => err.to_string(),
        }
    }

    /// Call the builtin and assert it refused with `tag`, leaving every file
    /// byte-identical. Returns the response.
    fn refuse(&self, tag: &str, name: &str, path: &str, params: &[&[(&str, &str)]]) -> VmValue {
        let before = self.snapshot();
        let result = self.change(name, path, params);
        assert_eq!(
            text(field(&result, "result")),
            tag,
            "details: {}",
            text(field(&result, "details"))
        );
        assert!(items(field(&result, "touched_files")).is_empty());
        assert_eq!(self.snapshot(), before, "a refusal changed files");
        result
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn assert_applied(result: &VmValue, call_sites: i64) {
    assert_eq!(
        text(field(result, "result")),
        "applied",
        "details: {}; sites: {:?}",
        text(field(result, "details")),
        field(result, "sites")
    );
    assert!(matches!(field(result, "applied"), VmValue::Bool(true)));
    assert_eq!(text(field(result, "scope")), "workspace");
    assert!(matches!(field(result, "match_count"), VmValue::Int(n) if *n > call_sites));
    for file in items(field(result, "touched_files")) {
        assert_eq!(text(field(&file, "before_sha256")).len(), 64);
    }
    assert!(
        matches!(field(result, "call_sites_updated"), VmValue::Int(n) if *n == call_sites),
        "call_sites_updated: {:?}",
        field(result, "call_sites_updated")
    );
}

/// `path:line kind` for every reported site.
fn site_lines(result: &VmValue) -> Vec<String> {
    items(field(result, "sites"))
        .iter()
        .map(|site| {
            format!(
                "{}:{} {}",
                text(field(site, "path")),
                match field(site, "line") {
                    VmValue::Int(n) => *n,
                    other => panic!("line {other:?}"),
                },
                text(field(site, "kind"))
            )
        })
        .collect()
}

// === Rust ===

const RUST_JOBS: &str = "pub struct Job {
    pub code: String,
}

pub fn render(job: &Job, verbose: bool) -> String {
    if verbose { format!(\"job:{}!\", job.code) } else { format!(\"job:{}\", job.code) }
}
";

#[test]
fn rust_cross_file_plain_qualified_and_macro_calls_are_remapped() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod api;\npub mod jobs;\n"),
        ("src/jobs.rs", RUST_JOBS),
        (
            "src/api.rs",
            "use crate::jobs;\n\
             use crate::jobs::{render, Job};\n\
             \n\
             pub fn show(job: &Job) -> String {\n\
             \x20   // render(job, true) stays a comment\n\
             \x20   let a = render(job, false);\n\
             \x20   let b = jobs::render(job, true);\n\
             \x20   format!(\"{} {} {}\", a, b, render(job, false))\n\
             }\n",
        ),
        (
            "tests/integration.rs",
            "use dispatch::jobs::{render, Job};\n\
             \n\
             #[test]\n\
             fn renders() {\n\
             \x20   let job = Job { code: \"a\".into() };\n\
             \x20   assert_eq!(render(&job, false), \"job:a\");\n\
             }\n",
        ),
    ]);
    let result = ws.change(
        "render",
        "src/jobs.rs",
        &[
            &[
                ("name", "prefix"),
                ("type", "&str"),
                ("call_value", "\"job\""),
            ],
            &[("name", "job")],
            &[("name", "verbose")],
        ],
    );
    assert_applied(&result, 4);
    assert_eq!(
        site_lines(&result),
        [
            "src/api.rs:6 call",
            "src/api.rs:7 qualified_call",
            "src/api.rs:8 call",
            "tests/integration.rs:6 call",
        ]
    );
    assert_eq!(
        ws.read("src/jobs.rs"),
        RUST_JOBS.replace(
            "render(job: &Job, verbose: bool)",
            "render(prefix: &str, job: &Job, verbose: bool)"
        )
    );
    assert_eq!(
        ws.read("src/api.rs"),
        "use crate::jobs;\n\
         use crate::jobs::{render, Job};\n\
         \n\
         pub fn show(job: &Job) -> String {\n\
         \x20   // render(job, true) stays a comment\n\
         \x20   let a = render(\"job\", job, false);\n\
         \x20   let b = jobs::render(\"job\", job, true);\n\
         \x20   format!(\"{} {} {}\", a, b, render(\"job\", job, false))\n\
         }\n"
    );
    assert!(ws
        .read("tests/integration.rs")
        .contains("assert_eq!(render(\"job\", &job, false), \"job:a\");"));
}

#[test]
fn rust_method_calls_keep_the_receiver_and_rename_body_uses() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod shapes;\npub mod user;\n"),
        (
            "src/shapes.rs",
            "pub struct Widget {\n\
             \x20   pub size: u32,\n\
             }\n\
             \n\
             impl Widget {\n\
             \x20   pub fn area(&self, scale: u32) -> u32 {\n\
             \x20       self.size * scale\n\
             \x20   }\n\
             \n\
             \x20   pub fn double(&self) -> u32 {\n\
             \x20       self.area(2)\n\
             \x20   }\n\
             }\n",
        ),
        (
            "src/user.rs",
            "use crate::shapes::Widget;\n\
             \n\
             pub fn total(w: &Widget) -> u32 {\n\
             \x20   w.area(3) + Widget::area(w, 4)\n\
             }\n",
        ),
    ]);
    // The index's qualified name selects the method too.
    let result = ws.change(
        "Widget.area",
        "src/shapes.rs",
        &[
            &[("name", "factor"), ("from", "scale")],
            &[("name", "offset"), ("type", "u32"), ("call_value", "0")],
        ],
    );
    assert_applied(&result, 3);
    let shapes = ws.read("src/shapes.rs");
    assert!(shapes.contains("pub fn area(&self, factor: u32, offset: u32) -> u32 {"));
    assert!(shapes.contains("self.size * factor\n"));
    assert!(shapes.contains("self.area(2, 0)"));
    assert!(ws
        .read("src/user.rs")
        .contains("w.area(3, 0) + Widget::area(w, 4, 0)"));
}

#[test]
fn rust_value_reference_refuses_and_lists_the_site() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod api;\npub mod jobs;\n"),
        ("src/jobs.rs", RUST_JOBS),
        (
            "src/api.rs",
            "use crate::jobs::{render, Job};\n\
             \n\
             pub fn all(jobs: &[Job]) -> Vec<String> {\n\
             \x20   let one = render(&jobs[0], true);\n\
             \x20   let f: fn(&Job, bool) -> String = render;\n\
             \x20   vec![one, f(&jobs[0], false)]\n\
             }\n",
        ),
    ]);
    let result = ws.refuse(
        "value_reference",
        "render",
        "src/jobs.rs",
        &[
            &[("name", "job")],
            &[("name", "verbose")],
            &[
                ("name", "prefix"),
                ("type", "&str"),
                ("call_value", "\"job\""),
            ],
        ],
    );
    assert_eq!(site_lines(&result), ["src/api.rs:5 value_reference"]);
}

#[test]
fn rust_call_that_cannot_be_remapped_refuses() {
    // Rust has no spread syntax; the call shape it cannot remap is an
    // argument count that does not match the declaration.
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod api;\npub mod jobs;\n"),
        ("src/jobs.rs", RUST_JOBS),
        (
            "src/api.rs",
            "use crate::jobs::{render, Job};\n\
             pub fn one(job: &Job) -> String {\n\
             \x20   render(job)\n\
             }\n",
        ),
    ]);
    let result = ws.refuse(
        "unsupported_call_site",
        "render",
        "src/jobs.rs",
        &[&[("name", "verbose")], &[("name", "job")]],
    );
    assert_eq!(site_lines(&result), ["src/api.rs:3 unsupported_call_site"]);
}

#[test]
fn removing_a_parameter_the_body_reads_refuses() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod jobs;\n"),
        ("src/jobs.rs", RUST_JOBS),
    ]);
    let result = ws.refuse(
        "parameter_in_use",
        "render",
        "src/jobs.rs",
        &[&[("name", "job")]],
    );
    assert_eq!(site_lines(&result), ["src/jobs.rs:6 parameter_use"]);
    let names: Vec<String> = items(field(&result, "parameters_before"))
        .iter()
        .map(|p| text(field(p, "name")))
        .collect();
    assert_eq!(names, ["job", "verbose"]);
}

fn symbol_at(name: &str, path: &str, line: i64) -> [(&'static str, VmValue); 1] {
    [(
        "symbol_ref",
        dict(&[
            ("name", string(name)),
            ("path", string(path)),
            ("line", VmValue::Int(line)),
        ]),
    )]
}

/// Change `name` at `path:line` and assert a refusal with `tag` that
/// leaves every file byte-identical.
fn refuse_at(ws: &Workspace, tag: &str, (name, path, line): (&str, &str, i64)) -> VmValue {
    let before = ws.snapshot();
    let result = ws.change_with(name, path, APPEND_OFFSET, &symbol_at(name, path, line));
    assert_eq!(
        text(field(&result, "result")),
        tag,
        "details: {}",
        text(field(&result, "details"))
    );
    assert_eq!(ws.snapshot(), before, "a refusal changed files");
    result
}

const APPEND_OFFSET: &[&[(&str, &str)]] = &[
    &[("name", "scale")],
    &[("name", "offset"), ("type", "u32"), ("call_value", "0")],
];

const RUST_SHAPES: &str = "pub trait Shape {
    fn area(&self, scale: u32) -> u32;
    fn twice(&self) -> u32 {
        self.area(2) * 2
    }
}
pub struct Square;
impl Shape for Square {
    fn area(&self, scale: u32) -> u32 {
        scale
    }
}
";

/// A trait method changes with every impl of it, from any member.
#[test]
fn rust_trait_family_changes_every_impl_and_call() {
    let files: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"geo-kit\"\n"),
        ("src/lib.rs", "pub mod shapes;\npub mod round;\n"),
        ("src/shapes.rs", RUST_SHAPES),
        (
            "src/round.rs",
            "use crate::shapes::Shape;\n\
             pub struct Circle;\n\
             impl Shape for Circle {\n\
             \x20   fn area(&self, _scale: u32) -> u32 {\n\
             \x20       3\n\
             \x20   }\n\
             }\n\
             pub fn total(c: &Circle) -> u32 {\n\
             \x20   c.area(1) + Shape::area(c, 4)\n\
             }\n",
        ),
        (
            "tests/it.rs",
            "use geo_kit::shapes::{Shape, Square};\n\
             #[test]\n\
             fn squares() {\n\
             \x20   assert_eq!(Square.area(5), 5);\n\
             }\n",
        ),
    ];
    // The index holds no body-less trait signature, so seeds are impls.
    for (seed_path, seed_line, kept) in
        [("src/shapes.rs", 9, "scale"), ("src/round.rs", 4, "_scale")]
    {
        let ws = Workspace::new(files);
        let result = ws.change_with(
            "area",
            seed_path,
            &[
                &[("name", kept)],
                &[("name", "offset"), ("type", "u32"), ("call_value", "0")],
            ],
            &symbol_at("area", seed_path, seed_line),
        );
        assert_applied(&result, 4);
        assert_eq!(
            ws.read("src/shapes.rs"),
            RUST_SHAPES
                .replace(
                    "area(&self, scale: u32)",
                    "area(&self, scale: u32, offset: u32)"
                )
                .replace("self.area(2)", "self.area(2, 0)")
        );
        let round = ws.read("src/round.rs");
        assert!(
            round.contains("fn area(&self, _scale: u32, offset: u32) -> u32"),
            "{round}"
        );
        assert!(
            round.contains("c.area(1, 0) + Shape::area(c, 4, 0)"),
            "{round}"
        );
        assert!(ws.read("tests/it.rs").contains("Square.area(5, 0)"));
    }
}

/// Any member the workspace cannot rewrite soundly keeps the refusal.
#[test]
fn rust_trait_family_refuses_external_macro_and_inherent_members() {
    let base: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"geo\"\n"),
        ("src/lib.rs", "pub mod shapes;\npub mod extra;\n"),
        ("src/shapes.rs", RUST_SHAPES),
    ];
    let cases: &[(&str, &str)] = &[
        // An impl of a same-named trait from another crate.
        (
            "use other::Shape;\npub struct Hex;\nimpl Shape for Hex {\n    fn area(&self, scale: u32) -> u32 {\n        scale\n    }\n}\n",
            "src/extra.rs:3 supertype",
        ),
        // An impl written by a macro.
        (
            "macro_rules! forward {\n    () => {\n        fn area(&self, scale: u32) -> u32 { scale }\n    };\n}\n",
            "src/extra.rs:3 macro",
        ),
        // An inherent method calls by name cannot tell apart.
        (
            "pub struct Hex;\nimpl Hex {\n    pub fn area(&self, scale: u32) -> u32 {\n        scale\n    }\n}\n",
            "src/extra.rs:3 declaration",
        ),
    ];
    for (extra, blocking) in cases {
        let mut files = base.to_vec();
        files.push(("src/extra.rs", extra));
        let ws = Workspace::new(&files);
        let result = refuse_at(&ws, "overrides_present", ("area", "src/shapes.rs", 9));
        assert_eq!(site_lines(&result), [*blocking], "{extra}");
    }
    // A method of a trait declared outside the workspace.
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod show;\n"),
        (
            "src/show.rs",
            "use std::fmt;\npub struct P;\nimpl fmt::Display for P {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        f.write_str(\"p\")\n    }\n}\n",
        ),
    ]);
    let result = ws.change_with(
        "fmt",
        "src/show.rs",
        &[
            &[("name", "f")],
            &[("name", "extra"), ("type", "u8"), ("call_value", "0")],
        ],
        &symbol_at("fmt", "src/show.rs", 4),
    );
    assert_eq!(text(field(&result, "result")), "overrides_present");
    assert_eq!(site_lines(&result), ["src/show.rs:3 supertype"]);
}

#[test]
fn unchanged_list_and_missing_call_value_raise_instead_of_no_ops() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod jobs;\n"),
        ("src/jobs.rs", RUST_JOBS),
    ]);
    let before = ws.snapshot();
    let same = ws.change_err(
        "render",
        "src/jobs.rs",
        &[&[("name", "job")], &[("name", "verbose")]],
    );
    assert!(same.contains("nothing would change"), "{same}");
    let missing = ws.change_err(
        "render",
        "src/jobs.rs",
        &[
            &[("name", "job")],
            &[("name", "verbose")],
            &[("name", "prefix"), ("type", "&str")],
        ],
    );
    assert!(missing.contains("call_value"), "{missing}");
    assert_eq!(ws.snapshot(), before);
}

#[test]
fn dry_run_plans_without_writing() {
    let ws = Workspace::new(&[
        (
            "src/lib.rs",
            "pub mod jobs;\npub fn go(j: &jobs::Job) -> String { jobs::render(j, true) }\n",
        ),
        ("src/jobs.rs", RUST_JOBS),
    ]);
    let before = ws.snapshot();
    let result = ws.change_with(
        "render",
        "src/jobs.rs",
        &[&[("name", "job")]],
        &[("dry_run", VmValue::Bool(true))],
    );
    // `verbose` is read by the body, so the dry run reports the refusal a
    // real run would hit.
    assert_eq!(text(field(&result, "result")), "parameter_in_use");
    let result = ws.change_with(
        "render",
        "src/jobs.rs",
        &[&[("name", "verbose")], &[("name", "job")]],
        &[("dry_run", VmValue::Bool(true))],
    );
    assert_eq!(text(field(&result, "result")), "applied");
    assert!(matches!(field(&result, "dry_run"), VmValue::Bool(true)));
    assert!(matches!(field(&result, "applied"), VmValue::Bool(false)));
    assert_eq!(items(field(&result, "touched_files")).len(), 2);
    assert_eq!(ws.snapshot(), before);
}

// === TypeScript ===

const TS_ORDERS: &str = "export interface Order {
  code: string
}

export function renderOrder(order: Order, loud: boolean = false): string {
  return loud ? `ORDER:${order.code}` : `order:${order.code}`
}
";

#[test]
fn typescript_named_and_namespace_calls_are_remapped() {
    let ws = Workspace::new(&[
        ("src/orders.ts", TS_ORDERS),
        (
            "src/api.ts",
            "import { renderOrder, type Order } from \"./orders\"\n\
             import * as orders from \"./orders\"\n\
             \n\
             export function dashboard(list: Order[]): string[] {\n\
             \x20 return list.map((o) => `${renderOrder(o)} ${orders.renderOrder(o, true)}`)\n\
             }\n\
             export { renderOrder }\n",
        ),
    ]);
    let result = ws.change(
        "renderOrder",
        "src/orders.ts",
        &[
            &[("name", "order")],
            &[
                ("name", "prefix"),
                ("type", "string"),
                ("call_value", "\"order\""),
            ],
            &[("name", "loud")],
        ],
    );
    assert_applied(&result, 2);
    assert!(ws
        .read("src/orders.ts")
        .contains("export function renderOrder(order: Order, prefix: string, loud: boolean = false): string {"));
    assert!(ws
        .read("src/api.ts")
        .contains("`${renderOrder(o, \"order\")} ${orders.renderOrder(o, \"order\", true)}`"));
}

#[test]
fn typescript_method_calls_are_remapped() {
    let ws = Workspace::new(&[
        (
            "src/format.ts",
            "export class Formatter {\n\
             \x20 fmt(value: number, width: number): string {\n\
             \x20   return String(value).padStart(width)\n\
             \x20 }\n\
             \x20 twice(value: number): string {\n\
             \x20   return this.fmt(value, 2) + this.fmt(value, 4)\n\
             \x20 }\n\
             }\n",
        ),
        (
            "src/use.ts",
            "import { Formatter } from \"./format\"\n\
             export const out = new Formatter().fmt(1, 3)\n",
        ),
    ]);
    let result = ws.change(
        "fmt",
        "src/format.ts",
        &[&[("name", "width")], &[("name", "value")]],
    );
    assert_applied(&result, 3);
    let format = ws.read("src/format.ts");
    assert!(format.contains("fmt(width: number, value: number): string {"));
    assert!(format.contains("this.fmt(2, value) + this.fmt(4, value)"));
    assert!(ws.read("src/use.ts").contains("new Formatter().fmt(3, 1)"));
}

#[test]
fn typescript_value_reference_refuses() {
    let ws = Workspace::new(&[
        ("src/orders.ts", TS_ORDERS),
        (
            "src/api.ts",
            "import { renderOrder, type Order } from \"./orders\"\n\
             export const all = (list: Order[]) => list.map(renderOrder)\n",
        ),
    ]);
    let result = ws.refuse(
        "value_reference",
        "renderOrder",
        "src/orders.ts",
        &[
            &[("name", "order")],
            &[("name", "prefix"), ("call_value", "\"order\"")],
            &[("name", "loud")],
        ],
    );
    assert_eq!(site_lines(&result), ["src/api.ts:2 value_reference"]);
}

#[test]
fn typescript_spread_call_refuses() {
    let ws = Workspace::new(&[
        ("src/orders.ts", TS_ORDERS),
        (
            "src/api.ts",
            "import { renderOrder, type Order } from \"./orders\"\n\
             export function one(args: [Order, boolean]): string {\n\
             \x20 return renderOrder(...args)\n\
             }\n",
        ),
    ]);
    let result = ws.refuse(
        "unsupported_call_site",
        "renderOrder",
        "src/orders.ts",
        &[
            &[("name", "order")],
            &[("name", "prefix"), ("call_value", "\"order\"")],
            &[("name", "loud")],
        ],
    );
    assert_eq!(site_lines(&result), ["src/api.ts:3 unsupported_call_site"]);
}

// === Python ===

const PY_ORDERS: &str = "\"\"\"Orders.\"\"\"


def render_order(order, loud=False):
    return f\"ORDER:{order}\" if loud else f\"order:{order}\"
";

#[test]
fn python_imported_qualified_and_keyword_calls_are_remapped() {
    let ws = Workspace::new(&[
        ("src/__init__.py", ""),
        ("src/orders.py", PY_ORDERS),
        (
            "src/api.py",
            "from src import orders\n\
             from src.orders import render_order\n\
             \n\
             \n\
             def dashboard(items):\n\
             \x20   # render_order(x) in a comment is not a call\n\
             \x20   first = render_order(items[0])\n\
             \x20   second = orders.render_order(items[1], True)\n\
             \x20   third = render_order(order=items[2], loud=True)\n\
             \x20   fourth = render_order(items[3], loud=False)\n\
             \x20   return [first, second, third, fourth]\n",
        ),
    ]);
    let result = ws.change(
        "render_order",
        "src/orders.py",
        &[
            &[("name", "item"), ("from", "order")],
            &[
                ("name", "prefix"),
                ("type", "str"),
                ("call_value", "\"order\""),
            ],
            &[("name", "shout"), ("from", "loud")],
        ],
    );
    assert_applied(&result, 4);
    assert_eq!(
        ws.read("src/orders.py"),
        "\"\"\"Orders.\"\"\"\n\
         \n\
         \n\
         def render_order(item, prefix: str, shout=False):\n\
         \x20   return f\"ORDER:{item}\" if shout else f\"order:{item}\"\n"
    );
    assert_eq!(
        ws.read("src/api.py"),
        "from src import orders\n\
         from src.orders import render_order\n\
         \n\
         \n\
         def dashboard(items):\n\
         \x20   # render_order(x) in a comment is not a call\n\
         \x20   first = render_order(items[0], \"order\")\n\
         \x20   second = orders.render_order(items[1], \"order\", True)\n\
         \x20   third = render_order(item=items[2], prefix=\"order\", shout=True)\n\
         \x20   fourth = render_order(items[3], \"order\", shout=False)\n\
         \x20   return [first, second, third, fourth]\n"
    );
}

#[test]
fn python_method_calls_skip_the_implicit_receiver() {
    let ws = Workspace::new(&[
        (
            "pkg/fmt.py",
            "class Formatter:\n\
             \x20   def fmt(self, value, width):\n\
             \x20       return str(value).rjust(width)\n\
             \n\
             \x20   def twice(self, value):\n\
             \x20       return self.fmt(value, 2) + self.fmt(value, width=4)\n",
        ),
        (
            "pkg/use.py",
            "from pkg.fmt import Formatter\n\
             \n\
             OUT = Formatter().fmt(1, 3)\n\
             EXPLICIT = Formatter.fmt(Formatter(), 5, 6)\n",
        ),
    ]);
    let result = ws.change(
        "fmt",
        "pkg/fmt.py",
        &[&[("name", "width")], &[("name", "value")]],
    );
    assert_applied(&result, 4);
    let fmt = ws.read("pkg/fmt.py");
    assert!(fmt.contains("def fmt(self, width, value):"));
    assert!(fmt.contains("self.fmt(2, value) + self.fmt(width=4, value=value)"));
    let usage = ws.read("pkg/use.py");
    assert!(usage.contains("OUT = Formatter().fmt(3, 1)"));
    assert!(usage.contains("EXPLICIT = Formatter.fmt(Formatter(), 6, 5)"));
}

#[test]
fn python_value_reference_refuses() {
    let ws = Workspace::new(&[
        ("src/__init__.py", ""),
        ("src/orders.py", PY_ORDERS),
        (
            "src/api.py",
            "from src.orders import render_order\n\
             \n\
             \n\
             def all_orders(items):\n\
             \x20   return list(map(render_order, items))\n",
        ),
    ]);
    let result = ws.refuse(
        "value_reference",
        "render_order",
        "src/orders.py",
        &[
            &[("name", "order")],
            &[("name", "prefix"), ("call_value", "\"order\"")],
            &[("name", "loud")],
        ],
    );
    assert_eq!(site_lines(&result), ["src/api.py:5 value_reference"]);
}

#[test]
fn python_splat_call_refuses() {
    let ws = Workspace::new(&[
        ("src/__init__.py", ""),
        ("src/orders.py", PY_ORDERS),
        (
            "src/api.py",
            "from src.orders import render_order\n\
             \n\
             \n\
             def one(args, opts):\n\
             \x20   return render_order(*args) + render_order(\"a\", **opts)\n",
        ),
    ]);
    let result = ws.refuse(
        "unsupported_call_site",
        "render_order",
        "src/orders.py",
        &[
            &[("name", "order")],
            &[("name", "prefix"), ("call_value", "\"order\"")],
            &[("name", "loud")],
        ],
    );
    assert_eq!(
        site_lines(&result),
        [
            "src/api.py:5 unsupported_call_site",
            "src/api.py:5 unsupported_call_site"
        ]
    );
}

const PY_SHAPES: &str = "from abc import ABC, abstractmethod


class Shape(ABC):
    @abstractmethod
    def area(self, scale):
        ...


class Square(Shape):
    def area(self, scale):
        return scale
";

/// An overridden method changes with its base and every override.
#[test]
fn python_override_family_changes_every_override_and_call() {
    let files: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/shapes.py", PY_SHAPES),
        (
            "pkg/round.py",
            "from .shapes import Shape, Square\n\
             \n\
             \n\
             class Circle(Shape):\n\
             \x20   def area(self, scale):\n\
             \x20       return 3 * scale\n\
             \n\
             \n\
             class Tiny(Square):\n\
             \x20   pass\n\
             \n\
             \n\
             def total(c, s):\n\
             \x20   return c.area(scale=1) + Shape.area(s, 2) + Square.area(s, 3) + Tiny.area(s, 4)\n",
        ),
    ];
    for seed_line in [6, 11] {
        let ws = Workspace::new(files);
        let result = ws.change_with(
            "area",
            "pkg/shapes.py",
            &[
                &[("name", "scale")],
                &[("name", "offset"), ("call_value", "0")],
            ],
            &symbol_at("area", "pkg/shapes.py", seed_line),
        );
        assert_applied(&result, 4);
        assert_eq!(
            ws.read("pkg/shapes.py"),
            PY_SHAPES.replace("area(self, scale)", "area(self, scale, offset)")
        );
        let round = ws.read("pkg/round.py");
        assert!(round.contains("def area(self, scale, offset):"), "{round}");
        assert!(
            round.contains(
                "c.area(scale=1, offset=0) + Shape.area(s, 2, 0) + Square.area(s, 3, 0) + Tiny.area(s, 4, 0)"
            ),
            "{round}"
        );
    }
}

#[test]
fn python_override_family_refuses_unrelated_and_external_members() {
    let cases: &[(&str, &str)] = &[
        // A same-named method on a class outside the hierarchy.
        (
            "class Plot:\n    def area(self, scale):\n        return 1\n",
            "pkg/shapes.py:6 declaration",
        ),
        // An override whose class also extends a package class.
        (
            "from widgets import Frame\nfrom .shapes import Shape\n\n\nclass Panel(Shape, Frame):\n    def area(self, scale):\n        return 2\n",
            "pkg/extra.py:5 supertype",
        ),
        // A subclass binds the name to a callable in its class body.
        (
            "from .shapes import Square\n\n\nclass Lazy(Square):\n    area = lambda self, scale: scale\n",
            "pkg/extra.py:5 class_attribute",
        ),
        // A workspace class shadows the builtin `object`; its subclass inherits.
        (
            "from .shapes import Square\nfrom widgets import Frame\n\n\nclass object(Square):\n    pass\n\n\nclass Leaf(Frame, object):\n    pass\n",
            "pkg/extra.py:9 supertype",
        ),
        // A subclass that inherits the method beside a package class.
        (
            "from widgets import Frame\nfrom .shapes import Square\n\n\nclass Local(Frame, Square):\n    pass\n",
            "pkg/extra.py:5 supertype",
        ),
    ];
    for (extra, blocking) in cases {
        let ws = Workspace::new(&[
            ("pkg/__init__.py", ""),
            ("pkg/shapes.py", PY_SHAPES),
            ("pkg/extra.py", extra),
        ]);
        let before = ws.snapshot();
        let result = ws.change_with(
            "area",
            "pkg/shapes.py",
            &[
                &[("name", "scale")],
                &[("name", "offset"), ("default", "0")],
            ],
            &symbol_at("area", "pkg/shapes.py", 11),
        );
        assert_eq!(
            text(field(&result, "result")),
            "overrides_present",
            "{}",
            text(field(&result, "details"))
        );
        assert!(
            site_lines(&result).contains(&blocking.to_string()),
            "{:?}",
            site_lines(&result)
        );
        assert_eq!(ws.snapshot(), before);
    }
}

#[test]
fn same_named_free_functions_refuse_as_ambiguous_symbol_with_warning_candidates() {
    let ws = Workspace::new(&[
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a.rs", "pub fn load(x: u32) -> u32 {\n    x\n}\n"),
        ("src/b.rs", "pub fn load(x: u32) -> u32 {\n    x + 1\n}\n"),
    ]);
    let result = ws.refuse(
        "ambiguous_symbol",
        "load",
        "src/a.rs",
        &[
            &[("name", "x")],
            &[("name", "y"), ("type", "u32"), ("call_value", "0")],
        ],
    );
    let warnings: Vec<String> = items(field(&result, "warnings"))
        .iter()
        .map(|w| {
            format!(
                "{}:{}",
                text(field(w, "path")),
                match field(w, "line") {
                    VmValue::Int(n) => *n,
                    other => panic!("{other:?}"),
                }
            )
        })
        .collect();
    assert_eq!(warnings, ["src/b.rs:1"]);
    assert_eq!(text(field(&result, "scope")), "workspace");
    assert!(matches!(field(&result, "match_count"), VmValue::Int(0)));
    assert!(items(field(&result, "conflicts")).is_empty());
}

#[test]
fn a_file_that_does_not_parse_refuses_before_planning() {
    // The captured input: CPython rejects it, tree-sitter-python does not.
    let ws = Workspace::new(&[("broken.py", "def f(a):\n    b = a +\n    print(b)\n")]);
    let result = ws.refuse(
        "syntax_error",
        "f",
        "broken.py",
        &[
            &[("name", "a"), ("from", "a")],
            &[("name", "z"), ("default", "0")],
        ],
    );
    let details = text(field(&result, "details"));
    assert!(
        details.contains("`broken.py` does not parse before the edit"),
        "{details}"
    );
}

// Probe regressions: each applied edit below failed the crate's compiler or
// tsc. A harness calls a test with no arguments, and a declared function type
// keeps the old arity, so these must refuse instead.
#[test]
fn test_entry_and_typed_arrow_signatures_refuse_as_fixed() {
    let ws = Workspace::new(&[
        (
            "tests/macros.rs",
            "#[test]\n\
             fn kv_error() {\n\
             \x20   helper();\n\
             }\n\
             // runs on the async runtime\n\
             #[tokio::test]\n\
             async fn kv_async() {}\n\
             #[cfg_attr(test, test)]\n\
             fn kv_gated() {}\n\
             #[unsafe(no_mangle)]\n\
             pub extern \"C\" fn exported() {}\n\
             #[rstest]\n\
             fn cased() {}\n\
             #[test_case(1)]\n\
             fn listed(n: u8) {}\n\
             #[wasm_bindgen(js_name = \"bound\")]\n\
             pub fn bound() {}\n\
             #[cfg_attr(test, allow(dead_code))]\n\
             fn quiet() {}\n\
             #[cfg(test)]\n\
             #[doc = \"a test helper\"]\n\
             fn helper() {}\n",
        ),
        ("src/main.rs", "fn main() {\n    run();\n}\nfn run() {}\n"),
        (
            "src/check.ts",
            "type Res = { ok: boolean }\n\
             export const fallible: () => Res = () => ({ ok: true })\n\
             export const plain = () => ({ ok: true })\n\
             export const val = fallible() && plain()\n",
        ),
    ]);
    let extra: &[&[(&str, &str)]] = &[&[("name", "extra"), ("type", "u8"), ("call_value", "0")]];
    for (name, path, reason) in [
        ("kv_error", "tests/macros.rs", "#[test]"),
        ("kv_async", "tests/macros.rs", "#[tokio::test]"),
        ("kv_gated", "tests/macros.rs", "#[cfg_attr(test, test)]"),
        ("exported", "tests/macros.rs", "#[unsafe(no_mangle)]"),
        ("cased", "tests/macros.rs", "#[rstest]"),
        ("listed", "tests/macros.rs", "#[test_case(1)]"),
        ("bound", "tests/macros.rs", "#[wasm_bindgen("),
        ("main", "src/main.rs", "entry point"),
    ] {
        let result = ws.refuse("fixed_signature", name, path, extra);
        assert!(text(field(&result, "details")).contains(reason), "{name}");
    }
    let ts_extra: &[&[(&str, &str)]] =
        &[&[("name", "extra"), ("type", "number"), ("call_value", "0")]];
    let result = ws.refuse("fixed_signature", "fallible", "src/check.ts", ts_extra);
    assert!(text(field(&result, "details")).contains("() => Res"));
    assert_applied(&ws.change("plain", "src/check.ts", ts_extra), 1);
    assert_applied(&ws.change("helper", "tests/macros.rs", extra), 1);
    // The `cfg_attr` condition is not an attribute it applies.
    assert_applied(&ws.change("quiet", "tests/macros.rs", extra), 0);
}

/// `symbol_ref` selects as for `rename_symbol`, so the column the index
/// reports is accepted and must match.
#[test]
fn symbol_ref_column_selects_the_declaration() {
    let ws = Workspace::new(&[(
        "src/lib.rs",
        "pub fn helper(a: u8) -> u8 {\n    a\n}\n\npub fn caller() -> u8 {\n    helper(1)\n}\n",
    )]);
    let params: &[&[(&str, &str)]] = &[
        &[("name", "a")],
        &[("name", "b"), ("type", "u8"), ("call_value", "0")],
    ];
    let at = |col: i64| {
        [(
            "symbol_ref",
            dict(&[
                ("name", string("helper")),
                ("path", string("src/lib.rs")),
                ("line", VmValue::Int(1)),
                ("col", VmValue::Int(col)),
            ]),
        )]
    };
    let missed = ws.change_with("helper", "src/lib.rs", params, &at(3));
    assert_eq!(text(field(&missed, "result")), "no_match");
    assert_applied(&ws.change_with("helper", "src/lib.rs", params, &at(8)), 1);
}

const TS_SHAPE: &str = "export interface Shape {
  area(scale: number): number
}
";

const TS_SQUARE: &str = "import type { Shape } from \"./shape.js\"
export class Square implements Shape {
  area(scale: number): number {
    return scale
  }
}
export const total = (s: Shape, q: Square) => s.area(1) + q.area(2)
";

/// An interface method changes with every implementation and override.
#[test]
fn typescript_interface_family_changes_every_member_and_call() {
    let files: &[(&str, &str)] = &[
        ("src/shape.ts", TS_SHAPE),
        (
            "src/base.ts",
            "import { Shape } from \"./shape\"\n\
             export abstract class Base implements Shape {\n\
             \x20 abstract area(scale: number): number\n\
             }\n\
             export class Circle extends Base {\n\
             \x20 area(s: number): number {\n\
             \x20   return s * 3\n\
             \x20 }\n\
             }\n",
        ),
        ("src/square.ts", TS_SQUARE),
    ];
    // Each seed names the kept parameter as it declares it.
    // The index holds no interface signature, so seeds are class methods.
    for (seed_path, seed_line, kept) in [("src/square.ts", 3, "scale"), ("src/base.ts", 6, "s")] {
        let ws = Workspace::new(files);
        let result = ws.change_with(
            "area",
            seed_path,
            &[
                &[("name", kept)],
                &[
                    ("name", "extra"),
                    ("type", "number"),
                    ("default", "0"),
                    ("call_value", "0"),
                ],
            ],
            &symbol_at("area", seed_path, seed_line),
        );
        assert_applied(&result, 2);
        // A signature without a body cannot carry an initializer.
        assert_eq!(
            ws.read("src/shape.ts"),
            TS_SHAPE.replace("(scale: number)", "(scale: number, extra?: number)")
        );
        let base = ws.read("src/base.ts");
        assert!(
            base.contains("abstract area(scale: number, extra?: number): number"),
            "{base}"
        );
        assert!(
            base.contains("area(s: number, extra: number = 0): number {"),
            "{base}"
        );
        let square = ws.read("src/square.ts");
        assert!(
            square.contains("area(scale: number, extra: number = 0): number {"),
            "{square}"
        );
        assert!(square.contains("s.area(1, 0) + q.area(2, 0)"), "{square}");
    }
}

#[test]
fn typescript_family_refuses_package_supertypes_and_property_members() {
    let cases: &[(&str, &str)] = &[
        // An implementation that also implements a package interface.
        (
            "import { Shape } from \"./shape\"\n\
             import { Sized } from \"geometry-kit\"\n\
             export class Hex implements Shape, Sized {\n\
             \x20 area(scale: number): number {\n\
             \x20   return scale\n\
             \x20 }\n\
             }\n",
            "src/extra.ts:3 supertype",
        ),
        // A class field may hold an implementation.
        (
            "export class Lazy {\n\
             \x20 area = (scale: number) => scale\n\
             }\n",
            "src/extra.ts:2 property",
        ),
        // A mixin call cannot be resolved by name.
        (
            "import { Shape } from \"./shape\"\n\
             import { Base } from \"./base\"\n\
             const mix = (b: any) => b\n\
             export class Hex extends mix(Base) implements Shape {\n\
             \x20 area(scale: number): number {\n\
             \x20   return scale\n\
             \x20 }\n\
             }\n",
            "src/extra.ts:4 supertype",
        ),
        // A class that inherits its implementation from a package base.
        (
            "import { Shape } from \"./shape\"\n\
             import { Widget } from \"ui-kit\"\n\
             export class Local extends Widget implements Shape {}\n",
            "src/extra.ts:3 supertype",
        ),
    ];
    for (extra, blocking) in cases {
        let ws = Workspace::new(&[
            ("src/shape.ts", TS_SHAPE),
            ("src/base.ts", "export class Base {}\n"),
            ("src/square.ts", TS_SQUARE),
            ("src/extra.ts", extra),
        ]);
        let before = ws.snapshot();
        let result = ws.change_with(
            "area",
            "src/shape.ts",
            &[
                &[("name", "scale")],
                &[("name", "extra"), ("type", "number"), ("call_value", "0")],
            ],
            &symbol_at("area", "src/square.ts", 3),
        );
        assert_eq!(
            text(field(&result, "result")),
            "overrides_present",
            "{}",
            text(field(&result, "details"))
        );
        assert_eq!(site_lines(&result), [*blocking]);
        assert_eq!(ws.snapshot(), before);
    }
}
