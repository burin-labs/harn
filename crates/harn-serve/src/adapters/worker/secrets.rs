//! The one secret boundary a hosted worker resolves credentials through.

use std::sync::Arc;

use crate::DispatchError;

/// The secret provider every worker-hosted job and connector resolves through.
///
/// One boundary, on purpose. The worker used to build its connector context
/// with an empty chain under its own `harn-worker` namespace, so a credentialed
/// connector reported a missing key even when the credential was stored
/// correctly — and populating that chain alone would not have fixed it, because
/// the namespace a chain is built with is the one its providers key on. A
/// worker reading `harn-worker` could never see what `harn connect` wrote.
///
/// [`configured_secret_chain`] is the same constructor the CLI run path, the
/// orchestrator, and `harn connect` already use, over the one namespace
/// `configured_secret_namespace` names. Reusing it is what makes a credential
/// that is storable also usable from a hosted worker.
///
/// An empty chain fails here rather than at the first `get`. Resolving nothing
/// because no backend is configured and resolving nothing because the
/// credential was never stored are different faults with different fixes, and
/// `SecretError::NoProviders` is indistinguishable from `NotFound` by the time
/// it has been flattened into a connector error. Failing at startup keeps the
/// misconfiguration named as one.
///
/// The provider is a handle. No secret value passes through this function, and
/// none is logged or recorded by it.
pub(super) fn worker_secret_provider(
) -> Result<Arc<dyn harn_vm::secrets::SecretProvider>, DispatchError> {
    let chain = harn_vm::secrets::configured_secret_chain()
        .map_err(|error| DispatchError::SecretBackend(error.to_string()))?;
    if chain.providers().is_empty() {
        return Err(DispatchError::SecretBackend(format!(
            "the secret provider chain for namespace '{}' resolved to zero providers; \
             set HARN_SECRET_PROVIDERS to a supported chain (default: env,keyring)",
            harn_vm::secrets::configured_secret_namespace()
        )));
    }
    Ok(Arc::new(chain))
}

/// The harness a job VM runs with.
///
/// `Harness::real()` carries no secret provider, so a `@job` calling
/// `harness.secrets.read(...)` failed with "no secret provider bound to this
/// harness" -- the same missing wiring the connector context had, wearing a
/// second symptom. Both take the provider from one place so they cannot drift
/// into disagreeing about which credentials a worker can see.
pub(super) fn worker_job_harness(
    provider: Arc<dyn harn_vm::secrets::SecretProvider>,
) -> harn_vm::Harness {
    harn_vm::Harness::real().with_secret_provider(provider)
}
