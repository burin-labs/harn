//! Workflow-bundle end-to-end benchmark.
//!
//! Exercises the validation + graph normalization + portable-bundle export
//! path that runs whenever a host previews or ships a bundle. The fixture
//! exercises every editable-field, trigger, capsule, and connector branch
//! so allocation hotspots in the export layer surface here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::orchestration::{
    export_workflow_bundle_graph, preview_workflow_bundle, validate_workflow_bundle, WorkflowBundle,
};

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: We pass through unchanged to the system allocator and only observe.
        let ptr = unsafe { System.alloc(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: We pass through unchanged to the system allocator and only observe.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Passed through unchanged from the allocator caller.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Passed through unchanged from the allocator caller.
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

fn fixture_bundle() -> WorkflowBundle {
    serde_json::from_str(FIXTURE_JSON).expect("fixture should parse")
}

fn allocation_stats(bundle: &WorkflowBundle, samples: u64, kind: ExportKind) -> (f64, f64) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for _ in 0..samples {
        TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
        kind.run(bundle);
        TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    }
    (
        ALLOCATION_COUNT.load(Ordering::Relaxed) as f64 / samples as f64,
        ALLOCATED_BYTES.load(Ordering::Relaxed) as f64 / samples as f64,
    )
}

#[derive(Clone, Copy)]
enum ExportKind {
    Validate,
    Preview,
    Export,
}

impl ExportKind {
    fn label(self) -> &'static str {
        match self {
            ExportKind::Validate => "validate",
            ExportKind::Preview => "preview",
            ExportKind::Export => "export_graph",
        }
    }

    fn run(self, bundle: &WorkflowBundle) {
        match self {
            ExportKind::Validate => {
                black_box(validate_workflow_bundle(bundle));
            }
            ExportKind::Preview => {
                black_box(preview_workflow_bundle(bundle));
            }
            ExportKind::Export => {
                let validation = validate_workflow_bundle(bundle);
                black_box(export_workflow_bundle_graph(bundle, &validation));
            }
        }
    }
}

fn bench_workflow_bundle(c: &mut Criterion) {
    let bundle = fixture_bundle();

    let mut group = c.benchmark_group("workflow_bundle");
    for kind in [
        ExportKind::Validate,
        ExportKind::Preview,
        ExportKind::Export,
    ] {
        let (allocations, allocated_bytes) = allocation_stats(&bundle, 64, kind);
        eprintln!(
            "workflow_bundle/{}: {:.2} allocations/run, {:.1} allocated bytes/run",
            kind.label(),
            allocations,
            allocated_bytes
        );
        group.bench_with_input(BenchmarkId::from_parameter(kind.label()), &bundle, {
            move |b, bundle| {
                b.iter(|| kind.run(black_box(bundle)));
            }
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_workflow_bundle
}
criterion_main!(benches);

const FIXTURE_JSON: &str = r#"{
  "schema_version": 1,
  "id": "github-pr-monitor",
  "name": "GitHub PR monitor",
  "version": "1.0.0",
  "triggers": [
    {
      "id": "github-pr-updated",
      "kind": "github",
      "provider": "github",
      "events": ["pull_request.opened", "pull_request.synchronize"],
      "node_id": "ingest"
    },
    {
      "id": "delay-log-check",
      "kind": "delay",
      "delay": "PT10M",
      "node_id": "query_logs"
    },
    {
      "id": "scheduled-summary",
      "kind": "schedule",
      "schedule": "0 */6 * * *",
      "node_id": "notify"
    },
    {
      "id": "mcp-summary",
      "kind": "mcp",
      "mcp_tool": "summarize_run",
      "node_id": "notify"
    }
  ],
  "workflow": {
    "_type": "workflow_graph",
    "id": "pr_monitor_workflow",
    "name": "PR monitor",
    "version": 1,
    "entry": "ingest",
    "nodes": {
      "ingest": {
        "id": "ingest",
        "kind": "action",
        "task_label": "Normalize PR event"
      },
      "wait_for_deploy": {
        "id": "wait_for_deploy",
        "kind": "waitpoint",
        "task_label": "Wait for deploy"
      },
      "query_logs": {
        "id": "query_logs",
        "kind": "action",
        "task_label": "Query logs"
      },
      "summarize": {
        "id": "summarize",
        "kind": "action",
        "task_label": "Summarize findings"
      },
      "review_required": {
        "id": "review_required",
        "kind": "approval",
        "task_label": "Require human approval"
      },
      "notify": {
        "id": "notify",
        "kind": "notification",
        "task_label": "Notify user"
      }
    },
    "edges": [
      {"from": "ingest", "to": "wait_for_deploy"},
      {"from": "wait_for_deploy", "to": "query_logs"},
      {"from": "query_logs", "to": "summarize"},
      {"from": "summarize", "to": "review_required"},
      {"from": "review_required", "to": "notify"}
    ]
  },
  "prompt_capsules": {
    "query-logs": {
      "id": "query-logs",
      "node_id": "query_logs",
      "trigger_id": "delay-log-check",
      "prompt": "Query deploy logs for the pull request and summarize failures."
    },
    "summarize": {
      "id": "summarize",
      "node_id": "summarize",
      "trigger_id": "scheduled-summary",
      "prompt": "Summarize PR activity for the team review."
    }
  },
  "policy": {
    "autonomy_tier": "act_with_approval",
    "retry": {"max_attempts": 3, "backoff": "exponential"},
    "catchup": {"mode": "latest", "max_events": 1}
  },
  "connectors": [
    {
      "id": "github",
      "provider_id": "github",
      "scopes": ["pull_requests:read", "checks:read"],
      "setup_required": true,
      "status_required": true
    },
    {
      "id": "slack",
      "provider_id": "slack",
      "scopes": ["chat:write"],
      "setup_required": true,
      "status_required": false
    }
  ],
  "environment": {
    "repo_setup_profile": "default",
    "worktree_policy": "host_managed",
    "command_gates": ["make test", "make lint"]
  },
  "receipts": {
    "run_id": "bundle_run_pr_monitor_fixture",
    "event_ids": ["github:event:42"],
    "workflow_version": 1
  }
}"#;
