use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use harn_vm::testbench::mcp_mock::{
    score_world_state, verify_cassettes, McpCassette, McpCassetteRecorder, McpCassetteReplayer,
    McpWorldEvalReport, McpWorldRuntime, McpWorldSpec,
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::cli::{
    McpMockCommand, McpMockEvalArgs, McpMockRecordArgs, McpMockReplayArgs, McpMockVerifyArgs,
    McpMockWorldArgs,
};

use super::stdio_client::{jsonrpc_id_key, split_command, write_json_line};

pub(crate) async fn run(command: &McpMockCommand) -> Result<i32, String> {
    match command {
        McpMockCommand::Record(args) => record_args(args).await,
        McpMockCommand::Replay(args) => replay_args(args).await,
        McpMockCommand::Verify(args) => verify_args(args).await,
        McpMockCommand::World(args) => world_args(args).await,
        McpMockCommand::Eval(args) => eval_args(args),
    }
}

#[derive(Clone)]
struct ProxyRecorder {
    recorder: Arc<McpCassetteRecorder>,
    in_flight: Arc<Mutex<BTreeMap<String, InFlightRequest>>>,
}

#[derive(Clone)]
struct InFlightRequest {
    request: JsonValue,
    started_at: harn_vm::clock_mock::ClockInstant,
}

impl ProxyRecorder {
    fn new(recorder: Arc<McpCassetteRecorder>) -> Self {
        Self {
            recorder,
            in_flight: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn observe_client_message(&self, message: &JsonValue) {
        if message.get("method").is_none() {
            return;
        }
        let Some(key) = jsonrpc_id_key(message.get("id")) else {
            self.recorder.record(message, &JsonValue::Null, 0);
            return;
        };
        self.in_flight
            .lock()
            .expect("MCP proxy in-flight mutex poisoned")
            .insert(
                key,
                InFlightRequest {
                    request: message.clone(),
                    started_at: harn_vm::clock_mock::instant_now(),
                },
            );
    }

    fn observe_server_message(&self, message: &JsonValue) {
        if message.get("method").is_some() {
            return;
        }
        let Some(key) = jsonrpc_id_key(message.get("id")) else {
            return;
        };
        let Some(in_flight) = self
            .in_flight
            .lock()
            .expect("MCP proxy in-flight mutex poisoned")
            .remove(&key)
        else {
            return;
        };
        let latency_ms = harn_vm::clock_mock::instant_now()
            .duration_since(in_flight.started_at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        self.recorder
            .record(&in_flight.request, message, latency_ms);
    }
}

async fn record_args(args: &McpMockRecordArgs) -> Result<i32, String> {
    let (program, argv) = split_command(&args.command)?;
    let mut child = Command::new(program)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("spawn upstream MCP server `{program}`: {error}"))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "upstream MCP server stdin was not piped".to_string())?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "upstream MCP server stdout was not piped".to_string())?;

    let recorder = Arc::new(McpCassetteRecorder::default());
    let proxy = ProxyRecorder::new(Arc::clone(&recorder));
    let proxy_for_input = proxy.clone();
    let client_to_server = tokio::spawn(async move {
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| format!("read client stdin: {error}"))?
        {
            if let Ok(message) = serde_json::from_str::<JsonValue>(line.trim()) {
                proxy_for_input.observe_client_message(&message);
            }
            child_stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|error| format!("write upstream stdin: {error}"))?;
            child_stdin
                .write_all(b"\n")
                .await
                .map_err(|error| format!("write upstream stdin: {error}"))?;
            child_stdin
                .flush()
                .await
                .map_err(|error| format!("flush upstream stdin: {error}"))?;
        }
        let _ = child_stdin.shutdown().await;
        Ok::<(), String>(())
    });

    let proxy_for_output = proxy;
    let server_to_client = tokio::spawn(async move {
        let stdout = BufReader::new(child_stdout);
        let mut lines = stdout.lines();
        let mut client_stdout = tokio::io::stdout();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| format!("read upstream stdout: {error}"))?
        {
            if let Ok(message) = serde_json::from_str::<JsonValue>(line.trim()) {
                proxy_for_output.observe_server_message(&message);
            }
            client_stdout
                .write_all(line.as_bytes())
                .await
                .map_err(|error| format!("write client stdout: {error}"))?;
            client_stdout
                .write_all(b"\n")
                .await
                .map_err(|error| format!("write client stdout: {error}"))?;
            client_stdout
                .flush()
                .await
                .map_err(|error| format!("flush client stdout: {error}"))?;
        }
        Ok::<(), String>(())
    });

    client_to_server
        .await
        .map_err(|error| format!("client proxy task failed: {error}"))??;
    server_to_client
        .await
        .map_err(|error| format!("server proxy task failed: {error}"))??;
    let _ = child.wait().await;
    recorder.snapshot().persist(Path::new(&args.cassette))?;
    Ok(0)
}

async fn replay_args(args: &McpMockReplayArgs) -> Result<i32, String> {
    let cassette = McpCassette::load(Path::new(&args.cassette))?;
    let mut replayer = McpCassetteReplayer::new(cassette);
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("read stdin: {error}"))?
    {
        let request: JsonValue = match serde_json::from_str(line.trim()) {
            Ok(request) => request,
            Err(_) => continue,
        };
        match replayer.replay_request(&request) {
            Ok(Some(response)) => write_json_line(&mut stdout, &response).await?,
            Ok(None) => {}
            Err(error) => {
                let response = harn_vm::jsonrpc::error_response(
                    request.get("id").cloned().unwrap_or(JsonValue::Null),
                    -32080,
                    &error.to_string(),
                );
                write_json_line(&mut stdout, &response).await?;
                return Ok(2);
            }
        }
    }
    if !replayer.is_finished() {
        return Err(format!(
            "replay ended with {} cassette interaction(s) unused",
            replayer.remaining()
        ));
    }
    Ok(0)
}

async fn verify_args(args: &McpMockVerifyArgs) -> Result<i32, String> {
    let baseline = McpCassette::load(Path::new(&args.cassette))?;
    let candidate = if let Some(candidate) = args.candidate.as_ref() {
        McpCassette::load(Path::new(candidate))?
    } else if !args.command.is_empty() {
        record_candidate_from_command(&baseline, &args.command).await?
    } else {
        return Err("verify requires either --candidate or a command after --".to_string());
    };
    let report = verify_cassettes(&baseline, &candidate);
    emit_json(&report, args.report.as_deref())?;
    Ok(if report.passed { 0 } else { 2 })
}

async fn record_candidate_from_command(
    baseline: &McpCassette,
    command: &[String],
) -> Result<McpCassette, String> {
    let (program, argv) = split_command(command)?;
    let mut child = Command::new(program)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("spawn candidate MCP server `{program}`: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "candidate MCP server stdin was not piped".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "candidate MCP server stdout was not piped".to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    let recorder = McpCassetteRecorder::default();

    for interaction in &baseline.interactions {
        let request = interaction.request.clone();
        let started_at = harn_vm::clock_mock::instant_now();
        write_json_line(&mut stdin, &request).await?;
        if interaction.notification {
            recorder.record(&request, &JsonValue::Null, 0);
            continue;
        }
        let id_key = jsonrpc_id_key(request.get("id"))
            .ok_or_else(|| "recorded request is missing JSON-RPC id".to_string())?;
        let response = loop {
            let line = lines
                .next_line()
                .await
                .map_err(|error| format!("read candidate stdout: {error}"))?
                .ok_or_else(|| "candidate MCP server exited before response".to_string())?;
            let Ok(message) = serde_json::from_str::<JsonValue>(line.trim()) else {
                continue;
            };
            if message.get("method").is_none()
                && jsonrpc_id_key(message.get("id")) == Some(id_key.clone())
            {
                break message;
            }
        };
        let latency_ms = harn_vm::clock_mock::instant_now()
            .duration_since(started_at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        recorder.record(&request, &response, latency_ms);
    }

    let _ = stdin.shutdown().await;
    let _ = child.wait().await;
    Ok(recorder.snapshot())
}

async fn world_args(args: &McpMockWorldArgs) -> Result<i32, String> {
    let spec = McpWorldSpec::load(Path::new(&args.spec))?;
    let mut runtime = McpWorldRuntime::new(spec.clone());
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("read stdin: {error}"))?
    {
        let request: JsonValue = match serde_json::from_str(line.trim()) {
            Ok(request) => request,
            Err(_) => continue,
        };
        if let Some(response) = runtime.handle_json_rpc(request) {
            write_json_line(&mut stdout, &response).await?;
        }
    }

    if let Some(path) = args.state_out.as_deref() {
        emit_json(runtime.state(), Some(path))?;
    }
    let mut exit_code = 0;
    if let Some(path) = args.report.as_deref() {
        let score = score_world_state(&spec, runtime.state());
        let report = McpWorldEvalReport::from_scores(vec![score]);
        exit_code = if report.passed { 0 } else { 2 };
        emit_json(&report, Some(path))?;
    }
    Ok(exit_code)
}

fn eval_args(args: &McpMockEvalArgs) -> Result<i32, String> {
    let spec = McpWorldSpec::load(Path::new(&args.spec))?;
    let mut scores = Vec::with_capacity(args.states.len());
    for state_path in &args.states {
        let body = std::fs::read_to_string(state_path)
            .map_err(|error| format!("read final state {state_path}: {error}"))?;
        let state: JsonValue = serde_json::from_str(&body)
            .map_err(|error| format!("parse final state {state_path}: {error}"))?;
        scores.push(score_world_state(&spec, &state));
    }
    let report = McpWorldEvalReport::from_scores(scores);
    emit_json(&report, args.report.as_deref())?;
    Ok(if report.passed { 0 } else { 2 })
}

fn emit_json<T: Serialize>(value: &T, path: Option<&str>) -> Result<(), String> {
    let body =
        serde_json::to_string_pretty(value).map_err(|error| format!("serialize JSON: {error}"))?;
    if let Some(path) = path {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
            }
        }
        std::fs::write(path, format!("{body}\n"))
            .map_err(|error| format!("write {path}: {error}"))?;
    } else {
        println!("{body}");
    }
    Ok(())
}
