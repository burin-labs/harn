use super::*;
use crate::test_support::*;

use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

use crate::code_index::CodeIndexCapability;

fn int(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

fn boolean(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn list(value: &VmValue) -> Vec<VmValue> {
    match value {
        VmValue::List(items) => items.iter().cloned().collect(),
        other => panic!("expected list, got {other:?}"),
    }
}

/// A workspace on disk, indexed, with a snapshot of every file.
struct Fixture {
    dir: tempfile::TempDir,
    capability: CodeIndexCapability,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempdir().unwrap();
        for (path, body) in files {
            let abs = dir.path().join(path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        let capability = CodeIndexCapability::new();
        let (state, _) = IndexState::build_from_root(dir.path());
        *capability.shared().lock().unwrap() = Some(state);
        Self { dir, capability }
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for entry in walkdir::WalkDir::new(self.dir.path()) {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(self.dir.path()).unwrap();
                out.insert(
                    rel.to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
        out
    }

    fn move_symbol(&self, args: &[(&str, VmValue)]) -> VmValue {
        run(&self.capability.shared(), &[dict(args)]).expect("move_symbol runs")
    }

    fn mv(&self, symbol: &str, path: &str, to_path: &str) -> VmValue {
        self.move_symbol(&[
            ("symbol", vm_string(symbol)),
            ("path", vm_string(path)),
            ("to_path", vm_string(to_path)),
        ])
    }

    /// Run a move that must refuse with `tag`, and prove no file changed.
    fn refused(&self, tag: &str, args: &[(&str, VmValue)]) -> VmValue {
        let before = self.snapshot();
        let result = self.move_symbol(args);
        assert_eq!(s(field(&result, "result")), tag, "{result:?}");
        assert_eq!(
            self.snapshot(),
            before,
            "a refusal must leave every file byte-identical"
        );
        assert!(list(field(&result, "touched_files")).is_empty());
        result
    }
}

fn tag(result: &VmValue) -> String {
    s(field(result, "result"))
}

fn touched_paths(result: &VmValue) -> Vec<String> {
    list(field(result, "touched_files"))
        .iter()
        .map(|f| s(field(f, "path")))
        .collect()
}

// === Rust ===

const RUST_JOBS: &str = "#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub code: String,
    pub priority: u8,
}

pub fn render_job(job: &Job) -> String {
    format!(\"job:{}:{}\", job.code, job.priority)
}

// Operators still use this wording in saved dispatch reports.
pub fn priority_label(job: &Job) -> &'static str {
    if job.priority >= 8 { \"urgent\" } else { \"normal\" }
}
";

fn rust_fixture() -> Fixture {
    Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"dispatch-kit\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("src/lib.rs", "pub mod api;\npub mod display;\npub mod jobs;\n\npub use jobs::Job;\n"),
        ("src/jobs.rs", RUST_JOBS),
        ("src/display.rs", "pub fn banner() -> &'static str {\n    \"dispatch\"\n}\n"),
        (
            "src/api.rs",
            "use crate::jobs::{priority_label, render_job, Job};\nuse crate::jobs;\n\npub fn dashboard(jobs: &[Job]) -> Vec<String> {\n    jobs.iter().map(|job| format!(\"{} {} {}\", render_job(job), priority_label(job), jobs::priority_label(job))).collect()\n}\n",
        ),
        (
            "tests/integration.rs",
            "use dispatch_kit::{api::dashboard, jobs::{priority_label, render_job, Job}};\n\n#[test]\nfn flow() {\n    let job = Job { code: \"a\".into(), priority: 9 };\n    assert_eq!(priority_label(&job), \"urgent\");\n    assert_eq!(dispatch_kit::jobs::priority_label(&job), \"urgent\");\n    let _ = (dashboard(&[]), render_job(&job));\n}\n",
        ),
    ])
}

#[test]
fn rust_move_splits_use_trees_rewrites_paths_and_imports_free_names() {
    let fixture = rust_fixture();
    let result = fixture.mv("priority_label", "src/jobs.rs", "src/display.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");

    // The item leaves with its attached comment; the source keeps the rest.
    let jobs = fixture.read("src/jobs.rs");
    assert!(!jobs.contains("priority_label"), "{jobs}");
    assert!(!jobs.contains("Operators still use"), "{jobs}");
    assert!(
        jobs.ends_with("    format!(\"job:{}:{}\", job.code, job.priority)\n}\n"),
        "{jobs:?}"
    );

    assert_eq!(
        fixture.read("src/display.rs"),
        "use crate::jobs::Job;\n\npub fn banner() -> &'static str {\n    \"dispatch\"\n}\n\n// Operators still use this wording in saved dispatch reports.\npub fn priority_label(job: &Job) -> &'static str {\n    if job.priority >= 8 { \"urgent\" } else { \"normal\" }\n}\n"
    );

    // Grouped use-tree split, plus a qualified call through a module import.
    assert_eq!(
        fixture.read("src/api.rs"),
        "use crate::jobs::{render_job, Job};\nuse crate::jobs;\nuse crate::display::priority_label;\n\npub fn dashboard(jobs: &[Job]) -> Vec<String> {\n    jobs.iter().map(|job| format!(\"{} {} {}\", render_job(job), priority_label(job), crate::display::priority_label(job))).collect()\n}\n"
    );

    // A tests/ crate names the library by its package, nested group split.
    assert_eq!(
        fixture.read("tests/integration.rs"),
        "use dispatch_kit::{api::dashboard, jobs::{render_job, Job}};\nuse dispatch_kit::display::priority_label;\n\n#[test]\nfn flow() {\n    let job = Job { code: \"a\".into(), priority: 9 };\n    assert_eq!(priority_label(&job), \"urgent\");\n    assert_eq!(dispatch_kit::display::priority_label(&job), \"urgent\");\n    let _ = (dashboard(&[]), render_job(&job));\n}\n"
    );

    let mut paths = touched_paths(&result);
    paths.sort();
    assert_eq!(
        paths,
        [
            "src/api.rs",
            "src/display.rs",
            "src/jobs.rs",
            "tests/integration.rs"
        ]
    );
    assert_eq!(int(field(&result, "occurrences_replaced")), 4);
    assert_eq!(int(field(&result, "call_sites_updated")), 2);
    assert!(list(field(&result, "comments_left_behind")).is_empty());
}

#[test]
fn rust_dry_run_reports_edits_and_writes_nothing() {
    let fixture = rust_fixture();
    let before = fixture.snapshot();
    let result = fixture.move_symbol(&[
        ("symbol", vm_string("priority_label")),
        ("file", vm_string("src/jobs.rs")),
        ("destination", vm_string("src/display.rs")),
        ("dry_run", VmValue::Bool(true)),
    ]);
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert!(boolean(field(&result, "dry_run")));
    assert!(!boolean(field(&result, "applied")));
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(touched_paths(&result).len(), 4);
}

#[test]
fn rust_source_that_still_uses_the_item_reimports_it() {
    let fixture = Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "use std::collections::HashMap;\n\n/// Doc stays attached.\n#[inline]\npub fn helper() -> HashMap<u8, u8> {\n    HashMap::new()\n}\n\npub fn caller() -> usize {\n    helper().len()\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.mv("helper", "src/a.rs", "src/b.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    // The import only the item used goes with it; the source re-imports
    // the item because `caller` still calls it.
    assert_eq!(
        fixture.read("src/a.rs"),
        "use crate::b::helper;\n\npub fn caller() -> usize {\n    helper().len()\n}\n"
    );
    assert_eq!(
        fixture.read("src/b.rs"),
        "use std::collections::HashMap;\n\npub fn other() {}\n\n/// Doc stays attached.\n#[inline]\npub fn helper() -> HashMap<u8, u8> {\n    HashMap::new()\n}\n"
    );
}

#[test]
fn rust_missing_destination_is_created_and_declared() {
    let fixture = rust_fixture();
    let result = fixture.mv("priority_label", "src/jobs.rs", "src/labels.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert!(boolean(field(&result, "created_destination")));
    assert_eq!(
        fixture.read("src/labels.rs"),
        "use crate::jobs::Job;\n\n// Operators still use this wording in saved dispatch reports.\npub fn priority_label(job: &Job) -> &'static str {\n    if job.priority >= 8 { \"urgent\" } else { \"normal\" }\n}\n"
    );
    assert_eq!(
        fixture.read("src/lib.rs"),
        "pub mod api;\npub mod display;\npub mod jobs;\npub mod labels;\n\npub use jobs::Job;\n"
    );
}

#[test]
fn rust_private_item_with_outside_users_requires_visibility() {
    let fixture = Fixture::new(&[
        (
            "Cargo.toml",
            "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "fn helper() -> u8 {\n    1\n}\n\npub fn caller() -> u8 {\n    helper()\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.refused(
        "visibility_required",
        &[
            ("symbol", vm_string("helper")),
            ("path", vm_string("src/a.rs")),
            ("to_path", vm_string("src/b.rs")),
        ],
    );
    let sites = list(field(&result, "sites"));
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert_eq!(int(field(&sites[0], "line")), 6);
}

#[test]
fn rust_item_using_a_private_field_requires_visibility() {
    let fixture = Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a.rs", "pub struct Job {\n    level: u8,\n}\n\npub fn level(job: &Job) -> u8 {\n    job.level\n}\n"),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    fixture.refused(
        "visibility_required",
        &[
            ("symbol", vm_string("level")),
            ("path", vm_string("src/a.rs")),
            ("to_path", vm_string("src/b.rs")),
        ],
    );
}

#[test]
fn destination_that_declares_the_name_refuses() {
    let fixture = rust_fixture();
    fs::write(
        fixture.dir.path().join("src/display.rs"),
        "pub fn priority_label() {}\n",
    )
    .unwrap();
    fixture.refused(
        "destination_conflict",
        &[
            ("symbol", vm_string("priority_label")),
            ("path", vm_string("src/jobs.rs")),
            ("to_path", vm_string("src/display.rs")),
        ],
    );
}

#[test]
fn unknown_symbol_and_wrong_language_refuse() {
    let fixture = rust_fixture();
    fixture.refused(
        "no_match",
        &[
            ("symbol", vm_string("nope")),
            ("path", vm_string("src/jobs.rs")),
            ("to_path", vm_string("src/display.rs")),
        ],
    );
    fixture.refused(
        "unsupported_language",
        &[
            ("symbol", vm_string("priority_label")),
            ("path", vm_string("src/jobs.rs")),
            ("to_path", vm_string("src/display.py")),
        ],
    );
}

#[test]
fn a_detached_comment_stays_and_is_reported() {
    let fixture = Fixture::new(&[
        (
            "Cargo.toml",
            "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub fn keep() {}\n\n// Section: helpers.\n\npub fn helper() {}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.mv("helper", "src/a.rs", "src/b.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    let left = list(field(&result, "comments_left_behind"));
    assert_eq!(left.len(), 1);
    assert_eq!(s(field(&left[0], "text")), "// Section: helpers.");
    assert_eq!(
        fixture.read("src/a.rs"),
        "pub fn keep() {}\n\n// Section: helpers.\n"
    );
    assert_eq!(
        fixture.read("src/b.rs"),
        "pub fn other() {}\n\npub fn helper() {}\n"
    );
}

// === TypeScript ===

const TS_ORDERS: &str = "export interface Order {
  code: string
  priority: number
}

export function renderOrder(order: Order): string {
  return `order:${order.code}`
}

// Paper dispatch sheets still use these status labels.
export function priorityLabel(order: Order): string {
  return order.priority >= 8 ? \"urgent\" : \"normal\"
}
";

fn ts_fixture() -> Fixture {
    Fixture::new(&[
        ("src/orders.ts", TS_ORDERS),
        ("src/view.ts", "export function heading(): string {\n  return \"orders\"\n}\n"),
        (
            "src/api.ts",
            "import { priorityLabel, renderOrder, type Order } from \"./orders\"\nimport * as orders from \"./orders\"\n\nexport function dashboard(list: Order[]): string[] {\n  return list.map((o) => `${renderOrder(o)} ${priorityLabel(o)} ${orders.priorityLabel(o)}`)\n}\n",
        ),
        (
            "src/report.ts",
            "import type { Order } from \"./orders\";\nimport { heading } from \"./view\";\n\nexport function title(o: Order): string {\n  return heading() + o.code;\n}\n",
        ),
    ])
}

#[test]
fn ts_move_splits_named_imports_and_rewrites_namespace_calls() {
    let fixture = ts_fixture();
    let result = fixture.mv("priorityLabel", "src/orders.ts", "src/view.ts");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        fixture.read("src/view.ts"),
        "import type { Order } from \"./orders\"\n\nexport function heading(): string {\n  return \"orders\"\n}\n\n// Paper dispatch sheets still use these status labels.\nexport function priorityLabel(order: Order): string {\n  return order.priority >= 8 ? \"urgent\" : \"normal\"\n}\n"
    );
    assert_eq!(
        fixture.read("src/api.ts"),
        "import { renderOrder, type Order } from \"./orders\"\nimport * as orders from \"./orders\"\nimport { priorityLabel } from \"./view\"\n\nexport function dashboard(list: Order[]): string[] {\n  return list.map((o) => `${renderOrder(o)} ${priorityLabel(o)} ${priorityLabel(o)}`)\n}\n"
    );
    assert!(!fixture.read("src/orders.ts").contains("priorityLabel"));
    assert!(!fixture.read("src/orders.ts").contains("Paper dispatch"));
    // Untouched: it never named the moved function.
    assert!(!touched_paths(&result).contains(&"src/report.ts".to_string()));
}

#[test]
fn ts_type_only_imports_stay_type_only_when_an_interface_moves() {
    let fixture = ts_fixture();
    let result = fixture.mv("Order", "src/orders.ts", "src/models.ts");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert!(boolean(field(&result, "created_destination")));
    assert_eq!(
        fixture.read("src/models.ts"),
        "export interface Order {\n  code: string\n  priority: number\n}\n"
    );
    // The source still uses the type, so it imports it back, type-only.
    assert!(fixture
        .read("src/orders.ts")
        .starts_with("import type { Order } from \"./models\"\n\nexport function renderOrder(order: Order): string {"));
    // Statement-level `import type`, retargeted in place, keeps `;` style.
    assert_eq!(
        fixture.read("src/report.ts"),
        "import type { Order } from \"./models\";\nimport { heading } from \"./view\";\n\nexport function title(o: Order): string {\n  return heading() + o.code;\n}\n"
    );
    // An inline `type` specifier is split out as a type-only import.
    assert!(fixture.read("src/api.ts").starts_with(
        "import { priorityLabel, renderOrder } from \"./orders\"\nimport * as orders from \"./orders\"\nimport type { Order } from \"./models\"\n"
    ));
}

#[test]
fn ts_unexported_item_used_by_the_source_requires_visibility() {
    let fixture = Fixture::new(&[
        ("src/a.ts", "function helper(): number {\n  return 1\n}\n\nexport function caller(): number {\n  return helper()\n}\n"),
        ("src/b.ts", "export const B = 1\n"),
    ]);
    fixture.refused(
        "visibility_required",
        &[
            ("symbol", vm_string("helper")),
            ("path", vm_string("src/a.ts")),
            ("to_path", vm_string("src/b.ts")),
        ],
    );
}

// === Python ===

const PY_ORDERS: &str = "\"\"\"Order parsing and formatting.\"\"\"

from dataclasses import dataclass


@dataclass(frozen=True)
class Order:
    code: str
    priority: int


def render_order(order: Order) -> str:
    return f\"order:{order.code}:{order.priority}\"


# Dispatch reports retain these labels for operators.
def priority_label(order: Order) -> str:
    return \"urgent\" if order.priority >= 8 else \"normal\"
";

fn py_fixture() -> Fixture {
    Fixture::new(&[
        ("src/__init__.py", "\"\"\"Dispatch package.\"\"\"\n"),
        ("src/orders.py", PY_ORDERS),
        ("src/view.py", "\"\"\"Display helpers.\"\"\"\n\n\ndef heading() -> str:\n    return \"orders\"\n"),
        (
            "src/api.py",
            "\"\"\"Public entry point.\"\"\"\n\nfrom src.orders import Order, priority_label, render_order\n\n\ndef dashboard(orders: list[Order]) -> list[str]:\n    return [f\"{render_order(o)} {priority_label(o)}\" for o in orders]\n",
        ),
        (
            "tests/test_flow.py",
            "import src.orders\nfrom src import orders as legacy\n\n\ndef test_label() -> None:\n    order = src.orders.Order(\"a\", 9)\n    assert src.orders.priority_label(order) == \"urgent\"\n    assert legacy.priority_label(order) == \"urgent\"\n",
        ),
    ])
}

#[test]
fn py_move_splits_from_imports_and_rewrites_module_qualified_calls() {
    let fixture = py_fixture();
    let result = fixture.mv("priority_label", "src/orders.py", "src/view.py");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        fixture.read("src/view.py"),
        "\"\"\"Display helpers.\"\"\"\n\nfrom src.orders import Order\n\n\ndef heading() -> str:\n    return \"orders\"\n\n\n# Dispatch reports retain these labels for operators.\ndef priority_label(order: Order) -> str:\n    return \"urgent\" if order.priority >= 8 else \"normal\"\n"
    );
    assert_eq!(
        fixture.read("src/api.py"),
        "\"\"\"Public entry point.\"\"\"\n\nfrom src.orders import Order, render_order\nfrom src.view import priority_label\n\n\ndef dashboard(orders: list[Order]) -> list[str]:\n    return [f\"{render_order(o)} {priority_label(o)}\" for o in orders]\n"
    );
    assert_eq!(
        fixture.read("tests/test_flow.py"),
        "import src.orders\nfrom src import orders as legacy\nimport src.view\n\n\ndef test_label() -> None:\n    order = src.orders.Order(\"a\", 9)\n    assert src.view.priority_label(order) == \"urgent\"\n    assert src.view.priority_label(order) == \"urgent\"\n"
    );
    let orders = fixture.read("src/orders.py");
    assert!(
        orders.ends_with("    return f\"order:{order.code}:{order.priority}\"\n"),
        "{orders:?}"
    );
}

#[test]
fn py_plain_module_import_and_attribute_call() {
    let fixture = Fixture::new(&[
        (
            "orders.py",
            "def label(n: int) -> str:\n    return str(n)\n\n\ndef other() -> int:\n    return 1\n",
        ),
        ("view.py", "def heading() -> str:\n    return \"h\"\n"),
        (
            "main.py",
            "import orders\n\nprint(orders.label(1), orders.other())\n",
        ),
    ]);
    let result = fixture.mv("label", "orders.py", "view.py");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        fixture.read("main.py"),
        "import orders\nimport view\n\nprint(view.label(1), orders.other())\n"
    );
    assert_eq!(
        fixture.read("orders.py"),
        "def other() -> int:\n    return 1\n"
    );
    assert_eq!(
        fixture.read("view.py"),
        "def heading() -> str:\n    return \"h\"\n\n\ndef label(n: int) -> str:\n    return str(n)\n"
    );
}

#[test]
fn py_move_that_would_close_a_module_cycle_refuses() {
    let fixture = py_fixture();
    // The source keeps calling the label, and the label needs `Order` from
    // the source: both modules would import each other at load time.
    let orders = format!("{PY_ORDERS}\n\ndef shout(order: Order) -> str:\n    return priority_label(order).upper()\n");
    fs::write(fixture.dir.path().join("src/orders.py"), orders).unwrap();
    let (state, _) = IndexState::build_from_root(fixture.dir.path());
    *fixture.capability.shared().lock().unwrap() = Some(state);
    let result = fixture.refused(
        "import_cycle",
        &[
            ("symbol", vm_string("priority_label")),
            ("path", vm_string("src/orders.py")),
            ("to_path", vm_string("src/view.py")),
        ],
    );
    assert!(!list(field(&result, "sites")).is_empty());
}

#[test]
fn ambiguous_seed_lists_candidates_in_warnings() {
    let fixture = Fixture::new(&[
        (
            "a.py",
            "def f():\n    return 1\n\n\ndef f():\n    return 2\n",
        ),
        ("b.py", "X = 1\n"),
    ]);
    let args = [
        ("symbol", vm_string("f")),
        ("path", vm_string("a.py")),
        ("to_path", vm_string("b.py")),
    ];
    let result = fixture.refused("ambiguous_symbol", &args);
    let lines: Vec<i64> = list(field(&result, "warnings"))
        .iter()
        .map(|c| int(field(c, "line")))
        .collect();
    assert_eq!(lines, [1, 5]);
    // `line` pins one of them.
    let mut pinned = args.to_vec();
    pinned.push(("line", VmValue::Int(5)));
    let result = fixture.move_symbol(&pinned);
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(fixture.read("b.py"), "X = 1\n\n\ndef f():\n    return 2\n");
}

#[test]
fn capability_column_matches_the_languages_the_builtin_moves() {
    for &language in Language::all() {
        assert_eq!(
            language.edit_capabilities().move_symbol,
            Family::of(language).is_some(),
            "{}",
            language.name()
        );
    }
}

#[test]
fn a_name_used_only_inside_an_fstring_is_imported_at_the_destination() {
    let fixture = Fixture::new(&[
        (
            "orders.py",
            "PREFIX = \"order\"\n\n\ndef label(n: int) -> str:\n    return f\"{PREFIX}:{n}\"\n",
        ),
        ("view.py", "def heading() -> str:\n    return \"h\"\n"),
    ]);
    let result = fixture.mv("label", "orders.py", "view.py");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert!(fixture
        .read("view.py")
        .starts_with("from orders import PREFIX\n\n"));
}

#[test]
fn a_referencing_file_that_does_not_parse_refuses_before_planning() {
    let broken = "from orders import label\n\nx = (\n";
    let fixture = Fixture::new(&[
        (
            "orders.py",
            "def label(n: int) -> str:\n    return str(n)\n",
        ),
        ("view.py", "def heading() -> str:\n    return \"h\"\n"),
        ("main.py", broken),
    ]);
    let result = fixture.refused(
        "syntax_error",
        &[
            ("symbol", vm_string("label")),
            ("path", vm_string("orders.py")),
            ("to_path", vm_string("view.py")),
        ],
    );
    assert!(s(field(&result, "details")).starts_with("`main.py` does not parse before the edit"));
}

#[test]
fn a_broken_file_that_only_mentions_the_name_does_not_block_the_move() {
    let fixture = Fixture::new(&[
        (
            "orders.py",
            "def label(n: int) -> str:\n    return str(n)\n",
        ),
        ("view.py", "def heading() -> str:\n    return \"h\"\n"),
        ("notes.py", "# label is documented here\nx = (\n"),
    ]);
    let result = fixture.mv("label", "orders.py", "view.py");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        fixture.read("notes.py"),
        "# label is documented here\nx = (\n"
    );
}

#[test]
fn a_path_outside_the_workspace_is_rejected_before_any_read_or_write() {
    let fixture = Fixture::new(&[
        (
            "pkg/orders.py",
            "def label(n: int) -> str:\n    return str(n)\n",
        ),
        ("pkg/view.py", "X = 1\n"),
    ]);
    let outside = fixture
        .dir
        .path()
        .parent()
        .unwrap()
        .join("move-symbol-escape.py");
    let before = fixture.snapshot();
    for (param, path, to_path) in [
        ("to_path", "pkg/orders.py", "../move-symbol-escape.py"),
        ("to_path", "pkg/orders.py", outside.to_str().unwrap()),
        ("path", "../pkg/orders.py", "pkg/view.py"),
    ] {
        let err = run(
            &fixture.capability.shared(),
            &[dict(&[
                ("symbol", vm_string("label")),
                ("path", vm_string(path)),
                ("to_path", vm_string(to_path)),
            ])],
        )
        .expect_err("an escaping path must be refused");
        assert!(
            matches!(err, HostlibError::InvalidParameter { param: p, .. } if p == param),
            "{err:?}"
        );
    }
    assert!(!outside.exists());
    assert_eq!(fixture.snapshot(), before);
}

#[cfg(unix)]
#[test]
fn a_destination_behind_a_symlink_out_of_the_workspace_is_rejected() {
    let outside = tempdir().unwrap();
    let fixture = Fixture::new(&[
        (
            "orders.py",
            "def label(n: int) -> str:\n    return str(n)\n",
        ),
        ("view.py", "X = 1\n"),
    ]);
    std::os::unix::fs::symlink(outside.path(), fixture.dir.path().join("link")).unwrap();
    let before = fixture.snapshot();
    let err = run(
        &fixture.capability.shared(),
        &[dict(&[
            ("symbol", vm_string("label")),
            ("path", vm_string("orders.py")),
            ("to_path", vm_string("link/new.py")),
        ])],
    )
    .expect_err("a destination behind an escaping symlink must be refused");
    assert!(
        matches!(
            err,
            HostlibError::InvalidParameter {
                param: "to_path",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(!outside.path().join("new.py").exists());
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn a_source_file_broken_across_lines_refuses_before_planning() {
    // tree-sitter-python reads `b = a +` and the next line as one expression;
    // Python rejects the file, so the move must not rewrite it.
    let source =
        "def label(n: int) -> str:\n    return str(n)\n\n\ndef f(a):\n    b = a +\n    print(b)\n";
    let fixture = Fixture::new(&[
        ("orders.py", source),
        ("view.py", "def heading() -> str:\n    return \"h\"\n"),
    ]);
    let result = fixture.refused(
        "syntax_error",
        &[
            ("symbol", vm_string("label")),
            ("path", vm_string("orders.py")),
            ("to_path", vm_string("view.py")),
        ],
    );
    assert!(s(field(&result, "details")).starts_with("`orders.py` does not parse before the edit"));
}

// The three Rust cases below are reductions of compiler failures a seeded
// qualification run found in published crates.

#[test]
fn rust_import_the_source_still_uses_stays_in_the_source() {
    let fixture = Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod buf;\n"),
        ("src/buf/mod.rs", "pub mod uninit;\n"),
        (
            "src/buf/uninit.rs",
            "use core::fmt;\nuse core::mem::MaybeUninit;\n\n/// Uninitialized bytes.\n#[repr(transparent)]\npub struct Slice([MaybeUninit<u8>]);\n\nimpl Slice {\n    pub fn new(slice: &mut [MaybeUninit<u8>]) -> &mut Slice {\n        unsafe { &mut *(slice as *mut [MaybeUninit<u8>] as *mut Slice) }\n    }\n}\n\nimpl fmt::Debug for Slice {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        f.write_str(\"Slice\")\n    }\n}\n",
        ),
    ]);
    let result = fixture.mv("Slice", "src/buf/uninit.rs", "src/buf/moved.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    let source = fixture.read("src/buf/uninit.rs");
    assert!(source.contains("use core::mem::MaybeUninit;"), "{source}");
    assert!(fixture
        .read("src/buf/moved.rs")
        .contains("use core::mem::MaybeUninit;"));
}

#[test]
fn rust_source_reading_the_moved_structs_private_fields_refuses() {
    let fixture = Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod chain;\npub mod other;\n"),
        (
            "src/chain.rs",
            "pub struct Chain<T, U> {\n    a: T,\n    b: U,\n}\n\nimpl<T, U> Chain<T, U> {\n    pub fn first_ref(&self) -> &T {\n        &self.a\n    }\n}\n",
        ),
        ("src/other.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.refused(
        "visibility_required",
        &[
            ("symbol", vm_string("Chain")),
            ("path", vm_string("src/chain.rs")),
            ("to_path", vm_string("src/other.rs")),
        ],
    );
    let sites = list(field(&result, "sites"));
    assert_eq!(int(field(&sites[0], "line")), 8, "{sites:?}");
}

#[test]
fn rust_import_list_in_a_nested_module_refuses() {
    let fixture = Fixture::new(&[
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        (
            "src/lib.rs",
            "pub mod other;\n\npub struct Meta {\n    pub level: u8,\n}\n\npub struct Record {\n    pub meta: Meta,\n}\n\n#[cfg(test)]\nmod tests {\n    use super::{Meta, Record};\n\n    #[test]\n    fn build() {\n        let _ = Record { meta: Meta { level: 1 } };\n    }\n}\n",
        ),
        ("src/other.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.refused(
        "nested_import",
        &[
            ("symbol", vm_string("Meta")),
            ("path", vm_string("src/lib.rs")),
            ("to_path", vm_string("src/other.rs")),
        ],
    );
    let sites = list(field(&result, "sites"));
    assert_eq!(int(field(&sites[0], "line")), 13, "{sites:?}");
}

/// A crate whose `a::model` declares `Meta` and the tuple struct `Wrap`,
/// with `b::moved` as the destination outside `a`.
fn restricted_fields_fixture(extra: &[(&str, &str)]) -> Fixture {
    let mut files = vec![
        ("Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/b/mod.rs", "pub mod moved;\n"),
        ("src/b/moved.rs", "pub fn other() {}\n"),
        (
            "src/a/model.rs",
            "pub mod child;\n\npub struct Meta {\n    pub(super) level: u8,\n    pub(crate) target: u8,\n    id: u8,\n}\n\npub struct Wrap(u8);\n",
        ),
    ];
    files.extend_from_slice(extra);
    Fixture::new(&files)
}

fn moved_out_of_a(fixture: &Fixture, symbol: &str) -> Vec<(String, i64)> {
    let result = fixture.refused(
        "visibility_required",
        &[
            ("symbol", vm_string(symbol)),
            ("path", vm_string("src/a/model.rs")),
            ("to_path", vm_string("src/b/moved.rs")),
        ],
    );
    list(field(&result, "sites"))
        .iter()
        .map(|site| (s(field(site, "path")), int(field(site, "line"))))
        .collect()
}

#[test]
fn rust_readers_that_lose_a_restricted_field_refuse() {
    let fixture = restricted_fields_fixture(&[
        (
            "src/a/mod.rs",
            "pub mod model;\n\npub fn level(m: &model::Meta) -> u8 {\n    m.level\n}\n",
        ),
        (
            "src/a/model/child.rs",
            "use super::Meta;\n\npub fn id(m: &Meta) -> u8 {\n    m.id + m.target\n}\n\npub fn copy(m: &Meta) -> Meta {\n    Meta { target: 2, ..*m }\n}\n\nimpl std::fmt::Display for Meta {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"{}\", self.id)\n    }\n}\n",
        ),
    ]);
    // `pub(super)` from the parent, private from a child module, and a
    // `..base` update that copies both; `pub(crate)` stays readable.
    assert_eq!(
        moved_out_of_a(&fixture, "Meta"),
        [
            ("src/a/mod.rs".to_string(), 4),
            ("src/a/model/child.rs".to_string(), 4),
            ("src/a/model/child.rs".to_string(), 8),
            ("src/a/model/child.rs".to_string(), 13),
        ]
    );
}

#[test]
fn rust_tuple_struct_reads_constructors_and_patterns_in_other_files_refuse() {
    let fixture = restricted_fields_fixture(&[
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model/child.rs",
            "pub fn get(w: &super::Wrap) -> u8 {\n    w.0\n}\n\npub fn make() -> super::Wrap {\n    super::Wrap(1)\n}\n\npub fn take(w: super::Wrap) -> u8 {\n    let super::Wrap(x) = w;\n    x\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn reads() {\n        let w = super::make();\n        assert_eq!(w.0, 1);\n        assert!(matches!(w, super::Wrap(1)));\n    }\n}\n",
        ),
    ]);
    let lines: Vec<i64> = moved_out_of_a(&fixture, "Wrap")
        .into_iter()
        .map(|(path, line)| {
            assert_eq!(path, "src/a/model/child.rs");
            line
        })
        .collect();
    assert_eq!(lines, [2, 6, 10, 19, 20]);
}

#[test]
fn rust_tuple_struct_built_through_an_import_alias_refuses() {
    let fixture = restricted_fields_fixture(&[
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model/child.rs",
            "use super::Wrap as W;\n\npub fn make() -> W {\n    W(1)\n}\n",
        ),
    ]);
    assert_eq!(
        moved_out_of_a(&fixture, "Wrap"),
        [("src/a/model/child.rs".to_string(), 4)]
    );
}

#[test]
fn rust_move_into_the_parent_keeps_child_readers() {
    let fixture = restricted_fields_fixture(&[
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model/child.rs",
            "pub fn get(w: &crate::a::model::Wrap) -> u8 {\n    w.0\n}\n",
        ),
    ]);
    // The child module stays a descendant of `a`, so it still sees `Wrap.0`.
    let result = fixture.mv("Wrap", "src/a/model.rs", "src/a/mod.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
}

#[path = "move_symbol_widen_tests.rs"]
mod widening;
