use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{criterion_group, criterion_main, Criterion};
use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_vm::{register_vm_stdlib, Chunk, Compiler, Vm, VmValue};

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn list_literal<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn compile(source: &str) -> Chunk {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().expect("bench source lexes");
    let mut parser = Parser::new(tokens);
    let program = parser.parse().expect("bench source parses");
    Compiler::new()
        .compile(&program)
        .expect("bench source compiles")
}

fn execute_counted(rt: &tokio::runtime::Runtime, chunk: &Chunk) -> (VmValue, usize) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(chunk).await.expect("bench source executes")
            })
            .await
    });
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    (result, ALLOCATIONS.load(Ordering::Relaxed))
}

fn callback_chunks() -> (Chunk, Chunk) {
    let numbers = list_literal(0..256);
    let words = list_literal((0..256).map(|index| format!("\"field_{index}\"")));
    let closure_source = format!(
        "pipeline default(task) {{ const out = [{numbers}].map({{ x -> x + 1 }})\nreturn len(out) }}"
    );
    let builtin_source = format!(
        "pipeline default(task) {{ const out = [{words}].map(snake_to_camel)\nreturn len(out) }}"
    );
    (compile(&closure_source), compile(&builtin_source))
}

fn list_callback_dispatch(c: &mut Criterion) {
    let (closure_chunk, builtin_chunk) = callback_chunks();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    c.bench_function("vm_list_map_closure_callback_vectorcall", |b| {
        b.iter(|| {
            let (result, allocations) = execute_counted(&rt, black_box(&closure_chunk));
            black_box((result, allocations))
        });
    });

    c.bench_function("vm_list_map_builtin_callback_vectorcall", |b| {
        b.iter(|| {
            let (result, allocations) = execute_counted(&rt, black_box(&builtin_chunk));
            black_box((result, allocations))
        });
    });
}

criterion_group!(benches, list_callback_dispatch);
criterion_main!(benches);
