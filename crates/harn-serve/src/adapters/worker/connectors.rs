use std::sync::Arc;

use harn_vm::event_log::AnyEventLog;
use harn_vm::{ConnectorRegistry, MetricsRegistry, RateLimitConfig, RateLimiterFactory};

use crate::DispatchError;

pub(super) async fn install_worker_connector_clients(
    registry: ConnectorRegistry,
    event_log: Arc<AnyEventLog>,
    secrets: Arc<dyn harn_vm::secrets::SecretProvider>,
) -> Result<harn_vm::ActiveConnectorClientsGuard, DispatchError> {
    let metrics = Arc::new(MetricsRegistry::default());
    let inbox = Arc::new(
        harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
            .await
            .map_err(|error| {
                DispatchError::Execution(format!(
                    "failed to initialize worker connector inbox: {error}"
                ))
            })?,
    );
    registry
        .init_all(harn_vm::ConnectorCtx {
            event_log,
            secrets,
            inbox,
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::new(RateLimitConfig::default())),
        })
        .await
        .map_err(|error| {
            DispatchError::Execution(format!("failed to initialize worker connector: {error}"))
        })?;
    Ok(harn_vm::scope_active_connector_clients(
        registry.client_map().await,
    ))
}
