//! Transcript-projection benchmark.
//!
//! `transcript_project` (and the shared `project_transcript` path the
//! agent-session host calls) runs once per agent turn to derive the
//! model-visible message prefix from the raw transcript. Arc-COW clones
//! of the transcript value are cheap; the projection *scans* are not —
//! they walk every message (and, for the scanning policies, correlate
//! tool calls with tool results across messages). The stage-loop
//! inversion re-architecture moves this per-turn work behind more VM
//! crossings, so this suite pins its cost at realistic transcript sizes
//! (~10k/50k/100k tokens, at the ~4 chars/token heuristic the runtime
//! itself uses for reclaim accounting).
//!
//! Policies covered:
//! - `raw` — passthrough copy; the floor.
//! - `clean_tool_repair` — the heaviest scan: correlates failed tool
//!   calls with later successful retries across the whole transcript.
//! - `squash_failed_calls` — per-message failed-call scan.
//! - `reachability_gc` — root-set construction + identifier scan over
//!   stale tool-result bodies.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use harn_vm::bench_internals::transcript_projection;
use harn_vm::{register_vm_stdlib, reset_thread_local_state, AsyncBuiltinCtx, Vm, VmValue};
use serde_json::{json, Value as JsonValue};
use tokio::runtime::{Builder, Runtime};

const TOKEN_TARGETS: [usize; 3] = [10_000, 50_000, 100_000];
const POLICIES: [&str; 4] = [
    "raw",
    "clean_tool_repair",
    "squash_failed_calls",
    "reachability_gc",
];
/// The same chars-per-token heuristic `reachability_gc` uses for
/// reclaim accounting.
const CHARS_PER_TOKEN: usize = 4;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: This allocator only observes calls before delegating to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: This allocator only observes calls before delegating to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout are passed through unchanged from the allocator caller.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The pointer, layout, and new size are passed through unchanged.
        let ptr = unsafe { System.realloc(ptr, layout, new_size) };
        record_allocation(ptr, new_size);
        ptr
    }
}

fn record_allocation(ptr: *mut u8, bytes: usize) {
    if !ptr.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("benchmark runtime")
}

struct TranscriptFixture {
    target_tokens: usize,
    approx_tokens: usize,
    message_count: usize,
    transcript: VmValue,
}

/// Deterministic filler prose so tool-result bodies have realistic bulk
/// without being compressible to nothing by the identifier scanner.
fn filler(seed: usize, chars: usize) -> String {
    let words = [
        "checked", "warning", "resolved", "pending", "matched", "skipped", "compiled", "linked",
        "cached", "verified",
    ];
    let mut out = String::with_capacity(chars + 16);
    let mut index = seed;
    while out.len() < chars {
        out.push_str(words[index % words.len()]);
        out.push(' ');
        out.push_str(&index.to_string());
        out.push(' ');
        index += 1;
    }
    out
}

/// One "agent turn" group of messages in provider block format:
/// assistant text + tool_use, then the matching user tool_result. Every
/// 7th turn emits a failed call followed by a successful retry of the
/// same tool, so the repair-scanning policies have real work to do.
fn turn_messages(turn: usize) -> Vec<JsonValue> {
    let tool = ["run_command", "read_file", "grep", "edit_file"][turn % 4];
    let call_id = format!("call_{turn:05}");
    let mut messages = vec![
        json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": format!("Working step {turn}: inspecting crates/module_{}/src/lib.rs next.", turn % 23)},
                {"type": "tool_use", "id": call_id, "name": tool, "input": {"path": format!("crates/module_{}/src/lib.rs", turn % 23), "query": format!("symbol_{turn}")}},
            ],
        }),
        json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": call_id, "content": filler(turn, 420 + (turn % 5) * 90)},
            ],
        }),
    ];
    if turn % 7 == 3 {
        let failed_id = format!("call_{turn:05}_fail");
        let retry_id = format!("call_{turn:05}_retry");
        messages.push(json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": failed_id, "name": tool, "input": {"path": "does/not/exist.rs"}},
            ],
        }));
        messages.push(json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": failed_id, "is_error": true, "content": "Error: file not found: does/not/exist.rs"},
            ],
        }));
        messages.push(json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": retry_id, "name": tool, "input": {"path": format!("crates/module_{}/src/lib.rs", turn % 23)}},
            ],
        }));
        messages.push(json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": retry_id, "content": filler(turn + 1, 380)},
            ],
        }));
    }
    messages
}

fn fixture(target_tokens: usize) -> TranscriptFixture {
    let mut messages = vec![json!({
        "role": "user",
        "content": "Fix the failing build across the workspace and report what changed.",
    })];
    let mut chars = 0usize;
    let mut turn = 0usize;
    while chars < target_tokens * CHARS_PER_TOKEN {
        for message in turn_messages(turn) {
            chars += message.to_string().len();
            messages.push(message);
        }
        turn += 1;
    }
    TranscriptFixture {
        target_tokens,
        approx_tokens: chars / CHARS_PER_TOKEN,
        message_count: messages.len(),
        transcript: transcript_projection::transcript_value_from_messages(&messages),
    }
}

fn projection_ctx() -> AsyncBuiltinCtx {
    reset_thread_local_state();
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    AsyncBuiltinCtx::from_vm(vm)
}

fn project(
    runtime: &Runtime,
    ctx: &AsyncBuiltinCtx,
    fixture: &TranscriptFixture,
    options: &JsonValue,
) -> VmValue {
    runtime
        .block_on(transcript_projection::project_for_bench(
            ctx,
            &fixture.transcript,
            options,
        ))
        .expect("projection should succeed")
}

fn measure_once(
    runtime: &Runtime,
    ctx: &AsyncBuiltinCtx,
    fixture: &TranscriptFixture,
    policy: &str,
    options: &JsonValue,
) {
    let result = project(runtime, ctx, fixture, options);
    assert!(
        result.as_dict().is_some(),
        "projection must return a result dict"
    );

    let started = Instant::now();
    let _ = black_box(project(runtime, ctx, fixture, options));
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    let _ = black_box(project(runtime, ctx, fixture, options));
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);

    eprintln!(
        "transcript_projection/{policy}/tokens_{} sample: {:.3} ms/projection ({} messages, ~{} tokens), {} allocations, {:.1} KiB allocated",
        fixture.target_tokens,
        elapsed_ms,
        fixture.message_count,
        fixture.approx_tokens,
        ALLOCATION_COUNT.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed) as f64 / 1024.0,
    );
}

fn bench_transcript_projection(c: &mut Criterion) {
    let runtime = runtime();
    let ctx = projection_ctx();
    let fixtures: Vec<TranscriptFixture> = TOKEN_TARGETS.into_iter().map(fixture).collect();

    for policy in POLICIES {
        let options = json!({ "policy": policy });
        for fixture in &fixtures {
            measure_once(&runtime, &ctx, fixture, policy, &options);
        }

        let mut group = c.benchmark_group(format!("transcript_projection/{policy}"));
        group.sample_size(20);
        for fixture in &fixtures {
            group.throughput(Throughput::Elements(fixture.approx_tokens as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("tokens_{}", fixture.target_tokens)),
                fixture,
                |b, fixture| {
                    b.iter(|| project(&runtime, &ctx, black_box(fixture), &options));
                },
            );
        }
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = bench_transcript_projection
}
criterion_main!(benches);
