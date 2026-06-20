use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::llm::bench_internals::llm_options_roundtrip_probe;
use harn_vm::VmValue;

const KEY_COUNTS: [usize; 4] = [1, 5, 25, 100];

struct Fixture {
    key_count: usize,
    args: Vec<VmValue>,
    options: Option<harn_vm::value::DictMap>,
}

fn string(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn dict(entries: harn_vm::value::DictMap) -> VmValue {
    VmValue::Dict(Arc::new(entries))
}

fn override_value(index: usize) -> VmValue {
    match index % 5 {
        0 => VmValue::Int(index as i64),
        1 => VmValue::Float(index as f64 / 10.0),
        2 => string(&format!("value-{index:03}")),
        3 => VmValue::Bool(index.is_multiple_of(2)),
        _ => VmValue::List(Arc::new(vec![
            string("nested"),
            VmValue::Int(index as i64),
            VmValue::Bool(true),
        ])),
    }
}

fn provider_overrides() -> VmValue {
    let mut overrides = harn_vm::value::DictMap::new();
    overrides.insert(
        harn_vm::value::intern_key("override_000"),
        override_value(0),
    );
    dict(overrides)
}

fn build_options(key_count: usize) -> harn_vm::value::DictMap {
    let mut options = harn_vm::value::DictMap::new();
    options.insert(harn_vm::value::intern_key("provider"), string("mock"));

    if key_count >= 2 {
        options.insert(harn_vm::value::intern_key("model"), string("gpt-4o-mini"));
    }
    if key_count >= 3 {
        options.insert(harn_vm::value::intern_key("stream"), VmValue::Bool(false));
    }
    if key_count >= 4 {
        options.insert(harn_vm::value::intern_key("max_tokens"), VmValue::Int(512));
    }
    if key_count >= 5 {
        options.insert(harn_vm::value::intern_key("mock"), provider_overrides());
    }

    while options.len() < key_count {
        let index = options.len();
        options.insert(
            harn_vm::value::intern_key(&format!("passthrough_{index:03}")),
            override_value(index),
        );
    }

    options
}

fn fixture(key_count: usize) -> Fixture {
    let options = build_options(key_count);
    let args = vec![
        string("Summarize the benchmark fixture."),
        VmValue::Nil,
        dict(options.clone()),
    ];
    Fixture {
        key_count,
        args,
        options: Some(options),
    }
}

fn bench_llm_options_roundtrip(c: &mut Criterion) {
    let fixtures: Vec<Fixture> = KEY_COUNTS.into_iter().map(fixture).collect();
    let mut group = c.benchmark_group("llm_options_roundtrip");

    for fixture in &fixtures {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("keys_{:03}", fixture.key_count)),
            fixture,
            |b, fixture| {
                b.iter(|| {
                    black_box(
                        llm_options_roundtrip_probe(
                            black_box(&fixture.args),
                            black_box(&fixture.options),
                        )
                        .expect("benchmark fixture should parse"),
                    )
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_llm_options_roundtrip);
criterion_main!(benches);
