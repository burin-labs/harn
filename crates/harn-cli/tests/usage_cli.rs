//! Conformance coverage for `harn usage`.
//!
//! Seeds a project `.harn/events.sqlite` with known
//! `provider_call_response` rows through the same event-log writer the
//! runtime uses, then invokes the real `harn usage` binary against it and
//! asserts exact aggregated totals. Deterministic and offline.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use harn_vm::event_log::{install_default_for_base_dir, EventLog, LogEvent, Topic};
use serde_json::{json, Value};
use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-usage-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

/// A `provider_call_response` payload as the runtime observability writer
/// emits it (only the fields `harn usage` reads are populated here).
fn call_event(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cost_usd: f64,
    cache_savings_usd: f64,
    response_ms: f64,
) -> Value {
    json!({
        "type": "provider_call_response",
        "provider": provider,
        "model": model,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_tokens": cache_read_tokens,
        "cache_write_tokens": 0,
        "cache_savings_usd": cache_savings_usd,
        "cost_usd": cost_usd,
        "response_ms": response_ms,
    })
}

/// Seed a project event log with a fixed set of rows: two openrouter
/// calls, one anthropic call, and one `mock` row that must be excluded.
fn seed_events(root: &Path) {
    let root = root.to_path_buf();
    run_in_harn_runtime(move || async move {
        let log = install_default_for_base_dir(&root).expect("install event log");
        let topic = Topic::new("agent.transcript.llm").expect("transcript topic");
        let rows = vec![
            call_event("openrouter", "qwen", 100, 10, 0, 0.02, 0.0, 120.0),
            call_event("openrouter", "qwen", 200, 20, 50, 0.03, 0.01, 80.0),
            call_event("anthropic", "sonnet", 300, 30, 0, 0.10, 0.0, 200.0),
            // Mock row: excluded from spend rollups by default.
            call_event("mock", "fixture", 999, 999, 0, 9.99, 0.0, 1.0),
        ];
        for row in rows {
            log.append(&topic, LogEvent::new("provider_call_response", row))
                .await
                .expect("append provider_call_response");
        }
        log.flush().await.expect("flush event log");
    });
}

fn run_usage(root: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["usage", "--project", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    Command::new(binary_path())
        .args(&args)
        .output()
        .expect("spawn harn usage")
}

fn stdout_json(output: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

#[test]
fn usage_by_provider_json_totals_are_exact() {
    let dir = TempDir::new().unwrap();
    seed_events(dir.path());

    let output = run_usage(dir.path(), &["--json"]);
    assert!(
        output.status.success(),
        "usage exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["ok"], json!(true));
    let data = &envelope["data"];
    assert_eq!(data["group_by"], json!("provider"));

    // Mock row excluded: 3 real calls remain.
    assert_eq!(data["calls"], json!(3));

    let groups = data["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    // Ordered by descending cost: anthropic (0.10) before openrouter (0.05).
    assert_eq!(groups[0]["key"], json!("anthropic"));
    assert!((groups[0]["cost_usd"].as_f64().unwrap() - 0.10).abs() < 1e-9);
    assert_eq!(groups[1]["key"], json!("openrouter"));
    assert!((groups[1]["cost_usd"].as_f64().unwrap() - 0.05).abs() < 1e-9);
    assert_eq!(groups[1]["input_tokens"], json!(300));
    assert_eq!(groups[1]["output_tokens"], json!(30));
    assert_eq!(groups[1]["cache_read_tokens"], json!(50));

    // Totals exclude the mock row.
    assert!((data["totals"]["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-9);
    assert_eq!(data["totals"]["input_tokens"], json!(600));
    assert_eq!(data["totals"]["output_tokens"], json!(60));
}

#[test]
fn usage_by_model_splits_provider_and_model() {
    let dir = TempDir::new().unwrap();
    seed_events(dir.path());

    let output = run_usage(dir.path(), &["--json", "--group-by", "model"]);
    assert!(output.status.success());
    let data = stdout_json(&output)["data"].clone();
    assert_eq!(data["group_by"], json!("model"));
    let keys: Vec<String> = data["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].as_str().unwrap().to_string())
        .collect();
    assert!(keys.contains(&"openrouter/qwen".to_string()));
    assert!(keys.contains(&"anthropic/sonnet".to_string()));
    assert!(!keys.iter().any(|k| k.starts_with("mock/")));
}

#[test]
fn usage_provider_filter_narrows_rows() {
    let dir = TempDir::new().unwrap();
    seed_events(dir.path());

    let output = run_usage(dir.path(), &["--json", "--provider", "anthropic"]);
    assert!(output.status.success());
    let data = stdout_json(&output)["data"].clone();
    assert_eq!(data["calls"], json!(1));
    let groups = data["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["key"], json!("anthropic"));
}

#[test]
fn usage_backfill_flag_errors_not_implemented() {
    let dir = TempDir::new().unwrap();
    seed_events(dir.path());

    let output = run_usage(dir.path(), &["--json", "--backfill", "anthropic"]);
    assert!(!output.status.success());
    let envelope = stdout_json(&output);
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(
        envelope["error"]["code"],
        json!("usage_backfill_unimplemented")
    );
}
