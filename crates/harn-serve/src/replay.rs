use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::DispatchError;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplayKey(pub String);

#[derive(Clone, Debug, PartialEq)]
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
