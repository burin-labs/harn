use std::path::PathBuf;

use harn_serve::transport::{
    read_jsonrpc_stdio_frame, write_jsonrpc_stdio_message, JsonRpcStdioFrameStyle,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use uuid::Uuid;

use crate::test_runner::{RunOptions, TestRunSession, TestRunSessionStats, TestShard};

const PROTOCOL_VERSION: &str = "1";
const TEST_RUN_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeParams {
    protocol_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestRunParams {
    path: PathBuf,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    max_test_ms: Option<u64>,
    #[serde(default)]
    max_execute_ms: Option<u64>,
    #[serde(default)]
    parallel: bool,
    #[serde(default)]
    fail_fast: bool,
    #[serde(default)]
    jobs: Option<usize>,
    #[serde(default)]
    shard: Option<TestRunShard>,
    #[serde(default)]
    skill_dirs: Vec<PathBuf>,
    #[serde(default)]
    diagnose: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestRunShard {
    index: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct TestRunResponse {
    schema_version: u32,
    worker_id: String,
    process_id: u32,
    run_count: u64,
    cache_before: TestRunSessionStats,
    cache_after: TestRunSessionStats,
    summary: crate::test_runner::TestSummary,
}

struct WorkerState {
    worker_id: String,
    initialized: bool,
    run_count: u64,
    session: TestRunSession,
}

impl Default for WorkerState {
    fn default() -> Self {
        Self {
            worker_id: Uuid::new_v4().to_string(),
            initialized: false,
            run_count: 0,
            session: TestRunSession::without_stdio(),
        }
    }
}

pub(crate) async fn serve_stdio() -> Result<(), String> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve(BufReader::new(stdin), stdout).await
}

async fn serve<R, W>(mut reader: R, mut writer: W) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut state = WorkerState::default();
    while let Some(frame) = read_jsonrpc_stdio_frame(&mut reader).await? {
        let style = frame.style;
        let request = match frame.parse_json() {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut writer,
                    parse_error(format!("invalid JSON: {error}")),
                    style,
                )
                .await?;
                continue;
            }
        };
        let (response, shutdown) = handle_request(request, &mut state).await;
        if let Some(response) = response {
            write_response(&mut writer, response, style).await?;
        }
        if shutdown {
            break;
        }
    }
    Ok(())
}

async fn handle_request(request: Value, state: &mut WorkerState) -> (Option<Value>, bool) {
    let Some(object) = request.as_object() else {
        return (Some(invalid_request(Value::Null)), false);
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if object.contains_key("id") && !(id.is_null() || id.is_string() || id.is_number()) {
        return (Some(invalid_request(Value::Null)), false);
    }
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return (Some(invalid_request(id)), false);
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return (Some(invalid_request(id)), false);
    };
    let notification = !object.contains_key("id");
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if notification {
        return (None, method == "shutdown");
    }

    let (response, shutdown) = match method {
        "initialize" => (initialize(id, params, state), false),
        "test/run" => (run_tests(id, params, state).await, false),
        "shutdown" => (
            harn_vm::jsonrpc::response(
                id,
                json!({
                    "worker_id": state.worker_id,
                    "process_id": std::process::id(),
                    "run_count": state.run_count,
                    "cache": state.session.stats(),
                }),
            ),
            true,
        ),
        _ => (
            harn_vm::jsonrpc::error_response(id, -32601, "method not found"),
            false,
        ),
    };
    (Some(response), shutdown)
}

fn initialize(id: Value, params: Value, state: &mut WorkerState) -> Value {
    let params = match serde_json::from_value::<InitializeParams>(params) {
        Ok(params) => params,
        Err(error) => return invalid_params(id, error.to_string()),
    };
    if params.protocol_version != PROTOCOL_VERSION {
        return invalid_params(
            id,
            format!(
                "unsupported protocol_version '{}'; expected {PROTOCOL_VERSION}",
                params.protocol_version
            ),
        );
    }
    state.initialized = true;
    harn_vm::jsonrpc::response(
        id,
        json!({
            "protocol_version": PROTOCOL_VERSION,
            "server_version": env!("CARGO_PKG_VERSION"),
            "worker_id": state.worker_id,
            "process_id": std::process::id(),
            "capabilities": {
                "test_run": {"schema_version": TEST_RUN_SCHEMA_VERSION}
            }
        }),
    )
}

async fn run_tests(id: Value, params: Value, state: &mut WorkerState) -> Value {
    if !state.initialized {
        return harn_vm::jsonrpc::error_response(id, -32002, "worker is not initialized");
    }
    let params = match serde_json::from_value::<TestRunParams>(params) {
        Ok(params) => params,
        Err(error) => return invalid_params(id, error.to_string()),
    };
    if !params.path.exists() {
        return invalid_params(
            id,
            format!("test path does not exist: {}", params.path.display()),
        );
    }
    let shard = match params.shard {
        Some(shard) => match TestShard::new(shard.index, shard.total) {
            Ok(shard) => Some(shard),
            Err(error) => return invalid_params(id, error),
        },
        None => None,
    };
    let options = RunOptions {
        filter: params.filter,
        timeout_ms: params.timeout_ms,
        max_test_ms: params.max_test_ms,
        max_execute_ms: params.max_execute_ms,
        parallel: params.parallel,
        fail_fast: params.fail_fast,
        jobs: params.jobs,
        shard,
        cli_skill_dirs: params.skill_dirs,
        progress: None,
        diagnose: params.diagnose,
        #[cfg(test)]
        setup_delay_ms: 0,
    };
    let cache_before = state.session.stats();
    let summary =
        crate::test_runner::run_tests_with_session(&params.path, &options, &state.session).await;
    state.run_count = state.run_count.saturating_add(1);
    let response = TestRunResponse {
        schema_version: TEST_RUN_SCHEMA_VERSION,
        worker_id: state.worker_id.clone(),
        process_id: std::process::id(),
        run_count: state.run_count,
        cache_before,
        cache_after: state.session.stats(),
        summary,
    };
    match serde_json::to_value(response) {
        Ok(result) => harn_vm::jsonrpc::response(id, result),
        Err(error) => harn_vm::jsonrpc::error_response_with_data(
            id,
            -32603,
            "failed to encode test result",
            json!({"detail": error.to_string()}),
        ),
    }
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn parse_error(detail: String) -> Value {
    harn_vm::jsonrpc::error_response_with_data(
        Value::Null,
        -32700,
        "parse error",
        json!({"detail": detail}),
    )
}

fn invalid_request(id: Value) -> Value {
    harn_vm::jsonrpc::error_response(id, -32600, "invalid request")
}

fn invalid_params(id: Value, detail: String) -> Value {
    harn_vm::jsonrpc::error_response_with_data(
        id,
        -32602,
        "invalid params",
        json!({"detail": detail}),
    )
}

async fn write_response<W>(
    writer: &mut W,
    response: Value,
    style: JsonRpcStdioFrameStyle,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    write_jsonrpc_stdio_message(writer, &response, style).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    async fn exchange(input: &[Value]) -> Vec<Value> {
        let bytes = input
            .iter()
            .map(|value| serde_json::to_string(value).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        let mut output = Vec::new();
        serve(BufReader::new(Cursor::new(bytes)), &mut output)
            .await
            .expect("serve");
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn requires_initialize_and_stops_after_shutdown() {
        let responses = exchange(&[
            harn_vm::jsonrpc::request(1, "test/run", json!({"path": "."})),
            harn_vm::jsonrpc::request(
                2,
                "initialize",
                json!({"protocol_version": PROTOCOL_VERSION}),
            ),
            harn_vm::jsonrpc::request(3, "shutdown", json!({})),
            harn_vm::jsonrpc::request(4, "missing", json!({})),
        ])
        .await;

        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["error"]["code"], -32002);
        assert_eq!(responses[1]["result"]["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(
            responses[1]["result"]["server_version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(responses[2]["result"]["run_count"], 0);
        assert_eq!(
            responses[2]["result"]["worker_id"],
            responses[1]["result"]["worker_id"]
        );
    }

    #[tokio::test]
    async fn rejects_unknown_protocol_versions() {
        let responses = exchange(&[harn_vm::jsonrpc::request(
            "init",
            "initialize",
            json!({"protocol_version": "999"}),
        )])
        .await;

        assert_eq!(responses[0]["id"], "init");
        assert_eq!(responses[0]["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn rejects_non_scalar_request_ids() {
        let responses = exchange(&[json!({
            "jsonrpc": "2.0",
            "id": {"invalid": true},
            "method": "initialize",
            "params": {"protocol_version": PROTOCOL_VERSION}
        })])
        .await;

        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(responses[0]["error"]["code"], -32600);
    }

    #[test]
    fn pins_test_run_response_schema_v2() {
        let response = TestRunResponse {
            schema_version: TEST_RUN_SCHEMA_VERSION,
            worker_id: "worker-1".to_string(),
            process_id: 42,
            run_count: 3,
            cache_before: TestRunSessionStats::default(),
            cache_after: TestRunSessionStats {
                workers: 1,
                hits: 2,
                misses: 1,
                insertions: 1,
                evictions: 0,
                entries: 1,
            },
            summary: crate::test_runner::TestSummary {
                results: vec![crate::test_runner::TestResult {
                    name: "test_timeout".to_string(),
                    file: "test_timeout.harn".to_string(),
                    passed: false,
                    error: Some("timed out".to_string()),
                    timeout: Some(crate::test_runner::TestTimeout {
                        phase: crate::test_runner::TestPhase::Execute,
                        limit_ms: 10,
                    }),
                    duration_ms: 12,
                    phases: Some(crate::test_runner::PhaseTimings {
                        setup_ms: 1,
                        compile_ms: 1,
                        execute_ms: 10,
                        teardown_ms: 0,
                        modules: harn_vm::ModulePhaseStats::default(),
                    }),
                }],
                passed: 0,
                failed: 1,
                total: 1,
                duration_ms: 13,
                timing: crate::test_timing::DurationSummary::from_samples(&[12]),
                aggregate: crate::test_runner::AggregateTimings {
                    collection_ms: 1,
                    setup_ms: 1,
                    compile_ms: 1,
                    execute_ms: 10,
                    teardown_ms: 0,
                    modules: harn_vm::ModulePhaseStats::default(),
                },
            },
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "schema_version": 2,
                "worker_id": "worker-1",
                "process_id": 42,
                "run_count": 3,
                "cache_before": {
                    "workers": 0, "hits": 0, "misses": 0,
                    "insertions": 0, "evictions": 0, "entries": 0
                },
                "cache_after": {
                    "workers": 1, "hits": 2, "misses": 1,
                    "insertions": 1, "evictions": 0, "entries": 1
                },
                "summary": {
                    "results": [{
                        "name": "test_timeout",
                        "file": "test_timeout.harn",
                        "passed": false,
                        "error": "timed out",
                        "timeout": {"phase": "execute", "limit_ms": 10},
                        "duration_ms": 12,
                        "phases": {
                            "setup_ms": 1, "compile_ms": 1,
                            "execute_ms": 10, "teardown_ms": 0,
                            "modules": {
                                "module_compile_ms": 0, "module_load_ms": 0,
                                "modules_compiled": 0, "modules_loaded": 0
                            }
                        }
                    }],
                    "passed": 0, "failed": 1, "total": 1, "duration_ms": 13,
                    "timing": {
                        "sample_count": 1, "average_ms": 12,
                        "p50_ms": 12, "p90_ms": 12, "p95_ms": 12, "p99_ms": 12
                    },
                    "aggregate": {
                        "collection_ms": 1, "setup_ms": 1, "compile_ms": 1,
                        "execute_ms": 10, "teardown_ms": 0,
                        "modules": {
                            "module_compile_ms": 0, "module_load_ms": 0,
                            "modules_compiled": 0, "modules_loaded": 0
                        }
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn stateful_notifications_are_ignored() {
        let responses = exchange(&[
            harn_vm::jsonrpc::notification(
                "initialize",
                json!({"protocol_version": PROTOCOL_VERSION}),
            ),
            harn_vm::jsonrpc::request(1, "test/run", json!({"path": "."})),
            harn_vm::jsonrpc::notification("shutdown", json!({})),
        ])
        .await;

        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["error"]["code"], -32002);
    }
}
