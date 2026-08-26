use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::{ClientError, ConnectorClient, ProviderId};

/// Execution-owned fallback for connector clients that are expensive to
/// construct until a script actually names their provider.
#[async_trait]
pub trait ConnectorClientResolver: Send + Sync {
    async fn resolve(
        &self,
        provider: &str,
    ) -> Result<Option<Arc<dyn ConnectorClient>>, ClientError>;
}

/// Connector projection carried by a VM tree rather than ambient thread
/// state. Child VMs share the same resolver and therefore the same lazy cache
/// even when Tokio migrates their tasks between worker threads.
#[derive(Clone, Default)]
pub struct VmConnectorClients {
    clients: Arc<BTreeMap<String, Arc<dyn ConnectorClient>>>,
    resolver: Option<Arc<dyn ConnectorClientResolver>>,
}

impl VmConnectorClients {
    pub fn new(
        clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>>,
        resolver: Option<Arc<dyn ConnectorClientResolver>>,
    ) -> Self {
        Self {
            clients: Arc::new(client_map(clients)),
            resolver,
        }
    }

    pub async fn resolve(
        &self,
        provider: &str,
    ) -> Result<Option<Arc<dyn ConnectorClient>>, ClientError> {
        // Project declarations can intentionally replace a built-in provider,
        // so the execution-owned resolver gets first refusal. Falling back to
        // the core map first would silently select the wrong implementation.
        if let Some(resolver) = self.resolver.as_ref() {
            if let Some(client) = resolver.resolve(provider).await? {
                return Ok(Some(client));
            }
        }
        Ok(self.clients.get(provider).cloned())
    }
}

thread_local! {
    static ACTIVE_CONNECTOR_CLIENTS: RefCell<BTreeMap<String, Arc<dyn ConnectorClient>>> =
        RefCell::new(BTreeMap::new());
}

pub fn install_active_connector_clients(clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>>) {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| *slot.borrow_mut() = client_map(clients));
}

/// Keep one connector client map active for the lifetime of the returned guard.
///
/// Leaving a nested runtime restores the host's prior connector projection.
pub fn scope_active_connector_clients(
    clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>>,
) -> ActiveConnectorClientsGuard {
    let previous = ACTIVE_CONNECTOR_CLIENTS
        .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), client_map(clients)));
    ActiveConnectorClientsGuard { previous }
}

pub struct ActiveConnectorClientsGuard {
    previous: BTreeMap<String, Arc<dyn ConnectorClient>>,
}

impl Drop for ActiveConnectorClientsGuard {
    fn drop(&mut self) {
        ACTIVE_CONNECTOR_CLIENTS.with(|slot| {
            *slot.borrow_mut() = std::mem::take(&mut self.previous);
        });
    }
}

pub fn active_connector_client(provider: &str) -> Option<Arc<dyn ConnectorClient>> {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| slot.borrow().get(provider).cloned())
}

pub fn clear_active_connector_clients() {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| slot.borrow_mut().clear());
}

fn client_map(
    clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>>,
) -> BTreeMap<String, Arc<dyn ConnectorClient>> {
    clients
        .into_iter()
        .map(|(provider, client)| (provider.as_str().to_string(), client))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::Value as JsonValue;

    use super::*;
    use crate::connectors::ClientError;

    struct NamedClient(&'static str);

    #[async_trait]
    impl ConnectorClient for NamedClient {
        async fn call(&self, _method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
            Ok(JsonValue::String(self.0.to_string()))
        }
    }

    struct OnceResolver {
        initializations: AtomicUsize,
        clients: tokio::sync::OnceCell<BTreeMap<ProviderId, Arc<dyn ConnectorClient>>>,
    }

    #[async_trait]
    impl ConnectorClientResolver for OnceResolver {
        async fn resolve(
            &self,
            provider: &str,
        ) -> Result<Option<Arc<dyn ConnectorClient>>, ClientError> {
            let clients = self
                .clients
                .get_or_init(|| async {
                    self.initializations.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    BTreeMap::from([(
                        ProviderId::from("core"),
                        Arc::new(NamedClient("project")) as Arc<dyn ConnectorClient>,
                    )])
                })
                .await;
            Ok(clients.get(&ProviderId::from(provider)).cloned())
        }
    }

    #[test]
    fn nested_client_scope_restores_the_host_projection() {
        clear_active_connector_clients();
        install_active_connector_clients(BTreeMap::from([(
            ProviderId::from("outer"),
            Arc::new(NamedClient("outer")) as Arc<dyn ConnectorClient>,
        )]));

        {
            let _inner = scope_active_connector_clients(BTreeMap::from([(
                ProviderId::from("inner"),
                Arc::new(NamedClient("inner")) as Arc<dyn ConnectorClient>,
            )]));
            assert!(active_connector_client("inner").is_some());
            assert!(active_connector_client("outer").is_none());
        }

        assert!(active_connector_client("outer").is_some());
        assert!(active_connector_client("inner").is_none());
        clear_active_connector_clients();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vm_resolver_is_once_only_across_tasks_and_overrides_core_clients() {
        let resolver = Arc::new(OnceResolver {
            initializations: AtomicUsize::new(0),
            clients: tokio::sync::OnceCell::new(),
        });
        let clients = VmConnectorClients::new(
            BTreeMap::from([(
                ProviderId::from("core"),
                Arc::new(NamedClient("builtin")) as Arc<dyn ConnectorClient>,
            )]),
            Some(resolver.clone()),
        );

        let tasks = (0..8)
            .map(|_| {
                let clients = clients.clone();
                tokio::spawn(async move {
                    let client = clients
                        .resolve("core")
                        .await
                        .expect("resolve")
                        .expect("project override");
                    client.call("ping", JsonValue::Null).await.expect("call")
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            assert_eq!(
                task.await.expect("task"),
                JsonValue::String("project".to_string())
            );
        }
        assert_eq!(resolver.initializations.load(Ordering::SeqCst), 1);
    }
}
