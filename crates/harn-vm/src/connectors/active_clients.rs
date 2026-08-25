use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{ConnectorClient, ProviderId};

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
}
