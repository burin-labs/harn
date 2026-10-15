//! Pool registry ownership across native polls and spawned tasks.

use super::PoolRegistry;
use std::sync::{Arc, OnceLock};

tokio::task_local! {
    static ACTIVE_POOL_REGISTRY: Arc<PoolRegistry>;
}

static FALLBACK_POOL_REGISTRY: OnceLock<Arc<PoolRegistry>> = OnceLock::new();

pub(crate) fn new_pool_registry() -> Arc<PoolRegistry> {
    Arc::new(PoolRegistry::default())
}

pub(crate) fn with_pool_registry_poll<T>(
    registry: &Arc<PoolRegistry>,
    poll: impl FnOnce() -> T,
) -> T {
    ACTIVE_POOL_REGISTRY.sync_scope(Arc::clone(registry), poll)
}

fn fallback_pool_registry() -> Arc<PoolRegistry> {
    FALLBACK_POOL_REGISTRY
        .get_or_init(new_pool_registry)
        .clone()
}

pub(crate) fn current_pool_registry() -> Arc<PoolRegistry> {
    ACTIVE_POOL_REGISTRY
        .try_with(Arc::clone)
        .unwrap_or_else(|_| fallback_pool_registry())
}
