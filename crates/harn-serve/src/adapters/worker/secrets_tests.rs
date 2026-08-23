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
    let Err(error) = worker_secret_provider() else {
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
