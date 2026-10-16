//! Golden tests for `code_index.extract_function`. Each applied case pins the
//! whole rewritten file; each refusal pins the tag and that the file on disk
//! is byte-identical afterwards.

use super::*;

use std::fs;
use tempfile::tempdir;

use crate::code_index::state::IndexState;
use crate::code_index::CodeIndexCapability;

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).unwrap_or_else(|| panic!("missing field {key}")),
        _ => panic!("expected dict, got {value:?}"),
    }
}

fn s(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn names(value: &VmValue) -> Vec<String> {
    match value {
        VmValue::List(list) => list.iter().map(s).collect(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn int(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

/// One workspace holding `files`, indexed like a real session.
struct Workspace {
    dir: tempfile::TempDir,
    capability: CodeIndexCapability,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempdir().expect("tempdir");
        for (path, body) in files {
            let abs = dir.path().join(path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        let capability = CodeIndexCapability::new();
        {
            let shared = capability.shared();
            let mut guard = shared.lock().expect("mutex");
            let (state, _outcome) = IndexState::build_from_root(dir.path());
            *guard = Some(state);
        }
        Self { dir, capability }
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    fn extract(&self, pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert(harn_vm::value::intern_key(k), v.clone());
        }
        let result = run(&self.capability.shared(), &[VmValue::dict(map)]).expect("extract runs");
        // Every outcome, refusals included, must match the response schema
        // the registry enforces on the live path.
        crate::schemas::validate_response(
            BUILTIN,
            "code_index",
            "extract_function",
            result.clone(),
        )
        .unwrap_or_else(|err| panic!("response violates its schema: {err}\n{result:?}"));
        result
    }

    /// Run a request that must refuse with `tag` and leave `path` untouched.
    fn refuses(&self, path: &str, tag: &str, pairs: &[(&str, VmValue)]) -> VmValue {
        let before = fs::read(self.dir.path().join(path)).unwrap();
        let result = self.extract(pairs);
        assert_eq!(s(field(&result, "result")), tag, "{result:?}");
        assert!(matches!(field(&result, "applied"), VmValue::Bool(false)));
        assert_eq!(
            fs::read(self.dir.path().join(path)).unwrap(),
            before,
            "a refusal must leave the file byte-identical"
        );
        result
    }
}

fn lines(path: &str, start: i64, end: i64, name: &str) -> Vec<(&'static str, VmValue)> {
    vec![
        ("path", vm_string(path)),
        ("start_line", VmValue::Int(start)),
        ("end_line", VmValue::Int(end)),
        ("new_name", vm_string(name)),
    ]
}

fn with(
    mut pairs: Vec<(&'static str, VmValue)>,
    key: &'static str,
    value: &str,
) -> Vec<(&'static str, VmValue)> {
    pairs.push((key, vm_string(value)));
    pairs
}

fn applied(result: &VmValue) {
    assert_eq!(s(field(result, "result")), "applied", "{result:?}");
    assert!(matches!(field(result, "applied"), VmValue::Bool(true)));
}

// === Rust ===

const RUST_TOTAL: &str = "pub fn total(items: &[u32], bonus: u32) -> u32 {
    let base = items.len() as u32;
    let doubled = base * 2 + bonus;
    doubled + 1
}
";

#[test]
fn rust_statement_run_with_one_output() {
    let ws = Workspace::new(&[("src/lib.rs", RUST_TOTAL)]);
    let result = ws.extract(&with(
        lines("src/lib.rs", 2, 3, "scaled"),
        "signature",
        "fn scaled(items: &[u32], bonus: u32) -> u32",
    ));
    applied(&result);
    assert_eq!(names(field(&result, "inputs")), ["items", "bonus"]);
    assert_eq!(names(field(&result, "outputs")), ["doubled"]);
    assert_eq!(
        ws.read("src/lib.rs"),
        "pub fn total(items: &[u32], bonus: u32) -> u32 {
    let doubled = scaled(items, bonus);
    doubled + 1
}

fn scaled(items: &[u32], bonus: u32) -> u32 {
    let base = items.len() as u32;
    let doubled = base * 2 + bonus;
    doubled
}
"
    );
}

#[test]
fn rust_statement_run_with_two_outputs_returns_a_tuple() {
    let source = "pub fn span(values: &[i64]) -> String {
    let mut low = i64::MAX;
    let mut high = i64::MIN;
    for v in values {
        low = low.min(*v);
        high = high.max(*v);
    }
    format!(\"{}..{}\", low, high)
}
";
    let ws = Workspace::new(&[("src/lib.rs", source)]);
    let result = ws.extract(&with(
        lines("src/lib.rs", 2, 7, "bounds"),
        "signature",
        "fn bounds(values: &[i64]) -> (i64, i64)",
    ));
    applied(&result);
    assert_eq!(names(field(&result, "outputs")), ["low", "high"]);
    assert_eq!(
        ws.read("src/lib.rs"),
        "pub fn span(values: &[i64]) -> String {
    let (low, high) = bounds(values);
    format!(\"{}..{}\", low, high)
}

fn bounds(values: &[i64]) -> (i64, i64) {
    let mut low = i64::MAX;
    let mut high = i64::MIN;
    for v in values {
        low = low.min(*v);
        high = high.max(*v);
    }
    (low, high)
}
"
    );
}

const RUST_CLOSURES: &str = "pub fn parse_all(input: &str) -> Vec<u32> {
    input.lines().filter_map(|line| {
        let trimmed = line.trim();
        trimmed.parse().ok()
    }).collect()
}

pub fn parse_last(input: &str) -> Option<u32> {
    input.lines().filter_map(|line| {
        let trimmed = line.trim();
        trimmed.parse().ok()
    }).last()
}
";

#[test]
fn rust_closure_body_becomes_a_function_over_its_parameters() {
    let ws = Workspace::new(&[("src/lib.rs", RUST_CLOSURES)]);
    let mut request = with(
        lines("src/lib.rs", 3, 4, "parse_line"),
        "signature",
        "fn parse_line(line: &str) -> Option<u32>",
    );
    request.push(("all_occurrences", VmValue::Bool(false)));
    let result = ws.extract(&request);
    applied(&result);
    assert_eq!(s(field(field(&result, "region"), "kind")), "closure_body");
    assert_eq!(names(field(&result, "inputs")), ["line"]);
    assert_eq!(
        ws.read("src/lib.rs"),
        "pub fn parse_all(input: &str) -> Vec<u32> {
    input.lines().filter_map(|line| parse_line(line)).collect()
}

fn parse_line(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    trimmed.parse().ok()
}

pub fn parse_last(input: &str) -> Option<u32> {
    input.lines().filter_map(|line| {
        let trimmed = line.trim();
        trimmed.parse().ok()
    }).last()
}
"
    );
}

#[test]
fn rust_all_occurrences_replaces_both_copies() {
    let ws = Workspace::new(&[("src/lib.rs", RUST_CLOSURES)]);
    let result = ws.extract(&with(
        lines("src/lib.rs", 3, 4, "parse_line"),
        "signature",
        "fn parse_line(line: &str) -> Option<u32>",
    ));
    applied(&result);
    assert_eq!(int(field(&result, "occurrences_replaced")), 2);
    assert_eq!(
        ws.read("src/lib.rs"),
        "pub fn parse_all(input: &str) -> Vec<u32> {
    input.lines().filter_map(|line| parse_line(line)).collect()
}

fn parse_line(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    trimmed.parse().ok()
}

pub fn parse_last(input: &str) -> Option<u32> {
    input.lines().filter_map(|line| parse_line(line)).last()
}
"
    );
}

#[test]
fn rust_continue_out_of_the_region_is_refused() {
    let source = "pub fn sum_odd(values: &[i32]) -> i32 {
    let mut total = 0;
    for v in values {
        if v % 2 == 0 {
            continue;
        }
        total += v;
    }
    total
}
";
    let ws = Workspace::new(&[("src/lib.rs", source)]);
    let result = ws.refuses(
        "src/lib.rs",
        "control_flow_escapes",
        &with(
            lines("src/lib.rs", 4, 7, "add_odd"),
            "signature",
            "fn add_odd(v: &i32, total: &mut i32)",
        ),
    );
    let VmValue::List(sites) = field(&result, "sites") else {
        panic!("sites list");
    };
    assert_eq!(s(field(&sites[0], "kind")), "continue");
    assert_eq!(int(field(&sites[0], "line")), 5);
}

#[test]
fn rust_signature_with_wrong_parameters_is_refused() {
    let ws = Workspace::new(&[("src/lib.rs", RUST_TOTAL)]);
    let result = ws.refuses(
        "src/lib.rs",
        "signature_mismatch",
        &with(
            lines("src/lib.rs", 2, 3, "scaled"),
            "signature",
            "fn scaled(items: &[u32], extra: u32) -> u32",
        ),
    );
    assert!(s(field(&result, "details")).contains("[items, bonus]"));
}

#[test]
fn rust_without_signature_reports_the_types_it_needs() {
    let ws = Workspace::new(&[("src/lib.rs", RUST_TOTAL)]);
    let result = ws.refuses(
        "src/lib.rs",
        "types_required",
        &lines("src/lib.rs", 2, 3, "scaled"),
    );
    assert_eq!(names(field(&result, "inputs")), ["items", "bonus"]);
    assert_eq!(names(field(&result, "outputs")), ["doubled"]);
}

// === TypeScript ===

const TS_TOTAL: &str = "export function total(items: number[], bonus: number): number {
  const base = items.length
  const doubled = base * 2 + bonus
  return doubled + 1
}
";

const STRICT: &str =
    "{\n  // comments are allowed\n  \"compilerOptions\": { \"strict\": true }\n}\n";

#[test]
fn typescript_statement_run_with_one_output() {
    let ws = Workspace::new(&[("src/a.ts", TS_TOTAL)]);
    let result = ws.extract(&lines("src/a.ts", 2, 3, "scaled"));
    applied(&result);
    assert_eq!(
        ws.read("src/a.ts"),
        "export function total(items: number[], bonus: number): number {
  const doubled = scaled(items, bonus)
  return doubled + 1
}

function scaled(items, bonus) {
  const base = items.length
  const doubled = base * 2 + bonus
  return doubled
}
"
    );
}

#[test]
fn typescript_two_outputs_destructure_an_object() {
    let source = "export function span(values: number[]): string {
  let low = Infinity
  let high = -Infinity
  for (const v of values) {
    low = Math.min(low, v)
    high = Math.max(high, v)
  }
  return `${low}..${high}`
}
";
    let ws = Workspace::new(&[("src/a.ts", source), ("tsconfig.json", STRICT)]);
    let result = ws.extract(&with(
        lines("src/a.ts", 2, 7, "bounds"),
        "signature",
        "function bounds(values: number[]): { low: number; high: number }",
    ));
    applied(&result);
    assert_eq!(
        ws.read("src/a.ts"),
        "export function span(values: number[]): string {
  let { low, high } = bounds(values)
  return `${low}..${high}`
}

function bounds(values: number[]): { low: number; high: number } {
  let low = Infinity
  let high = -Infinity
  for (const v of values) {
    low = Math.min(low, v)
    high = Math.max(high, v)
  }
  return { low, high }
}
"
    );
}

const TS_CLOSURES: &str = "export function parseAll(input: string): number[] {
  return input.split(\"\\n\").flatMap((line) => {
    const n = Number(line.trim());
    return Number.isInteger(n) ? [n] : [];
  });
}

export function parseLast(input: string): number | undefined {
  return input.split(\"\\n\").flatMap((line) => {
    const n = Number(line.trim());
    return Number.isInteger(n) ? [n] : [];
  }).at(-1);
}
";

#[test]
fn typescript_arrow_body_and_its_copy_become_one_function() {
    let ws = Workspace::new(&[("src/a.ts", TS_CLOSURES), ("tsconfig.json", STRICT)]);
    let result = ws.extract(&with(
        lines("src/a.ts", 3, 4, "parseLine"),
        "signature",
        "function parseLine(line: string): number[]",
    ));
    applied(&result);
    assert_eq!(int(field(&result, "occurrences_replaced")), 2);
    assert_eq!(
        ws.read("src/a.ts"),
        "export function parseAll(input: string): number[] {
  return input.split(\"\\n\").flatMap((line) => parseLine(line));
}

function parseLine(line: string): number[] {
  const n = Number(line.trim());
  return Number.isInteger(n) ? [n] : [];
}

export function parseLast(input: string): number | undefined {
  return input.split(\"\\n\").flatMap((line) => parseLine(line)).at(-1);
}
"
    );
}

#[test]
fn typescript_statement_copies_are_replaced_with_semicolons() {
    let source = "export function a(x: number): number {
  const y = x * 2;
  const z = y + 1;
  return z * 10;
}

export function b(x: number): number {
  const y = x * 2;
  const z = y + 1;
  return z - 10;
}
";
    let ws = Workspace::new(&[("src/a.ts", source)]);
    let result = ws.extract(&lines("src/a.ts", 2, 3, "step"));
    applied(&result);
    assert_eq!(
        ws.read("src/a.ts"),
        "export function a(x: number): number {
  const z = step(x);
  return z * 10;
}

function step(x) {
  const y = x * 2;
  const z = y + 1;
  return z;
}

export function b(x: number): number {
  const z = step(x);
  return z - 10;
}
"
    );
}

#[test]
fn typescript_continue_out_of_the_region_is_refused() {
    let source = "export function sumOdd(values: number[]): number {
  let total = 0
  for (const v of values) {
    if (v % 2 === 0) {
      continue
    }
    total += v
  }
  return total
}
";
    let ws = Workspace::new(&[("src/a.ts", source)]);
    ws.refuses(
        "src/a.ts",
        "control_flow_escapes",
        &lines("src/a.ts", 4, 7, "addOdd"),
    );
}

#[test]
fn typescript_signature_with_another_name_is_refused() {
    let ws = Workspace::new(&[("src/a.ts", TS_TOTAL)]);
    ws.refuses(
        "src/a.ts",
        "signature_mismatch",
        &with(
            lines("src/a.ts", 2, 3, "scaled"),
            "signature",
            "function doubled(items: number[], bonus: number): number",
        ),
    );
}

#[test]
fn strict_typescript_without_signature_reports_the_types_it_needs() {
    let ws = Workspace::new(&[("src/a.ts", TS_TOTAL), ("tsconfig.json", STRICT)]);
    let result = ws.refuses(
        "src/a.ts",
        "types_required",
        &lines("src/a.ts", 2, 3, "scaled"),
    );
    assert_eq!(names(field(&result, "inputs")), ["items", "bonus"]);
}

// === Python ===

const PY_ORDERS: &str = "def parse_orders(text):
    found = []
    for line in text.splitlines():
        if \"|\" not in line:
            continue
        code, raw = line.split(\"|\", 1)
        if not raw.strip().isdigit():
            continue
        found.append((code.strip(), int(raw)))
    return found


def last_order(text):
    found = []
    for line in text.splitlines():
        if \"|\" not in line:
            continue
        code, raw = line.split(\"|\", 1)
        if not raw.strip().isdigit():
            continue
        found.append((code.strip(), int(raw)))
    return found[-1] if found else None
";

#[test]
fn python_statement_with_two_outputs_and_its_copy() {
    let ws = Workspace::new(&[("orders.py", PY_ORDERS)]);
    let result = ws.extract(&lines("orders.py", 6, 6, "split_order"));
    applied(&result);
    assert_eq!(names(field(&result, "inputs")), ["line"]);
    assert_eq!(names(field(&result, "outputs")), ["code", "raw"]);
    assert_eq!(int(field(&result, "occurrences_replaced")), 2);
    let after = ws.read("orders.py");
    assert_eq!(after.matches("code, raw = split_order(line)").count(), 2);
    assert!(after.contains(
        "    return found


def split_order(line):
    code, raw = line.split(\"|\", 1)
    return code, raw


def last_order(text):"
    ));
    assert_eq!(after.matches("line.split(\"|\", 1)").count(), 1);
}

#[test]
fn python_statement_run_with_one_output_needs_no_types() {
    let source = "def total(items, bonus):
    base = len(items)
    doubled = base * 2 + bonus
    return doubled + 1
";
    let ws = Workspace::new(&[("calc.py", source)]);
    applied(&ws.extract(&lines("calc.py", 2, 3, "scaled")));
    assert_eq!(
        ws.read("calc.py"),
        "def total(items, bonus):
    doubled = scaled(items, bonus)
    return doubled + 1


def scaled(items, bonus):
    base = len(items)
    doubled = base * 2 + bonus
    return doubled
"
    );
}

#[test]
fn python_lambda_body_captures_outer_locals_after_its_parameters() {
    let source = "def rank(rows, weight):
    return sorted(rows, key=lambda row: row.score * weight)
";
    let ws = Workspace::new(&[("rank.py", source)]);
    let result = ws.extract(&[
        ("path", vm_string("rank.py")),
        ("region", vm_string("row.score * weight")),
        ("new_name", vm_string("weighted")),
    ]);
    applied(&result);
    assert_eq!(names(field(&result, "inputs")), ["row", "weight"]);
    assert_eq!(
        ws.read("rank.py"),
        "def rank(rows, weight):
    return sorted(rows, key=lambda row: weighted(row, weight))


def weighted(row, weight):
    return row.score * weight
"
    );
}

#[test]
fn python_continue_out_of_the_region_is_refused() {
    let ws = Workspace::new(&[("orders.py", PY_ORDERS)]);
    let result = ws.refuses(
        "orders.py",
        "control_flow_escapes",
        &lines("orders.py", 4, 9, "parse_line"),
    );
    let VmValue::List(sites) = field(&result, "sites") else {
        panic!("sites list");
    };
    assert_eq!(sites.len(), 2);
}

#[test]
fn python_signature_with_wrong_parameters_is_refused() {
    let ws = Workspace::new(&[("orders.py", PY_ORDERS)]);
    ws.refuses(
        "orders.py",
        "signature_mismatch",
        &with(
            lines("orders.py", 6, 6, "split_order"),
            "signature",
            "def split_order(text: str):",
        ),
    );
}

#[test]
fn helper_text_is_inserted_verbatim_and_every_copy_calls_it() {
    let ws = Workspace::new(&[("orders.py", PY_ORDERS)]);
    let helper = "def split_order(line: str) -> tuple[str, str]:\n    \"\"\"Split one order line.\"\"\"\n    code, raw = line.split(\"|\", 1)\n    return code, raw\n";
    applied(&ws.extract(&with(
        lines("orders.py", 6, 6, "split_order"),
        "helper",
        helper,
    )));
    let after = ws.read("orders.py");
    assert!(after.contains("\n\n\ndef split_order(line: str) -> tuple[str, str]:\n    \"\"\"Split one order line.\"\"\"\n"));
    assert_eq!(after.matches("split_order(line)").count(), 2);
}

// === Shared refusals ===

#[test]
fn name_conflict_ambiguity_and_unsupported_languages_refuse() {
    let ws = Workspace::new(&[
        ("orders.py", PY_ORDERS),
        ("main.go", "package main\n\nfunc main() {}\n"),
    ]);
    let conflict = ws.refuses(
        "orders.py",
        "name_conflict",
        &lines("orders.py", 6, 6, "last_order"),
    );
    let VmValue::List(conflicts) = field(&conflict, "conflicts") else {
        panic!("conflicts list");
    };
    assert_eq!(int(field(&conflicts[0], "line")), 13);
    let mut request = vec![
        ("path", vm_string("orders.py")),
        ("region", vm_string("code, raw = line.split(\"|\", 1)")),
        ("new_name", vm_string("split_order")),
    ];
    request.push(("all_occurrences", VmValue::Bool(false)));
    let result = ws.refuses("orders.py", "ambiguous_symbol", &request);
    let VmValue::List(candidates) = field(&result, "warnings") else {
        panic!("candidates list");
    };
    assert_eq!(candidates.len(), 2);
    ws.refuses(
        "orders.py",
        "no_match",
        &lines("orders.py", 40, 41, "split_order"),
    );
    // Half a statement lines up with nothing.
    ws.refuses(
        "orders.py",
        "no_match",
        &[
            ("path", vm_string("orders.py")),
            ("region", vm_string("line.split(\"|\"")),
            ("new_name", vm_string("split_order")),
        ],
    );
    ws.refuses(
        "main.go",
        "unsupported_language",
        &lines("main.go", 3, 3, "helper"),
    );
}

#[test]
fn dry_run_reports_the_plan_without_writing() {
    let ws = Workspace::new(&[("calc.py", "def f(a):\n    b = a + 1\n    return b\n")]);
    let mut request = lines("calc.py", 2, 2, "inc");
    request.push(("dry_run", VmValue::Bool(true)));
    let result = ws.extract(&request);
    assert_eq!(s(field(&result, "result")), "applied");
    assert!(matches!(field(&result, "dry_run"), VmValue::Bool(true)));
    assert!(matches!(field(&result, "applied"), VmValue::Bool(false)));
    assert_eq!(s(field(&result, "call")), "b = inc(a)");
    assert_eq!(
        s(field(&result, "helper")),
        "def inc(a):\n    b = a + 1\n    return b"
    );
    assert_eq!(
        ws.read("calc.py"),
        "def f(a):\n    b = a + 1\n    return b\n"
    );
}

#[test]
fn capability_column_matches_the_planner_dialects() {
    for language in Language::all() {
        assert_eq!(
            Dialect::of(*language).is_some(),
            language.edit_capabilities().extract_function,
            "{}",
            language.name()
        );
    }
}

#[test]
fn a_file_that_does_not_parse_is_refused_before_any_analysis() {
    // tree-sitter recovers from each of these Python indentation errors
    // without an ERROR node, so the parse alone would let them through.
    for (name, source) in [
        (
            "dedent.py",
            "def f(a):\n    b = a + 1\n    return b\n  c = 2\n",
        ),
        ("tab.py", "def f(a):\n    b = a + 1\n\treturn b\n"),
        (
            "indent.py",
            "def f(a):\n    b = a + 1\n        c = b\n    return c\n",
        ),
        ("error.py", "def f(a):\n    b = (a + 1\n    return b\n"),
        (
            "error.rs",
            "fn f(a: i32) -> i32 {\n    let b = a + ;\n    b\n}\n",
        ),
        ("error.ts", "function f(a: number) {\n  const b = a +\n}\n"),
    ] {
        let ws = Workspace::new(&[(name, source)]);
        let result = ws.refuses(name, "syntax_error", &lines(name, 2, 2, "g"));
        assert!(
            s(field(&result, "details")).contains("does not parse"),
            "{name}"
        );
    }
}

const PY_METHODS: &str = "class Account:
    def total(self, extra):
        base = self.balance + extra
        return base * 2

    def ratio(self):
        scaled = super().ratio() * 2
        return scaled

    def secret(self):
        hidden = self.__pin + 1
        return hidden
";

#[test]
fn python_passes_self_as_an_ordinary_parameter() {
    // A Python receiver is a positional parameter, so the module-level
    // helper takes it like any other input and the call passes it.
    let ws = Workspace::new(&[("acct.py", PY_METHODS)]);
    let result = ws.extract(&lines("acct.py", 3, 3, "with_extra"));
    applied(&result);
    assert_eq!(names(field(&result, "inputs")), ["self", "extra"]);
    let after = ws.read("acct.py");
    assert!(after.contains("        base = with_extra(self, extra)\n"));
    assert!(after.ends_with(
        "\n\n\ndef with_extra(self, extra):\n    base = self.balance + extra\n    return base\n"
    ));
}

#[test]
fn class_bound_regions_are_refused_in_every_language() {
    let ws = Workspace::new(&[
        ("acct.py", PY_METHODS),
        (
            "acct.rs",
            "struct A { n: i32 }\nimpl A {\n    fn f(&self) -> i32 {\n        let m = self.n + 1;\n        m\n    }\n}\n",
        ),
        (
            "acct.ts",
            "class A {\n  n = 1\n  f(): number {\n    const m = this.n + 1\n    return m\n  }\n}\n",
        ),
    ]);
    for (path, line, needle) in [
        ("acct.py", 7, "super()"),
        ("acct.py", 11, "__pin"),
        ("acct.rs", 4, "`self`"),
        ("acct.ts", 4, "`this`"),
    ] {
        let mut request = lines(path, line, line, "helper");
        if path.ends_with(".rs") {
            request.push(("signature", vm_string("fn helper() -> i32")));
        }
        let result = ws.refuses(path, "unsupported_region", &request);
        assert!(
            s(field(&result, "details")).contains(needle),
            "{path}: {result:?}"
        );
    }
}
