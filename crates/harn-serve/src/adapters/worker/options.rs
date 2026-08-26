use std::time::Duration as StdDuration;

use harn_vm::{ConnectorRegistry, RetryPolicy, TenantScope, TriggerRetryConfig};

use super::{DEFAULT_CLAIM_TTL, DEFAULT_SHUTDOWN_DRAIN};

pub struct WorkerServeOptions {
    pub consumer_id: Option<String>,
    pub claim_ttl: StdDuration,
    pub drain_timeout: StdDuration,
    /// Connector definitions resolved from the entry package by the host.
    pub connector_registry: Option<ConnectorRegistry>,
    /// Tenant authority resolved by the host for every dispatch in this worker.
    pub tenant_scope: Option<TenantScope>,
}

impl Default for WorkerServeOptions {
    fn default() -> Self {
        Self {
            consumer_id: None,
            claim_ttl: DEFAULT_CLAIM_TTL,
            drain_timeout: DEFAULT_SHUTDOWN_DRAIN,
            connector_registry: None,
            tenant_scope: None,
        }
    }
}

/// Driver-level knobs for a one-shot `@job` run.
///
/// The default preserves the job's declared retry policy and installs no
/// package connectors. Hosts that resolve a package manifest can pass its
/// connector registry through [`Self::with_connector_registry`].
#[derive(Default)]
pub struct JobRunOptions {
    /// Retry policy for this dispatch, or the job's declared policy when absent.
    pub retry_override: Option<TriggerRetryConfig>,
    /// Connector definitions resolved from the entry package by the host.
    pub connector_registry: Option<ConnectorRegistry>,
    /// Tenant authority resolved by the host for this dispatch.
    pub tenant_scope: Option<TenantScope>,
}

impl JobRunOptions {
    /// Replace the job's declared retry policy for this dispatch.
    pub fn with_retry(mut self, retry: TriggerRetryConfig) -> Self {
        self.retry_override = Some(retry);
        self
    }

    /// Limit the dispatch to one attempt with no backoff.
    pub fn fail_fast() -> Self {
        Self {
            retry_override: Some(TriggerRetryConfig::new(
                1,
                RetryPolicy::Linear { delay_ms: 0 },
            )),
            connector_registry: None,
            tenant_scope: None,
        }
    }

    /// Install connector definitions resolved from the entry package.
    pub fn with_connector_registry(mut self, registry: ConnectorRegistry) -> Self {
        self.connector_registry = Some(registry);
        self
    }

    /// Run under a tenant scope already authenticated by the host.
    pub fn with_tenant_scope(mut self, scope: TenantScope) -> Self {
        self.tenant_scope = Some(scope);
        self
    }
}
