//! Per-phase compiler front-end benchmarks: lex, parse, typecheck, compile.
//!
//! Each phase is timed against the *output of the previous phase* (tokens for
//! parse, the AST for typecheck/compile), so a regression localizes to the
//! phase that caused it instead of smearing across an end-to-end number.
//! See `bench/frontend/README.md` for the corpus rationale and the house
//! before/after method.

use criterion::{criterion_group, criterion_main, Criterion};
use std::fmt::Write as _;
use std::hint::black_box;

/// Deterministic synthetic module exercising the constructs that dominate
/// real Harn sources. `blocks` scales the module linearly (~55 lines per
/// block), so phase timings stay comparable when the corpus grows.
fn synthetic_module(blocks: usize) -> String {
    let mut src = String::new();
    src.push_str("// synthetic front-end benchmark corpus\n");
    src.push_str("type Mode = \"fast\" | \"slow\" | \"safe\"\n");
    src.push_str("struct Point { x: float, y: float, label: string }\n");
    src.push_str("enum Shape { Circle(radius: float), Rect(w: float, h: float) }\n\n");
    for i in 0..blocks {
        write!(
            src,
            r#"
fn scale_{i}(p: Point, factor: float = 2.0) -> Point {{
  let scaled = Point {{ x: p.x * factor, y: p.y * factor, label: "s${{factor}}:${{p.label}}" }}
  return scaled
}}

fn area_{i}(shape: Shape) -> float {{
  match shape {{
    Circle(radius) -> {{ return 3.14159 * radius * radius }}
    Rect(w, h) -> {{ return w * h }}
  }}
}}

fn summarize_{i}(items: list<int>, mode: Mode) -> dict {{
  let out = {{count: items.count(), mode: mode, tags: ["a", "b", "c"]}}
  let total = 0
  for value in items {{
    if value % 2 == 0 && value > 3 {{
      total = total + value
    }} else {{
      total = total - 1
    }}
  }}
  out["total"] = total
  out["evens"] = items.filter({{ value -> value % 2 == 0 }}).map({{ value -> value * 2 }})
  while total > 100 {{
    total = total / 2
  }}
  return out
}}

fn describe_{i}(input: string?) -> string {{
  const fallback = "none"
  let name = input ?? fallback
  let banner = """
    report for ${{name}}
    total sections: {i}
  """
  return banner
}}
"#
        )
        .expect("writing to a String cannot fail");
    }
    src
}

fn bench_frontend_phases(c: &mut Criterion) {
    let source = synthetic_module(40);
    let tokens = harn_lexer::Lexer::new(&source)
        .tokenize()
        .expect("corpus lexes");
    let program = harn_parser::Parser::new(tokens.clone())
        .parse()
        .expect("corpus parses");
    // The corpus must stay diagnostics-free so typecheck timings measure the
    // inference walk, not diagnostic rendering.
    let checker = harn_parser::TypeChecker::new();
    let diagnostics = checker.check(&program);
    assert!(
        diagnostics.is_empty(),
        "synthetic corpus must typecheck cleanly: {diagnostics:?}"
    );
    harn_kernel::Compiler::new()
        .compile(&program)
        .expect("corpus compiles");

    let mut group = c.benchmark_group("frontend_phases");
    group.bench_function("lex", |b| {
        b.iter(|| {
            harn_lexer::Lexer::new(black_box(&source))
                .tokenize()
                .expect("corpus lexes")
        });
    });
    group.bench_function("parse", |b| {
        b.iter(|| {
            harn_parser::Parser::new(black_box(tokens.clone()))
                .parse()
                .expect("corpus parses")
        });
    });
    group.bench_function("typecheck", |b| {
        b.iter(|| {
            let checker = harn_parser::TypeChecker::new();
            checker.check(black_box(&program))
        });
    });
    group.bench_function("compile", |b| {
        b.iter(|| {
            harn_kernel::Compiler::new()
                .compile(black_box(&program))
                .expect("corpus compiles")
        });
    });
    group.finish();
}

criterion_group!(benches, bench_frontend_phases);
criterion_main!(benches);
