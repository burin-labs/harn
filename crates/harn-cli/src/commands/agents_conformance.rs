use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process;
use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "agents-protocol-2026-04-25";
const REPORT_SCHEMA: &str = "harn-agents-conformance-2026-04-25";

#[derive(Debug, Clone)]
pub(crate) struct AgentsConformanceConfig {
    pub(crate) target_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) categories: Vec<String>,
    pub(crate) timeout_ms: u64,
    pub(crate) verbose: bool,
    pub(crate) json: bool,
    pub(crate) json_out: Option<String>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ConformanceClient {
    http: reqwest::Client,
    base_url: Url,
    api_key: Option<String>,
    timeout: Duration,
}

#[derive(Debug)]
struct HttpResult {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
    json: Option<Value>,
}

#[derive(Debug, Default)]
struct ProbeState {
    run_id: String,
    discovery: Option<Value>,
    capabilities: BTreeMap<String, bool>,
    workspace_id: Option<String>,
    session_id: Option<String>,
    task_id: Option<String>,
    message_id: Option<String>,
    artifact_id: Option<String>,
    event_id: Option<String>,
    receipt_id: Option<String>,
    outcome_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ProbeStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Serialize)]
struct TestReport {
    name: String,
    status: ProbeStatus,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct CategoryReport {
    name: String,
    status: ProbeStatus,
    passed: usize,
    failed: usize,
    skipped: usize,
    duration_ms: u64,
    percentiles_ms: Percentiles,
    tests: Vec<TestReport>,
}

#[derive(Debug, Default, Serialize)]
struct Percentiles {
    avg: u64,
    p50: u64,
    p95: u64,
    p99: u64,
}

#[derive(Debug, Serialize)]
struct SummaryReport {
    status: ProbeStatus,
    score: f64,
    passed: usize,
    failed: usize,
    skipped: usize,
    total: usize,
    categories_passed: usize,
    categories_failed: usize,
    categories_skipped: usize,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
struct LeaderboardReport {
    schema: &'static str,
    target: String,
    protocol_version: &'static str,
    generated_at: String,
    summary: SummaryReport,
    percentiles_ms: Percentiles,
    capabilities: BTreeMap<String, bool>,
    categories: Vec<CategoryReport>,
}

type ProbeResult<T = ()> = Result<T, String>;

pub(crate) async fn run_agents_conformance(config: AgentsConformanceConfig) {
    let client = ConformanceClient::new(
        &config.target_url,
        config.api_key.clone(),
        Duration::from_millis(config.timeout_ms),
    )
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(2);
    });

    let requested_categories = match resolve_categories(&config.categories) {
        Ok(categories) => categories,
        Err(error) => {
            eprintln!("{error}");
            process::exit(2);
        }
    };

    let suite_start = Instant::now();
    let mut state = ProbeState {
        run_id: Uuid::new_v4().to_string(),
        workspace_id: config.workspace_id.clone(),
        session_id: config.session_id.clone(),
        ..ProbeState::default()
    };
    let mut reports = Vec::new();

    if !config.json {
        println!(
            "Running Harn Agents Protocol conformance against {}",
            client.base_url.as_str().trim_end_matches('/')
        );
        println!();
    }

    for category in requested_categories {
        let report = run_category(category, &client, &mut state, config.verbose).await;
        if !config.json {
            print_category_report(&report, config.verbose);
        }
        reports.push(report);
    }

    let report = build_leaderboard_report(client.base_url.as_str(), state, reports, suite_start);
    let failed = report.summary.failed > 0 || report.summary.categories_failed > 0;

    if config.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
        );
    } else {
        print_summary(&report);
    }

    if let Some(path) = config.json_out.as_deref() {
        let rendered = serde_json::to_string_pretty(&report).unwrap_or_else(|error| {
            eprintln!("failed to serialize agents conformance JSON: {error}");
            process::exit(1);
        });
        fs::write(path, rendered).unwrap_or_else(|error| {
            eprintln!("failed to write agents conformance JSON to {path}: {error}");
            process::exit(1);
        });
        if !config.json {
            println!("JSON report written to {path}");
        }
    }

    if failed {
        process::exit(1);
    }
}

async fn run_category(
    name: &str,
    client: &ConformanceClient,
    state: &mut ProbeState,
    verbose: bool,
) -> CategoryReport {
    let start = Instant::now();
    let mut tests = Vec::new();

    match name {
        "core" => {
            run_test(&mut tests, "discovery", || discovery(client, state)).await;
            run_test(&mut tests, "agent_card", || agent_card(client)).await;
            run_test(&mut tests, "list_core_resources", || {
                list_core_resources(client)
            })
            .await;
            run_test(&mut tests, "workspace_crud", || {
                workspace_crud(client, state)
            })
            .await;
            run_test(&mut tests, "session_crud", || session_crud(client, state)).await;
            run_test(&mut tests, "message_crud", || message_crud(client, state)).await;
            run_test(&mut tests, "task_crud", || task_crud(client, state)).await;
            run_test(&mut tests, "artifact_crud", || artifact_crud(client, state)).await;
            run_test(&mut tests, "event_reads", || event_reads(client, state)).await;
            run_test(&mut tests, "persona_memory_branch_reads", || {
                extended_read_surfaces(client, state)
            })
            .await;
        }
        "streaming" => {
            run_test(&mut tests, "session_sse_event_shape", || {
                session_sse_event_shape(client, state)
            })
            .await;
            run_test(&mut tests, "task_sse_event_shape", || {
                task_sse_event_shape(client, state)
            })
            .await;
        }
        "lifecycle" => {
            run_test(&mut tests, "task_state_machine", || {
                task_state_machine(client, state)
            })
            .await;
            run_test(&mut tests, "cancel_terminal_consistency", || {
                cancel_terminal_consistency(client, state)
            })
            .await;
        }
        "idempotency" => {
            run_test(&mut tests, "same_key_same_body_reuses_task", || {
                same_key_same_body_reuses_task(client, state)
            })
            .await;
            run_test(&mut tests, "same_key_different_body_conflicts", || {
                same_key_different_body_conflicts(client, state)
            })
            .await;
        }
        "auth-and-scopes" => {
            run_test(&mut tests, "authenticated_request_succeeds", || {
                authenticated_request_succeeds(client)
            })
            .await;
            run_test(&mut tests, "missing_auth_rejected", || {
                missing_auth_rejected(client)
            })
            .await;
            run_test(&mut tests, "unsupported_protocol_version_rejected", || {
                unsupported_protocol_version_rejected(client)
            })
            .await;
        }
        "replay" => {
            run_test(&mut tests, "event_range_is_byte_deterministic", || {
                event_range_is_byte_deterministic(client, state)
            })
            .await;
            run_test(&mut tests, "event_resume_cursor", || {
                event_resume_cursor(client, state)
            })
            .await;
        }
        "receipts" => {
            run_test(&mut tests, "task_receipts", || task_receipts(client, state)).await;
            run_test(&mut tests, "receipt_verification", || {
                receipt_verification(client, state)
            })
            .await;
        }
        "vaults" => {
            run_test(&mut tests, "vault_create_and_read", || {
                vault_create_and_read(client, state)
            })
            .await;
        }
        "outcomes" => {
            run_test(&mut tests, "outcome_list_and_read", || {
                outcome_list_and_read(client, state)
            })
            .await;
        }
        "webhooks" => {
            run_test(&mut tests, "connector_webhook_metadata", || {
                connector_webhook_metadata(client)
            })
            .await;
        }
        _ => tests.push(TestReport {
            name: format!("unknown category {name}"),
            status: ProbeStatus::Failed,
            duration_ms: 0,
            error: Some("internal category dispatch error".to_string()),
        }),
    }

    let passed = tests
        .iter()
        .filter(|test| test.status == ProbeStatus::Passed)
        .count();
    let failed = tests
        .iter()
        .filter(|test| test.status == ProbeStatus::Failed)
        .count();
    let skipped = tests
        .iter()
        .filter(|test| test.status == ProbeStatus::Skipped)
        .count();
    let status = if failed > 0 {
        ProbeStatus::Failed
    } else if passed > 0 {
        ProbeStatus::Passed
    } else {
        ProbeStatus::Skipped
    };
    let durations = tests
        .iter()
        .map(|test| test.duration_ms)
        .collect::<Vec<_>>();
    let report = CategoryReport {
        name: name.to_string(),
        status,
        passed,
        failed,
        skipped,
        duration_ms: start.elapsed().as_millis() as u64,
        percentiles_ms: percentiles(durations),
        tests,
    };

    if verbose {
        eprintln!("agents-conformance category {} completed", report.name);
    }
    report
}

async fn run_test<Fut>(tests: &mut Vec<TestReport>, name: &'static str, f: impl FnOnce() -> Fut)
where
    Fut: std::future::Future<Output = ProbeResult>,
{
    let start = Instant::now();
    let result = f().await;
    let duration_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(()) => tests.push(TestReport {
            name: name.to_string(),
            status: ProbeStatus::Passed,
            duration_ms,
            error: None,
        }),
        Err(error) if error.starts_with("SKIP:") => tests.push(TestReport {
            name: name.to_string(),
            status: ProbeStatus::Skipped,
            duration_ms,
            error: Some(error.trim_start_matches("SKIP:").trim().to_string()),
        }),
        Err(error) => tests.push(TestReport {
            name: name.to_string(),
            status: ProbeStatus::Failed,
            duration_ms,
            error: Some(error),
        }),
    }
}

async fn discovery(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let response = client.get_public_json("/v1").await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "protocol_discovery")?;
    expect_field_eq(body, "protocol_family", "harn_agents_protocol")?;
    expect_field_eq(body, "current_version", PROTOCOL_VERSION)?;
    expect_string_array_contains(body, "supported_versions", PROTOCOL_VERSION)?;
    let capabilities = body
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| "discovery.capabilities must be an object".to_string())?;
    state.capabilities = capabilities
        .iter()
        .filter_map(|(key, value)| value.as_bool().map(|enabled| (key.clone(), enabled)))
        .collect();
    state.discovery = Some(body.clone());
    Ok(())
}

async fn agent_card(client: &ConformanceClient) -> ProbeResult {
    let response = client.get_public_json("/v1/agent-card").await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "agent_card")?;
    expect_string_field(body, "id")?;
    expect_string_field(body, "name")?;
    expect_field_eq(body, "protocol_version", PROTOCOL_VERSION)?;
    expect_array_field(body, "interfaces")?;
    expect_array_field(body, "skills")?;
    let a2a_card = expect_object_field(body, "a2a_agent_card")?;
    expect_string_field(a2a_card, "name")?;
    expect_string_field(a2a_card, "description")?;
    expect_string_field(a2a_card, "version")?;
    expect_string_field(a2a_card, "url")?;
    expect_object_field(a2a_card, "capabilities")?;
    let interfaces = expect_array_field(a2a_card, "supportedInterfaces")?;
    if interfaces.is_empty() {
        return Err("a2a_agent_card.supportedInterfaces must not be empty".to_string());
    }
    for interface in interfaces {
        expect_string_field(interface, "url")?;
        expect_string_field(interface, "protocolBinding")?;
        expect_string_field(interface, "protocolVersion")?;
    }
    expect_array_field(a2a_card, "skills")?;
    Ok(())
}

async fn list_core_resources(client: &ConformanceClient) -> ProbeResult {
    for path in [
        "/v1/tasks",
        "/v1/sessions",
        "/v1/artifacts",
        "/v1/events",
        "/v1/outcomes",
    ] {
        let response = client.get_json(path).await?;
        expect_status(&response, StatusCode::OK)?;
        expect_list_response(expect_json(&response)?, path)?;
    }
    Ok(())
}

async fn workspace_crud(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    if let Some(workspace_id) = state.workspace_id.clone() {
        let body = get_required_resource(client, &format!("/v1/workspaces/{workspace_id}")).await?;
        expect_field_eq(&body, "object", "workspace")?;
        return Ok(());
    }

    let request = json!({
        "name": format!("agents-conformance-{}", state.run_id),
        "root": format!("harn://conformance/{}", state.run_id),
        "metadata": conformance_metadata(&state.run_id),
    });
    let response = client
        .post_json(
            "/v1/workspaces",
            &request,
            Some(&format!("{}-workspace", state.run_id)),
        )
        .await?;
    if response.status == StatusCode::NOT_FOUND || response.status == StatusCode::NOT_IMPLEMENTED {
        let fallback = first_list_id(client, "/v1/workspaces").await?;
        state.workspace_id = Some(fallback);
        return Ok(());
    }
    expect_status_any(&response, &[StatusCode::CREATED, StatusCode::OK])?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "workspace")?;
    let workspace_id = expect_string_field(body, "id")?.to_string();
    state.workspace_id = Some(workspace_id.clone());

    let patch = json!({
        "metadata": conformance_metadata(&state.run_id),
    });
    let response = client
        .patch_json(
            &format!("/v1/workspaces/{workspace_id}"),
            &patch,
            Some(&format!("{}-workspace-patch", state.run_id)),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::OK, StatusCode::NOT_IMPLEMENTED])?;
    Ok(())
}

async fn session_crud(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    if let Some(session_id) = state.session_id.clone() {
        let body = get_required_resource(client, &format!("/v1/sessions/{session_id}")).await?;
        expect_field_eq(&body, "object", "session")?;
        return Ok(());
    }

    let workspace_id = ensure_workspace(client, state).await?;
    let request = json!({
        "workspace_id": workspace_id,
        "initial_messages": [message_input("agents conformance session setup")],
        "metadata": conformance_metadata(&state.run_id),
    });
    let response = client
        .post_json(
            "/v1/sessions",
            &request,
            Some(&format!("{}-session", state.run_id)),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::CREATED, StatusCode::OK])?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "session")?;
    let session_id = expect_string_field(body, "id")?.to_string();
    state.session_id = Some(session_id.clone());

    let patch = json!({
        "summary": "agents conformance probe",
        "metadata": conformance_metadata(&state.run_id),
    });
    let response = client
        .patch_json(
            &format!("/v1/sessions/{session_id}"),
            &patch,
            Some(&format!("{}-session-patch", state.run_id)),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::OK, StatusCode::NOT_IMPLEMENTED])?;
    Ok(())
}

async fn message_crud(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let session_id = ensure_session(client, state).await?;
    let response = client
        .post_json(
            &format!("/v1/sessions/{session_id}/messages"),
            &json!({"message": message_input("agents conformance message")}),
            Some(&format!("{}-message", state.run_id)),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::CREATED, StatusCode::OK])?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "message")?;
    let message_id = expect_string_field(body, "id")?.to_string();
    state.message_id = Some(message_id.clone());

    let body = get_required_resource(client, &format!("/v1/messages/{message_id}")).await?;
    expect_field_eq(&body, "object", "message")?;

    let response = client
        .get_json(&format!("/v1/sessions/{session_id}/messages"))
        .await?;
    expect_status(&response, StatusCode::OK)?;
    expect_list_response(expect_json(&response)?, "session messages")?;
    Ok(())
}

async fn task_crud(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let task = submit_task(client, state, "agents conformance task").await?;
    let task_id = expect_string_field(&task, "id")?.to_string();
    state.task_id = Some(task_id.clone());
    remember_task_links(state, &task);

    let body = get_required_resource(client, &format!("/v1/tasks/{task_id}")).await?;
    expect_field_eq(&body, "object", "task")?;
    expect_task_status(&body)?;

    let response = client
        .get_json(&format!("/v1/tasks/{task_id}/events"))
        .await?;
    expect_status(&response, StatusCode::OK)?;
    remember_first_event(state, expect_json(&response)?);

    let response = client
        .post_json(
            &format!("/v1/tasks/{task_id}/cancel"),
            &json!({"reason": "agents conformance cancellation probe"}),
            Some(&format!("{}-cancel", state.run_id)),
        )
        .await?;
    expect_status_any(
        &response,
        &[
            StatusCode::OK,
            StatusCode::CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY,
        ],
    )?;
    if response.status == StatusCode::OK {
        expect_task_status(expect_json(&response)?)?;
    }
    Ok(())
}

async fn artifact_crud(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let workspace_id = ensure_workspace(client, state).await?;
    let response = client
        .post_json(
            "/v1/artifacts",
            &json!({
                "kind": "log",
                "mime_type": "text/plain",
                "uri": format!("harn://conformance/{}/artifact.txt", state.run_id),
                "visibility": "public",
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "workspace_id": workspace_id,
                "metadata": conformance_metadata(&state.run_id),
            }),
            Some(&format!("{}-artifact", state.run_id)),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::CREATED, StatusCode::OK])?;
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "artifact")?;
    let artifact_id = expect_string_field(body, "id")?.to_string();
    state.artifact_id = Some(artifact_id.clone());

    let body = get_required_resource(client, &format!("/v1/artifacts/{artifact_id}")).await?;
    expect_field_eq(&body, "object", "artifact")?;
    Ok(())
}

async fn event_reads(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let response = client.get_json("/v1/events").await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_list_response(body, "events")?;
    remember_first_event(state, body);
    if let Some(event_id) = state.event_id.as_deref() {
        let event = get_required_resource(client, &format!("/v1/events/{event_id}")).await?;
        expect_field_eq(&event, "object", "event")?;
        expect_string_field(&event, "event")?;
    } else {
        return skip("no events were available to read by id");
    }
    Ok(())
}

async fn extended_read_surfaces(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    for path in [
        "/v1/personas",
        "/v1/connectors",
        "/v1/skills",
        "/v1/quotas",
        "/v1/memories",
    ] {
        let response = client.get_json(path).await?;
        expect_status_any(&response, &[StatusCode::OK, StatusCode::NOT_IMPLEMENTED])?;
        if response.status == StatusCode::OK {
            expect_list_response(expect_json(&response)?, path)?;
        }
    }

    let session_id = ensure_session(client, state).await?;
    let response = client
        .post_json(
            &format!("/v1/sessions/{session_id}/branches"),
            &json!({
                "kind": "session",
                "base_ref": "main",
                "metadata": conformance_metadata(&state.run_id),
            }),
            Some(&format!("{}-branch", state.run_id)),
        )
        .await?;
    expect_status_any(
        &response,
        &[
            StatusCode::CREATED,
            StatusCode::OK,
            StatusCode::NOT_IMPLEMENTED,
        ],
    )?;
    if response.status == StatusCode::CREATED || response.status == StatusCode::OK {
        let body = expect_json(&response)?;
        expect_field_eq(body, "object", "branch")?;
        let branch_id = expect_string_field(body, "id")?;
        let body = get_required_resource(client, &format!("/v1/branches/{branch_id}")).await?;
        expect_field_eq(&body, "object", "branch")?;
    }
    Ok(())
}

async fn session_sse_event_shape(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult {
    let session_id = ensure_session(client, state).await?;
    let stream_client = client.clone();
    let path = format!("/v1/sessions/{session_id}/events/stream");
    let stream = tokio::spawn(async move { stream_sse_frames(&stream_client, &path, 1).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = client
        .post_json(
            &format!("/v1/sessions/{session_id}/messages"),
            &json!({"message": message_input("agents conformance stream event")}),
            Some(&format!("{}-stream-message", state.run_id)),
        )
        .await;
    let frames = stream
        .await
        .map_err(|error| format!("session stream task failed: {error}"))??;
    validate_sse_frames(&frames)
}

async fn task_sse_event_shape(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let task = submit_task(client, state, "agents conformance task stream").await?;
    let task_id = expect_string_field(&task, "id")?.to_string();
    state.task_id = Some(task_id.clone());
    let frames = stream_sse_frames(client, &format!("/v1/tasks/{task_id}/stream"), 1).await?;
    validate_sse_frames(&frames)
}

async fn task_state_machine(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let task = submit_task(client, state, "agents conformance lifecycle").await?;
    let task_id = expect_string_field(&task, "id")?.to_string();
    state.task_id = Some(task_id.clone());
    let mut statuses = Vec::new();
    statuses.push(expect_task_status(&task)?.to_string());

    let deadline =
        Instant::now() + Duration::from_millis(client.timeout.as_millis().min(10_000) as u64);
    while Instant::now() < deadline {
        let body = get_required_resource(client, &format!("/v1/tasks/{task_id}")).await?;
        let status = expect_task_status(&body)?.to_string();
        if statuses.last() != Some(&status) {
            statuses.push(status.clone());
        }
        if is_terminal_status(&status) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    validate_status_transitions(&statuses)?;
    let events = client
        .get_json(&format!("/v1/tasks/{task_id}/events"))
        .await?;
    expect_status(&events, StatusCode::OK)?;
    let list = expect_json(&events)?;
    let data = list_data(list)?;
    if data.is_empty() {
        return Err("task lifecycle did not expose any task events".to_string());
    }
    for event in data {
        expect_field_eq(event, "object", "event")?;
        expect_string_field(event, "event")?;
        expect_u64_field(event, "sequence")?;
    }
    Ok(())
}

async fn cancel_terminal_consistency(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult {
    let task = submit_task(client, state, "agents conformance cancellation lifecycle").await?;
    let task_id = expect_string_field(&task, "id")?.to_string();
    let response = client
        .post_json(
            &format!("/v1/tasks/{task_id}/cancel"),
            &json!({"reason": "agents conformance cancellation lifecycle"}),
            Some(&format!("{}-lifecycle-cancel", state.run_id)),
        )
        .await?;
    expect_status_any(
        &response,
        &[
            StatusCode::OK,
            StatusCode::CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY,
        ],
    )?;
    let body = get_required_resource(client, &format!("/v1/tasks/{task_id}")).await?;
    expect_task_status(&body)?;
    if body.get("status").and_then(Value::as_str) == Some("CANCELED")
        && body
            .get("completed_at")
            .is_some_and(|value| !value.is_null())
    {
        return Err("canceled task must not also expose completed_at".to_string());
    }
    Ok(())
}

async fn same_key_same_body_reuses_task(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult {
    let session_id = ensure_session(client, state).await?;
    let body = json!({"input": message_input("agents conformance idempotent task")});
    let key = format!("{}-same-body", state.run_id);
    let first = client
        .post_json(
            &format!("/v1/sessions/{session_id}/tasks"),
            &body,
            Some(&key),
        )
        .await?;
    expect_status_any(&first, &[StatusCode::ACCEPTED, StatusCode::OK])?;
    let second = client
        .post_json(
            &format!("/v1/sessions/{session_id}/tasks"),
            &body,
            Some(&key),
        )
        .await?;
    expect_status_any(&second, &[StatusCode::ACCEPTED, StatusCode::OK])?;
    let first_id = expect_string_field(expect_json(&first)?, "id")?;
    let second_id = expect_string_field(expect_json(&second)?, "id")?;
    if first_id != second_id {
        return Err(format!(
            "same Idempotency-Key created different tasks: {first_id} != {second_id}"
        ));
    }
    Ok(())
}

async fn same_key_different_body_conflicts(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult {
    let session_id = ensure_session(client, state).await?;
    let key = format!("{}-different-body", state.run_id);
    let first = client
        .post_json(
            &format!("/v1/sessions/{session_id}/tasks"),
            &json!({"input": message_input("first idempotent body")}),
            Some(&key),
        )
        .await?;
    expect_status_any(&first, &[StatusCode::ACCEPTED, StatusCode::OK])?;
    let second = client
        .post_json(
            &format!("/v1/sessions/{session_id}/tasks"),
            &json!({"input": message_input("second idempotent body")}),
            Some(&key),
        )
        .await?;
    expect_status(&second, StatusCode::CONFLICT)?;
    expect_error_code(&second, "idempotency_key_reused")
}

async fn authenticated_request_succeeds(client: &ConformanceClient) -> ProbeResult {
    if client.api_key.is_none() {
        return skip("no --api-key supplied; authenticated request probe skipped");
    }
    let response = client.get_json("/v1/tasks").await?;
    expect_status(&response, StatusCode::OK)?;
    expect_list_response(expect_json(&response)?, "tasks")?;
    Ok(())
}

async fn missing_auth_rejected(client: &ConformanceClient) -> ProbeResult {
    let response = client.get_json_without_auth("/v1/tasks").await?;
    if response.status == StatusCode::OK {
        return Err("authenticated endpoint accepted a request without Authorization".to_string());
    }
    expect_status_any(
        &response,
        &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
    )?;
    if let Some(api_key) = client.api_key.as_deref() {
        if !api_key.is_empty() && response.body.contains(api_key) {
            return Err("error response leaked the API key".to_string());
        }
    }
    let code = error_code(&response)?;
    if code != "unauthenticated" && code != "permission_denied" {
        return Err(format!(
            "missing auth returned error code {code}, expected unauthenticated or permission_denied"
        ));
    }
    Ok(())
}

async fn unsupported_protocol_version_rejected(client: &ConformanceClient) -> ProbeResult {
    let response = client
        .request_json(
            Method::GET,
            "/v1/tasks",
            None,
            None,
            true,
            Some("agents-protocol-1900-01-01"),
        )
        .await?;
    expect_status(&response, StatusCode::UPGRADE_REQUIRED)?;
    expect_error_code(&response, "unsupported_protocol_version")
}

async fn event_range_is_byte_deterministic(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult {
    if !capability_enabled(state, "replay") {
        return skip("target does not advertise replay capability");
    }
    let task_id = ensure_task(client, state).await?;
    let path = format!("/v1/tasks/{task_id}/events");
    let first = client.get_json(&path).await?;
    expect_status(&first, StatusCode::OK)?;
    let second = client.get_json(&path).await?;
    expect_status(&second, StatusCode::OK)?;
    let first_json = serde_json::to_string(expect_json(&first)?)
        .map_err(|error| format!("failed to render first event range: {error}"))?;
    let second_json = serde_json::to_string(expect_json(&second)?)
        .map_err(|error| format!("failed to render second event range: {error}"))?;
    if first_json != second_json {
        return Err("same event replay range returned different JSON bytes".to_string());
    }
    Ok(())
}

async fn event_resume_cursor(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    if !capability_enabled(state, "replay") {
        return skip("target does not advertise replay capability");
    }
    let task_id = ensure_task(client, state).await?;
    let events = client
        .get_json(&format!("/v1/tasks/{task_id}/events"))
        .await?;
    expect_status(&events, StatusCode::OK)?;
    let list = expect_json(&events)?;
    let Some(first_event) = list_data(list)?.first() else {
        return skip("no task events available for resume cursor probe");
    };
    let event_id = expect_string_field(first_event, "id")?;
    let resumed = client
        .get_json(&format!(
            "/v1/tasks/{task_id}/events?after_event_id={event_id}"
        ))
        .await?;
    expect_status(&resumed, StatusCode::OK)?;
    expect_list_response(expect_json(&resumed)?, "resumed task events")?;
    Ok(())
}

async fn task_receipts(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    if !capability_enabled(state, "receipts") {
        return skip("target does not advertise receipts capability");
    }
    let task_id = ensure_task(client, state).await?;
    let response = client
        .get_json(&format!("/v1/tasks/{task_id}/receipts"))
        .await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_list_response(body, "task receipts")?;
    let data = list_data(body)?;
    let Some(receipt) = data.first() else {
        return Err("receipts capability is advertised, but task has no receipt".to_string());
    };
    expect_field_eq(receipt, "object", "receipt")?;
    let receipt_id = expect_string_field(receipt, "id")?.to_string();
    state.receipt_id = Some(receipt_id.clone());
    let body = get_required_resource(client, &format!("/v1/receipts/{receipt_id}")).await?;
    expect_field_eq(&body, "object", "receipt")?;
    expect_string_field(&body, "format")?;
    Ok(())
}

async fn receipt_verification(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    if !capability_enabled(state, "receipts") {
        return skip("target does not advertise receipts capability");
    }
    if state.receipt_id.is_none() {
        task_receipts(client, state).await?;
    }
    let Some(receipt_id) = state.receipt_id.as_deref() else {
        return skip("no receipt available for verification");
    };
    let response = client
        .post_json(
            &format!("/v1/receipts/{receipt_id}/verify"),
            &json!({}),
            Some(&format!("{}-receipt-verify", state.run_id)),
        )
        .await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    body.get("valid")
        .and_then(Value::as_bool)
        .ok_or_else(|| "receipt verification must include boolean valid".to_string())?;
    expect_string_field(body, "checked_at")?;
    Ok(())
}

async fn vault_create_and_read(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let workspace_id = ensure_workspace(client, state).await?;
    let response = client
        .post_json(
            "/v1/vaults",
            &json!({
                "workspace_id": workspace_id,
                "provider": "conformance-oauth",
                "capabilities": ["oauth_refresh"],
                "metadata": conformance_metadata(&state.run_id),
            }),
            Some(&format!("{}-vault", state.run_id)),
        )
        .await?;
    expect_status_any(
        &response,
        &[
            StatusCode::CREATED,
            StatusCode::OK,
            StatusCode::NOT_IMPLEMENTED,
        ],
    )?;
    if response.status == StatusCode::NOT_IMPLEMENTED {
        return skip("vault creation is not implemented by this target");
    }
    let body = expect_json(&response)?;
    expect_field_eq(body, "object", "vault")?;
    let serialized = serde_json::to_string(body)
        .map_err(|error| format!("failed to inspect vault body: {error}"))?;
    for forbidden in [
        "access_token",
        "refresh_token",
        "client_secret",
        "private_key",
    ] {
        if serialized.contains(forbidden) {
            return Err(format!(
                "vault response leaked secret-shaped field {forbidden}"
            ));
        }
    }
    let vault_id = expect_string_field(body, "id")?;
    let body = get_required_resource(client, &format!("/v1/vaults/{vault_id}")).await?;
    expect_field_eq(&body, "object", "vault")?;
    Ok(())
}

async fn outcome_list_and_read(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult {
    let _ = ensure_task(client, state).await?;
    let response = client.get_json("/v1/outcomes").await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_list_response(body, "outcomes")?;
    if let Some(outcome_id) = state
        .outcome_id
        .clone()
        .or_else(|| first_id_from_list(body))
    {
        let outcome = get_required_resource(client, &format!("/v1/outcomes/{outcome_id}")).await?;
        expect_field_eq(&outcome, "object", "outcome")?;
        expect_string_field(&outcome, "status")?;
    } else {
        return skip("no outcome resource available to read by id");
    }
    Ok(())
}

async fn connector_webhook_metadata(client: &ConformanceClient) -> ProbeResult {
    let response = client.get_json("/v1/connectors").await?;
    expect_status_any(&response, &[StatusCode::OK, StatusCode::NOT_IMPLEMENTED])?;
    if response.status == StatusCode::NOT_IMPLEMENTED {
        return skip("connector listing is not implemented by this target");
    }
    let body = expect_json(&response)?;
    expect_list_response(body, "connectors")?;
    let connectors = list_data(body)?;
    let Some(connector) = connectors.iter().find(|connector| {
        connector.get("webhook").is_some()
            || connector
                .get("event_kinds")
                .and_then(Value::as_array)
                .is_some_and(|events| {
                    events.iter().any(|event| {
                        event
                            .as_str()
                            .is_some_and(|event| event.contains("webhook"))
                    })
                })
    }) else {
        return skip("no webhook-capable connector advertised");
    };
    expect_field_eq(connector, "object", "connector")?;
    expect_array_field(connector, "event_kinds")?;
    Ok(())
}

async fn ensure_workspace(
    client: &ConformanceClient,
    state: &mut ProbeState,
) -> ProbeResult<String> {
    if let Some(workspace_id) = state.workspace_id.clone() {
        return Ok(workspace_id);
    }
    workspace_crud(client, state).await?;
    state
        .workspace_id
        .clone()
        .ok_or_else(|| "workspace setup did not produce a workspace id".to_string())
}

async fn ensure_session(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult<String> {
    if let Some(session_id) = state.session_id.clone() {
        return Ok(session_id);
    }
    session_crud(client, state).await?;
    state
        .session_id
        .clone()
        .ok_or_else(|| "session setup did not produce a session id".to_string())
}

async fn ensure_task(client: &ConformanceClient, state: &mut ProbeState) -> ProbeResult<String> {
    if let Some(task_id) = state.task_id.clone() {
        return Ok(task_id);
    }
    let task = submit_task(client, state, "agents conformance setup task").await?;
    let task_id = expect_string_field(&task, "id")?.to_string();
    state.task_id = Some(task_id.clone());
    remember_task_links(state, &task);
    Ok(task_id)
}

async fn submit_task(
    client: &ConformanceClient,
    state: &mut ProbeState,
    text: &str,
) -> ProbeResult<Value> {
    let session_id = ensure_session(client, state).await?;
    let response = client
        .post_json(
            &format!("/v1/sessions/{session_id}/tasks"),
            &json!({"input": message_input(text)}),
            Some(&format!("{}-task-{}", state.run_id, Uuid::new_v4())),
        )
        .await?;
    expect_status_any(&response, &[StatusCode::ACCEPTED, StatusCode::OK])?;
    let task = expect_json(&response)?.clone();
    expect_field_eq(&task, "object", "task")?;
    expect_task_status(&task)?;
    remember_task_links(state, &task);
    Ok(task)
}

async fn first_list_id(client: &ConformanceClient, path: &str) -> ProbeResult<String> {
    let response = client.get_json(path).await?;
    expect_status(&response, StatusCode::OK)?;
    let body = expect_json(&response)?;
    expect_list_response(body, path)?;
    first_id_from_list(body).ok_or_else(|| format!("{path} returned no reusable resources"))
}

async fn get_required_resource(client: &ConformanceClient, path: &str) -> ProbeResult<Value> {
    let response = client.get_json(path).await?;
    expect_status(&response, StatusCode::OK)?;
    Ok(expect_json(&response)?.clone())
}

async fn stream_sse_frames(
    client: &ConformanceClient,
    path: &str,
    min_frames: usize,
) -> ProbeResult<Vec<SseFrame>> {
    let mut request = client
        .http
        .get(client.url(path)?)
        .header("Harn-Agents-Protocol-Version", PROTOCOL_VERSION)
        .header(ACCEPT, "text/event-stream");
    if let Some(api_key) = client.api_key.as_deref() {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("GET {path} failed: {error}"))?;
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("GET {path} returned {status}: {body}"));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("text/event-stream") {
        return Err(format!(
            "GET {path} returned content-type {content_type}, expected text/event-stream"
        ));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut frames = Vec::new();
    let deadline = Instant::now() + client.timeout.min(Duration::from_secs(5));
    while frames.len() < min_frames && Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Some(chunk) = next.ok().flatten() else {
            continue;
        };
        let chunk = chunk.map_err(|error| format!("SSE read failed for {path}: {error}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find("\n\n") {
            let raw = buffer[..idx].to_string();
            buffer = buffer[idx + 2..].to_string();
            if raw.trim().is_empty() {
                continue;
            }
            frames.push(parse_sse_frame(&raw)?);
            if frames.len() >= min_frames {
                break;
            }
        }
    }

    if frames.len() < min_frames {
        return Err(format!(
            "SSE stream {path} produced {} frame(s), expected at least {min_frames}",
            frames.len()
        ));
    }
    Ok(frames)
}

#[derive(Debug)]
struct SseFrame {
    id: Option<String>,
    event: Option<String>,
    data: Option<Value>,
}

fn parse_sse_frame(raw: &str) -> ProbeResult<SseFrame> {
    let mut id = None;
    let mut event = None;
    let mut data_lines = Vec::new();
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    let data = if data_lines.is_empty() {
        None
    } else {
        Some(
            serde_json::from_str(&data_lines.join("\n"))
                .map_err(|error| format!("SSE data is not valid JSON: {error}"))?,
        )
    };
    Ok(SseFrame { id, event, data })
}

fn validate_sse_frames(frames: &[SseFrame]) -> ProbeResult {
    for frame in frames {
        let id = frame
            .id
            .as_deref()
            .ok_or_else(|| "SSE frame is missing id".to_string())?;
        if id.is_empty() {
            return Err("SSE frame id must not be empty".to_string());
        }
        let event = frame
            .event
            .as_deref()
            .ok_or_else(|| "SSE frame is missing event".to_string())?;
        if event.is_empty() {
            return Err("SSE frame event must not be empty".to_string());
        }
        let data = frame
            .data
            .as_ref()
            .ok_or_else(|| "SSE frame is missing data".to_string())?;
        expect_field_eq(data, "object", "event")?;
        expect_string_field(data, "id")?;
        expect_string_field(data, "event")?;
        expect_u64_field(data, "sequence")?;
    }
    Ok(())
}

impl ConformanceClient {
    fn new(base_url: &str, api_key: Option<String>, timeout: Duration) -> ProbeResult<Self> {
        let mut base_url = Url::parse(base_url).map_err(|error| {
            format!("invalid agents conformance target URL {base_url}: {error}")
        })?;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path().trim_end_matches('/'));
            base_url.set_path(&path);
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("failed to create HTTP client: {error}"))?;
        Ok(Self {
            http,
            base_url,
            api_key,
            timeout,
        })
    }

    fn url(&self, path: &str) -> ProbeResult<Url> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| format!("failed to resolve URL path {path}: {error}"))
    }

    async fn get_public_json(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, false, None)
            .await
    }

    async fn get_json(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, true, None)
            .await
    }

    async fn get_json_without_auth(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, false, None)
            .await
    }

    async fn post_json(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        self.request_json(Method::POST, path, Some(body), idempotency_key, true, None)
            .await
    }

    async fn patch_json(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        self.request_json(Method::PATCH, path, Some(body), idempotency_key, true, None)
            .await
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        auth: bool,
        protocol_version: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        let url = self.url(path)?;
        let mut request = self
            .http
            .request(method.clone(), url)
            .header(ACCEPT, "application/json")
            .header(
                "Harn-Agents-Protocol-Version",
                protocol_version.unwrap_or(PROTOCOL_VERSION),
            );
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if auth {
            if let Some(api_key) = self.api_key.as_deref() {
                request = request.header(AUTHORIZATION, bearer_value(api_key)?);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("{method} {path} failed: {error}"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body_text = response
            .text()
            .await
            .map_err(|error| format!("{method} {path} body read failed: {error}"))?;
        let json = if body_text.trim().is_empty() {
            None
        } else {
            serde_json::from_str(&body_text).ok()
        };
        Ok(HttpResult {
            status,
            headers,
            body: body_text,
            json,
        })
    }
}

fn bearer_value(api_key: &str) -> ProbeResult<HeaderValue> {
    HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|error| format!("invalid API key for Authorization header: {error}"))
}

fn resolve_categories(requested: &[String]) -> ProbeResult<Vec<&'static str>> {
    let all = [
        "core",
        "streaming",
        "lifecycle",
        "idempotency",
        "auth-and-scopes",
        "replay",
        "receipts",
        "vaults",
        "outcomes",
        "webhooks",
    ];
    if requested.is_empty() {
        return Ok(all.to_vec());
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in requested {
        for item in raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if !all.contains(&item) {
                return Err(format!(
                    "unknown agents conformance category `{item}`; expected one of {}",
                    all.join(", ")
                ));
            }
            if seen.insert(item.to_string()) {
                selected.push(all.iter().copied().find(|known| *known == item).unwrap());
            }
        }
    }
    Ok(selected)
}

fn build_leaderboard_report(
    target: &str,
    state: ProbeState,
    categories: Vec<CategoryReport>,
    suite_start: Instant,
) -> LeaderboardReport {
    let passed = categories.iter().map(|category| category.passed).sum();
    let failed = categories.iter().map(|category| category.failed).sum();
    let skipped = categories.iter().map(|category| category.skipped).sum();
    let total = passed + failed + skipped;
    let categories_passed = categories
        .iter()
        .filter(|category| category.status == ProbeStatus::Passed)
        .count();
    let categories_failed = categories
        .iter()
        .filter(|category| category.status == ProbeStatus::Failed)
        .count();
    let categories_skipped = categories
        .iter()
        .filter(|category| category.status == ProbeStatus::Skipped)
        .count();
    let status = if failed > 0 {
        ProbeStatus::Failed
    } else if passed > 0 {
        ProbeStatus::Passed
    } else {
        ProbeStatus::Skipped
    };
    let score = if total == skipped {
        0.0
    } else {
        passed as f64 / (passed + failed) as f64
    };
    let durations = categories
        .iter()
        .flat_map(|category| category.tests.iter().map(|test| test.duration_ms))
        .collect();
    LeaderboardReport {
        schema: REPORT_SCHEMA,
        target: target.trim_end_matches('/').to_string(),
        protocol_version: PROTOCOL_VERSION,
        generated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
        summary: SummaryReport {
            status,
            score,
            passed,
            failed,
            skipped,
            total,
            categories_passed,
            categories_failed,
            categories_skipped,
            duration_ms: suite_start.elapsed().as_millis() as u64,
        },
        percentiles_ms: percentiles(durations),
        capabilities: state.capabilities,
        categories,
    }
}

fn percentiles(mut durations: Vec<u64>) -> Percentiles {
    if durations.is_empty() {
        return Percentiles::default();
    }
    durations.sort_unstable();
    let len = durations.len();
    let avg = durations.iter().sum::<u64>() / len as u64;
    Percentiles {
        avg,
        p50: durations[len * 50 / 100],
        p95: durations[(len * 95 / 100).min(len - 1)],
        p99: durations[(len * 99 / 100).min(len - 1)],
    }
}

fn print_category_report(report: &CategoryReport, verbose: bool) {
    let label = match report.status {
        ProbeStatus::Passed => "\x1b[32mPASS\x1b[0m",
        ProbeStatus::Failed => "\x1b[31mFAIL\x1b[0m",
        ProbeStatus::Skipped => "\x1b[33mSKIP\x1b[0m",
    };
    println!(
        "{label} {:<16} {} passed, {} failed, {} skipped ({} ms)",
        report.name, report.passed, report.failed, report.skipped, report.duration_ms
    );
    for test in &report.tests {
        match test.status {
            ProbeStatus::Passed if verbose => {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {} ({} ms)",
                    test.name, test.duration_ms
                );
            }
            ProbeStatus::Failed => {
                println!("  \x1b[31mFAIL\x1b[0m  {}", test.name);
                if let Some(error) = test.error.as_deref() {
                    println!("        {error}");
                }
            }
            ProbeStatus::Skipped if verbose => {
                println!("  \x1b[33mSKIP\x1b[0m  {}", test.name);
                if let Some(error) = test.error.as_deref() {
                    println!("        {error}");
                }
            }
            _ => {}
        }
    }
}

fn print_summary(report: &LeaderboardReport) {
    println!();
    let label = match report.summary.status {
        ProbeStatus::Passed => "\x1b[32mPASS\x1b[0m",
        ProbeStatus::Failed => "\x1b[31mFAIL\x1b[0m",
        ProbeStatus::Skipped => "\x1b[33mSKIP\x1b[0m",
    };
    println!(
        "{label} agents conformance: {} passed, {} failed, {} skipped, score {:.1}% ({} ms)",
        report.summary.passed,
        report.summary.failed,
        report.summary.skipped,
        report.summary.score * 100.0,
        report.summary.duration_ms
    );
    println!(
        "Per-test: avg={} ms  p50={} ms  p95={} ms  p99={} ms",
        report.percentiles_ms.avg,
        report.percentiles_ms.p50,
        report.percentiles_ms.p95,
        report.percentiles_ms.p99
    );
}

fn conformance_metadata(run_id: &str) -> Value {
    json!({
        "harn_agents_conformance": true,
        "run_id": run_id,
    })
}

fn message_input(text: &str) -> Value {
    json!({
        "role": "user",
        "parts": [{
            "type": "text",
            "text": text,
            "visibility": "public",
        }],
    })
}

fn expect_status(response: &HttpResult, expected: StatusCode) -> ProbeResult {
    if response.status == expected {
        Ok(())
    } else {
        Err(format!(
            "expected HTTP {}, got {}{}",
            expected.as_u16(),
            response.status.as_u16(),
            body_suffix(response)
        ))
    }
}

fn expect_status_any(response: &HttpResult, expected: &[StatusCode]) -> ProbeResult {
    if expected.contains(&response.status) {
        Ok(())
    } else {
        let expected = expected
            .iter()
            .map(|status| status.as_u16().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "expected HTTP one of [{}], got {}{}",
            expected,
            response.status.as_u16(),
            body_suffix(response)
        ))
    }
}

fn body_suffix(response: &HttpResult) -> String {
    let mut suffix = String::new();
    if let Some(content_type) = response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        suffix.push_str(&format!(" content-type={content_type}"));
    }
    if !response.body.trim().is_empty() {
        let body = response.body.trim();
        suffix.push_str(": ");
        suffix.push_str(if body.len() > 500 { &body[..500] } else { body });
    }
    suffix
}

fn expect_json(response: &HttpResult) -> ProbeResult<&Value> {
    response.json.as_ref().ok_or_else(|| {
        format!(
            "expected JSON response for HTTP {}, body was {}",
            response.status, response.body
        )
    })
}

fn expect_list_response<'a>(value: &'a Value, label: &str) -> ProbeResult<&'a [Value]> {
    let data = list_data(value)?;
    value
        .get("object")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} list is missing object discriminator"))?;
    Ok(data)
}

fn list_data(value: &Value) -> ProbeResult<&[Value]> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "list response must include data array".to_string())
}

fn expect_field_eq<'a>(value: &'a Value, field: &str, expected: &str) -> ProbeResult<&'a str> {
    let actual = expect_string_field(value, field)?;
    if actual == expected {
        Ok(actual)
    } else {
        Err(format!("{field} must be {expected:?}, got {actual:?}"))
    }
}

fn expect_string_field<'a>(value: &'a Value, field: &str) -> ProbeResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn expect_u64_field(value: &Value, field: &str) -> ProbeResult<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field} must be an unsigned integer"))
}

fn expect_array_field<'a>(value: &'a Value, field: &str) -> ProbeResult<&'a [Value]> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{field} must be an array"))
}

fn expect_object_field<'a>(value: &'a Value, field: &str) -> ProbeResult<&'a Value> {
    let object = value
        .get(field)
        .ok_or_else(|| format!("{field} must be an object"))?;
    if object.as_object().is_some() {
        Ok(object)
    } else {
        Err(format!("{field} must be an object"))
    }
}

fn expect_string_array_contains(value: &Value, field: &str, expected: &str) -> ProbeResult {
    let values = expect_array_field(value, field)?;
    if values.iter().any(|value| value.as_str() == Some(expected)) {
        Ok(())
    } else {
        Err(format!("{field} must contain {expected}"))
    }
}

fn expect_task_status(value: &Value) -> ProbeResult<&str> {
    let status = expect_string_field(value, "status")?;
    if is_task_status(status) {
        Ok(status)
    } else {
        Err(format!("unknown task status {status:?}"))
    }
}

fn is_task_status(status: &str) -> bool {
    matches!(
        status,
        "SUBMITTED"
            | "WORKING"
            | "INPUT_REQUIRED"
            | "AUTH_REQUIRED"
            | "COMPLETED"
            | "FAILED"
            | "CANCELED"
    )
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "COMPLETED" | "FAILED" | "CANCELED")
}

fn validate_status_transitions(statuses: &[String]) -> ProbeResult {
    for pair in statuses.windows(2) {
        let from = pair[0].as_str();
        let to = pair[1].as_str();
        let allowed = match from {
            "SUBMITTED" => matches!(to, "WORKING" | "CANCELED" | "FAILED"),
            "WORKING" => matches!(
                to,
                "INPUT_REQUIRED" | "AUTH_REQUIRED" | "COMPLETED" | "FAILED" | "CANCELED"
            ),
            "INPUT_REQUIRED" | "AUTH_REQUIRED" => matches!(to, "WORKING" | "FAILED" | "CANCELED"),
            "COMPLETED" | "FAILED" | "CANCELED" => false,
            _ => false,
        };
        if !allowed {
            return Err(format!("invalid task status transition {from} -> {to}"));
        }
    }
    Ok(())
}

fn expect_error_code(response: &HttpResult, expected: &str) -> ProbeResult {
    let actual = error_code(response)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected error code {expected}, got {actual}"))
    }
}

fn error_code(response: &HttpResult) -> ProbeResult<String> {
    let body = expect_json(response)?;
    body.get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "error response must include error.code".to_string())
}

fn first_id_from_list(value: &Value) -> Option<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn remember_first_event(state: &mut ProbeState, list: &Value) {
    if state.event_id.is_none() {
        state.event_id = first_id_from_list(list);
    }
}

fn remember_task_links(state: &mut ProbeState, task: &Value) {
    if state.outcome_id.is_none() {
        state.outcome_id = task
            .get("outcome_id")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    if state.receipt_id.is_none() {
        state.receipt_id = task
            .get("receipt_id")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
}

fn capability_enabled(state: &ProbeState, capability: &str) -> bool {
    state.capabilities.get(capability).copied().unwrap_or(false)
}

fn skip<T>(reason: &str) -> ProbeResult<T> {
    Err(format!("SKIP: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_categories() {
        let categories = resolve_categories(&[]).unwrap();
        assert_eq!(categories[0], "core");
        assert!(categories.contains(&"receipts"));
    }

    #[test]
    fn parses_comma_separated_categories_once() {
        let categories =
            resolve_categories(&["core,streaming".to_string(), "core".to_string()]).unwrap();
        assert_eq!(categories, vec!["core", "streaming"]);
    }

    #[test]
    fn rejects_unknown_category() {
        let error = resolve_categories(&["bogus".to_string()]).unwrap_err();
        assert!(error.contains("unknown agents conformance category"));
    }

    #[test]
    fn validates_task_transitions() {
        validate_status_transitions(&[
            "SUBMITTED".to_string(),
            "WORKING".to_string(),
            "COMPLETED".to_string(),
        ])
        .unwrap();
        assert!(
            validate_status_transitions(&["COMPLETED".to_string(), "WORKING".to_string()]).is_err()
        );
    }

    #[test]
    fn parses_sse_frame() {
        let frame = parse_sse_frame(
            "id: evt_1\nevent: task.started\ndata: {\"object\":\"event\",\"id\":\"evt_1\",\"event\":\"task.started\",\"sequence\":1}\n",
        )
        .unwrap();
        assert_eq!(frame.id.as_deref(), Some("evt_1"));
        assert_eq!(frame.event.as_deref(), Some("task.started"));
        validate_sse_frames(&[frame]).unwrap();
    }
}
