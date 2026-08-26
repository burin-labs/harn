//! How a hosted worker resolves credentials.
//!
//! Split from the worker's own test module because these are contract claims
//! about the secret boundary rather than tests of the dispatch lifecycle, and
//! they each need a paired control to mean anything.

use super::test_support::{write_script, ScopedEnvVar, ENV_LOCK};

use super::*;

/// The canonical worker path resolves a configured credential.
///
/// The reported defect was that a hosted worker built its connector context
/// with an empty secret provider, so a credentialed connector reported a
/// missing key even when the credential was configured. This runs a `@job`
/// through `run_job_from_files` — the same path the hosted worker uses —
/// and requires it to read a credential the process has configured.
///
/// The job returns a fixed marker rather than the secret. Asserting on the
/// value would put it in a test binary's output on failure, and the one
/// thing this seam must never do is copy secret values anywhere.
#[tokio::test(flavor = "current_thread")]
async fn a_job_resolves_a_configured_credential() {
    let _guard = ENV_LOCK.lock().await;
    let _chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "env");
    let _value = ScopedEnvVar::set("HARN_SECRET_WORKERTEST_API_TOKEN", "configured");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("read_credential")
pub fn read_credential(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("workertest/api-token")
  return {resolved: len(token) > 0}
}
"#,
            )
            .await;
            // Fail fast first. A regression here means the read fails, and
            // the default retry policy would spend a minute backing off
            // before the file-oriented call below reported anything.
            let probe = run_job_once_with_options(
                &script,
                "read_credential",
                serde_json::json!({}),
                JobRunOptions::fail_fast(),
                |_vm| {},
            )
            .await
            .expect("run credentialed job");
            assert!(
                probe.succeeded(),
                "the worker did not resolve a configured credential: {:?}",
                probe.error
            );

            // Then the same job through the file-oriented entrypoint the
            // hosted worker actually uses.
            let request_path = dir.path().join("req.json");
            tokio::fs::write(&request_path, "{}")
                .await
                .expect("write request");

            let (outcome, rendered) =
                run_job_from_files(&script, "read_credential", &request_path, None, false)
                    .await
                    .expect("run credentialed job");

            assert!(outcome.succeeded(), "job failed: {rendered}");
            let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("parse report");
            assert_eq!(parsed["resolved"], serde_json::json!(true));
            assert!(
                !rendered.contains("configured"),
                "the report must not carry the secret's value"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tenanted_jobs_resolve_only_their_own_present_credential() {
    let _guard = ENV_LOCK.lock().await;
    let _chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "env");
    let _tenant_a = ScopedEnvVar::set(
        "HARN_SECRET_HARN_TENANT_TENANT_A_WORKERTEST_API_TOKEN",
        "tenant-a-value",
    );
    let _tenant_b = ScopedEnvVar::set(
        "HARN_SECRET_HARN_TENANT_TENANT_B_WORKERTEST_API_TOKEN",
        "tenant-b-value",
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("read_own")
pub fn read_own(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("workertest/api-token")
  return {resolved: len(token) > 0, tenant: harness.tenant.id()}
}

@job("read_other")
pub fn read_other(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("harn.tenant.tenant-b.workertest/api-token")
  return {resolved: len(token) > 0}
}

@job("read_missing")
pub fn read_missing(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("workertest/not-stored")
  return {resolved: len(token) > 0}
}
"#,
            )
            .await;
            let scope = |tenant: &str| {
                harn_vm::TenantScope::new(harn_vm::TenantId::new(tenant), dir.path())
                    .expect("tenant scope")
            };

            let tenant_a = run_job_once_with_options(
                &script,
                "read_own",
                serde_json::json!({}),
                JobRunOptions::fail_fast().with_tenant_scope(scope("tenant-a")),
                |_vm| {},
            )
            .await
            .expect("run tenant-a job");
            assert!(
                tenant_a.succeeded(),
                "tenant-a failed: {:?}",
                tenant_a.error
            );
            assert_eq!(tenant_a.result.as_ref().unwrap()["tenant"], "tenant-a");

            let tenant_b = run_job_once_with_options(
                &script,
                "read_own",
                serde_json::json!({}),
                JobRunOptions::fail_fast().with_tenant_scope(scope("tenant-b")),
                |_vm| {},
            )
            .await
            .expect("run tenant-b job");
            assert!(
                tenant_b.succeeded(),
                "tenant-b failed: {:?}",
                tenant_b.error
            );
            assert_eq!(tenant_b.result.as_ref().unwrap()["tenant"], "tenant-b");

            let denied = run_job_once_with_options(
                &script,
                "read_other",
                serde_json::json!({}),
                JobRunOptions::fail_fast().with_tenant_scope(scope("tenant-a")),
                |_vm| {},
            )
            .await
            .expect("the worker starts; only cross-tenant access fails");
            assert!(!denied.succeeded());
            let report = serde_json::to_string(&denied.report_json()).expect("render report");
            assert!(report.contains("denied"), "unexpected denial: {report}");
            assert!(!report.contains("tenant-a-value"));
            assert!(!report.contains("tenant-b-value"));

            let missing = run_job_once_with_options(
                &script,
                "read_missing",
                serde_json::json!({}),
                JobRunOptions::fail_fast().with_tenant_scope(scope("tenant-a")),
                |_vm| {},
            )
            .await
            .expect("the worker starts; only the credential is absent");
            let missing_report =
                serde_json::to_string(&missing.report_json()).expect("render missing report");
            assert!(!missing.succeeded());
            assert!(missing_report.contains("not found"));
            assert!(!missing_report.contains("denied"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tenanted_worker_server_claims_only_its_own_durable_jobs() {
    let _guard = ENV_LOCK.lock().await;
    let _chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "env");
    let _tenant_a = ScopedEnvVar::set(
        "HARN_SECRET_HARN_TENANT_TENANT_A_WORKERTEST_API_TOKEN",
        "tenant-a-durable-value",
    );
    let _tenant_b = ScopedEnvVar::set(
        "HARN_SECRET_HARN_TENANT_TENANT_B_WORKERTEST_API_TOKEN",
        "tenant-b-durable-value",
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("read_own")
@queue("tenant-jobs")
pub fn read_own(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("workertest/api-token")
  return {resolved: len(token) > 0, tenant: harness.tenant.id()}
}
"#,
            )
            .await;
            let scope = |tenant: &str| {
                harn_vm::TenantScope::new(harn_vm::TenantId::new(tenant), dir.path())
                    .expect("tenant scope")
            };
            let server = start_worker_server(
                &script,
                WorkerServeOptions {
                    consumer_id: Some("tenant-a-worker".to_string()),
                    drain_timeout: StdDuration::from_secs(5),
                    tenant_scope: Some(scope("tenant-a")),
                    ..WorkerServeOptions::default()
                },
            )
            .await
            .expect("start tenant worker");
            let registration = server.jobs().first().expect("job registration").clone();
            let event_log = server.event_log();
            let queue = WorkerQueue::new(event_log.clone());
            let response_topic = Topic::new(harn_vm::worker_response_topic_name("tenant-jobs"))
                .expect("response topic");
            let latest = event_log
                .latest(&response_topic)
                .await
                .expect("latest response");
            let mut responses = event_log
                .clone()
                .subscribe(&response_topic, latest)
                .await
                .expect("subscribe responses");

            let enqueue = |tenant: &str| harn_vm::WorkerQueueJob {
                queue: "tenant-jobs".to_string(),
                trigger_id: registration.binding_id.clone(),
                binding_key: registration.binding_key.clone(),
                binding_version: registration.binding_version,
                event: job_event("read_own", serde_json::json!({}), Some(scope(tenant).id)),
                replay_of_event_id: None,
                priority: WorkerQueuePriority::Normal,
            };
            let foreign = queue
                .enqueue(&enqueue("tenant-b"))
                .await
                .expect("enqueue foreign job");
            let own = queue
                .enqueue(&enqueue("tenant-a"))
                .await
                .expect("enqueue own job");

            let response = tokio::time::timeout(StdDuration::from_secs(5), async {
                loop {
                    let (_, event) = responses
                        .next()
                        .await
                        .expect("response stream ended")
                        .expect("response event");
                    if event.kind == "job_response" {
                        break serde_json::from_value::<WorkerQueueResponseRecord>(event.payload)
                            .expect("response record");
                    }
                }
            })
            .await
            .expect("tenant worker response");
            assert_eq!(response.job_event_id, own.job_event_id);
            let outcome = response.outcome.expect("dispatch outcome");
            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(outcome.result.expect("result")["tenant"], "tenant-a");

            let state = queue.queue_state("tenant-jobs").await.expect("queue state");
            let foreign = state
                .jobs
                .iter()
                .find(|job| job.job_event_id == foreign.job_event_id)
                .expect("foreign job state");
            assert!(!foreign.acked);
            assert!(foreign.active_claim.is_none());

            for topic in event_log.topics().await.expect("event log topics") {
                for (_, event) in event_log
                    .read_range(&topic, None, usize::MAX)
                    .await
                    .expect("durable events")
                {
                    let record = serde_json::to_string(&event).expect("serialize durable event");
                    assert!(!record.contains("tenant-a-durable-value"));
                    assert!(!record.contains("tenant-b-durable-value"));
                }
            }

            let report = server.shutdown().await.expect("shutdown worker");
            assert!(report.drained);
        })
        .await;
}

/// A credential that was never stored is a different fault from a host with
/// no secret backend at all, and the two must not read the same.
///
/// This is the pair the issue asks for. The negative control is the
/// point: without it, "the worker reports a failure" would be satisfied by
/// the broken behavior too.
#[tokio::test(flavor = "current_thread")]
async fn a_missing_credential_and_a_missing_backend_are_different_failures() {
    let _guard = ENV_LOCK.lock().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("read_credential")
pub fn read_credential(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("workertest/never-stored")
  return {resolved: len(token) > 0}
}
"#,
            )
            .await;
            // A configured backend that does not hold this credential: the
            // job runs and fails on the read. `fail_fast` because this is a
            // failure path and the dispatcher would otherwise retry it.
            let missing_credential = {
                let _chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "env");
                run_job_once_with_options(
                    &script,
                    "read_credential",
                    serde_json::json!({}),
                    JobRunOptions::fail_fast(),
                    |_vm| {},
                )
                .await
                .expect("the worker starts; only the read fails")
            };
            assert!(
                !missing_credential.succeeded(),
                "an absent credential must not read as success"
            );

            // No backend at all: the worker refuses to start, and says so
            // as a backend fault rather than letting every read report a
            // credential that was never there.
            let _empty_chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "");
            let missing_backend = run_job_once_with_options(
                &script,
                "read_credential",
                serde_json::json!({}),
                JobRunOptions::fail_fast(),
                |_vm| {},
            )
            .await
            .expect_err("a host with no secret backend must fail loudly");
            assert!(
                matches!(missing_backend, DispatchError::SecretBackend(_)),
                "expected a typed backend fault, got: {missing_backend:?}"
            );
            assert!(
                missing_backend.message().contains("zero providers"),
                "the diagnostic must name the misconfiguration: {}",
                missing_backend.message()
            );
        })
        .await;
}

/// A secret provider chain the host cannot build is a backend fault too,
/// and it must not be mistaken for a run-time failure of the job.
#[tokio::test(flavor = "current_thread")]
async fn an_unbuildable_secret_chain_is_a_backend_fault() {
    let _guard = ENV_LOCK.lock().await;
    let _chain = ScopedEnvVar::set(
        harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV,
        "not-a-provider",
    );
    // `expect_err` would need the provider handle to be `Debug`, which a
    // secret provider deliberately is not.
    let Err(error) = worker_secret_provider(None) else {
        panic!("an unknown provider name must fail");
    };
    assert!(
        matches!(error, DispatchError::SecretBackend(_)),
        "expected a typed backend fault, got: {error:?}"
    );
    assert!(
        error.message().contains("not-a-provider"),
        "the diagnostic must name the offending configuration: {}",
        error.message()
    );
}

/// Secrets stay keyed by their own namespace, so a credential stored for one
/// tenant is not reachable by naming another.
///
/// Both halves run against the same configuration in the same process, and
/// that pairing is what makes the test mean anything. "tenantb does not
/// resolve" is equally true of a worker with no secret provider at all, so
/// on its own this would pass against the very wiring the change replaces.
/// It only says something once tenanta resolving proves the provider is
/// live.
#[tokio::test(flavor = "current_thread")]
async fn one_tenants_credential_is_not_reachable_by_naming_another() {
    let _guard = ENV_LOCK.lock().await;
    let _chain = ScopedEnvVar::set(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV, "env");
    let _value = ScopedEnvVar::set("HARN_SECRET_TENANTA_API_TOKEN", "configured");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("read_own")
pub fn read_own(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("tenanta/api-token")
  return {resolved: len(token) > 0}
}

@job("read_other")
pub fn read_other(harness: Harness, event: TriggerEvent) -> dict {
  const token = harness.secrets.read("tenantb/api-token")
  return {resolved: len(token) > 0}
}
"#,
            )
            .await;

            let own = run_job_once_with_options(
                &script,
                "read_own",
                serde_json::json!({}),
                JobRunOptions::fail_fast(),
                |_vm| {},
            )
            .await
            .expect("run own-tenant job");
            assert!(
                own.succeeded(),
                "the provider must be live, or the other half proves nothing"
            );

            let other = run_job_once_with_options(
                &script,
                "read_other",
                serde_json::json!({}),
                JobRunOptions::fail_fast(),
                |_vm| {},
            )
            .await
            .expect("the worker starts; only the read fails");
            assert!(
                !other.succeeded(),
                "tenantb must not resolve tenanta's credential"
            );
            let rendered = serde_json::to_string(&other.report_json()).expect("render report");
            assert!(
                !rendered.contains("configured"),
                "the report must not carry the other namespace's value"
            );
        })
        .await;
}
