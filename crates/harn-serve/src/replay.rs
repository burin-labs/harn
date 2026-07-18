use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::DispatchError;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplayKey(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayCacheEntry {
    pub value: serde_json::Value,
    pub printed_output: String,
}

#[async_trait]
pub trait ReplayCache: Send + Sync {
    async fn get(&self, key: &ReplayKey) -> Result<Option<ReplayCacheEntry>, DispatchError>;
    async fn put(&self, key: ReplayKey, value: ReplayCacheEntry) -> Result<(), DispatchError>;
}

#[derive(Clone, Default)]
pub struct InMemoryReplayCache {
    entries: Arc<RwLock<HashMap<ReplayKey, ReplayCacheEntry>>>,
}

impl InMemoryReplayCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ReplayCache for InMemoryReplayCache {
    async fn get(&self, key: &ReplayKey) -> Result<Option<ReplayCacheEntry>, DispatchError> {
        Ok(self.entries.read().await.get(key).cloned())
    }

    async fn put(&self, key: ReplayKey, value: ReplayCacheEntry) -> Result<(), DispatchError> {
        self.entries.write().await.insert(key, value);
        Ok(())
    }
}

/// A replay cache that never caches — every `get` misses and every `put`
/// is dropped.
///
/// Dispatch replay is opt-in through an adapter-supplied retry identity. A
/// host can still install this cache to disable replay globally, including
/// for callers that supply such an identity. `harn serve site` uses it as a
/// defense in depth: two HTTP requests with identical bodies remain distinct
/// requests and each runs the handler's side effects.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoReplayCache;

#[async_trait]
impl ReplayCache for NoReplayCache {
    async fn get(&self, _key: &ReplayKey) -> Result<Option<ReplayCacheEntry>, DispatchError> {
        Ok(None)
    }

    async fn put(&self, _key: ReplayKey, _value: ReplayCacheEntry) -> Result<(), DispatchError> {
        Ok(())
    }
}
