//! The runtime connector registry: which connectors exist, what credentials
//! they declared, and the clients that dispatch calls to them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::secrets::{SecretId, SecretProvider};
use crate::triggers::{registered_provider_metadata, ProviderId, ProviderRuntimeMetadata};

use super::defaults::default_connector_for_provider;
use super::secret_injection::{with_declared_secrets, DeclaredConnectorSecrets};
use super::{
    ActivationHandle, Connector, ConnectorClient, ConnectorCtx, ConnectorError, ConnectorHandle,
    TriggerRegistry,
};

/// Runtime connector registry keyed by provider id.
pub struct ConnectorRegistry {
    connectors: BTreeMap<ProviderId, ConnectorHandle>,
    declared_secrets: DeclaredConnectorSecrets,
    /// Captured from the first [`ConnectorRegistry::init_all`] so `client_map`
    /// can resolve declared credentials without the caller re-supplying the
    /// store it already handed the registry.
    secrets: Mutex<Option<Arc<dyn SecretProvider>>>,
}

impl ConnectorRegistry {
    pub fn empty() -> Self {
        Self {
            connectors: BTreeMap::new(),
            declared_secrets: BTreeMap::new(),
            secrets: Mutex::new(None),
        }
    }

    pub fn with_defaults() -> Self {
        Self::with_defaults_and_clock(harn_clock::RealClock::arc())
    }

    /// Build the default connector set with a shared clock for time-driven providers.
    pub fn with_defaults_and_clock(clock: Arc<dyn harn_clock::Clock>) -> Self {
        let mut registry = Self::empty();
        for provider in registered_provider_metadata() {
            if !matches!(provider.runtime, ProviderRuntimeMetadata::Builtin { .. }) {
                continue;
            }
            registry
                .register(default_connector_for_provider(&provider, clock.clone()))
                .expect("default connector registration should not fail");
        }
        registry
    }

    pub fn register(&mut self, connector: Box<dyn Connector>) -> Result<(), ConnectorError> {
        let provider = connector.provider_id().clone();
        if self.connectors.contains_key(&provider) {
            return Err(ConnectorError::DuplicateProvider(provider.0));
        }
        self.connectors
            .insert(provider, Arc::new(AsyncMutex::new(connector)));
        Ok(())
    }

    /// Record the credentials `provider`'s manifest declared, so dispatched
    /// calls carry them without the caller ever reading a secret value.
    ///
    /// Independent of registration order: a declaration survives the
    /// remove-then-register cycle a host uses to override a connector, and one
    /// for a provider that is never registered is simply never consulted.
    pub fn declare_secrets(&mut self, provider: ProviderId, secrets: Vec<SecretId>) {
        if secrets.is_empty() {
            self.declared_secrets.remove(&provider);
        } else {
            self.declared_secrets.insert(provider, secrets);
        }
    }

    pub fn get(&self, id: &ProviderId) -> Option<ConnectorHandle> {
        self.connectors.get(id).cloned()
    }

    pub fn remove(&mut self, id: &ProviderId) -> Option<ConnectorHandle> {
        self.connectors.remove(id)
    }

    pub fn list(&self) -> Vec<ProviderId> {
        self.connectors.keys().cloned().collect()
    }

    /// Bind the store that [`ConnectorRegistry::client_map`] resolves declared
    /// credentials from.
    ///
    /// [`ConnectorRegistry::init_all`] does this for callers that initialize
    /// every connector at once; hosts that init connectors individually must
    /// bind the same store they built their [`ConnectorCtx`] with.
    pub fn bind_secret_store(&self, secrets: Arc<dyn SecretProvider>) {
        *self.secrets.lock().expect("registry secrets poisoned") = Some(secrets);
    }

    pub async fn init_all(&self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        self.bind_secret_store(Arc::clone(&ctx.secrets));
        for connector in self.connectors.values() {
            connector.lock().await.init(ctx.clone()).await?;
        }
        Ok(())
    }

    /// Dispatch clients for every registered connector.
    ///
    /// A connector that declared credentials gets a client that resolves them
    /// from the store this registry was initialized with, so `status`-usable
    /// and agent-usable are the same fact.
    pub async fn client_map(&self) -> BTreeMap<ProviderId, Arc<dyn ConnectorClient>> {
        let secrets = self
            .secrets
            .lock()
            .expect("registry secrets poisoned")
            .clone();
        let mut clients = BTreeMap::new();
        for (provider, connector) in &self.connectors {
            let client = connector.lock().await.client();
            let declared = self
                .declared_secrets
                .get(provider)
                .map(Vec::as_slice)
                .unwrap_or_default();
            clients.insert(
                provider.clone(),
                with_declared_secrets(client, declared, secrets.as_ref()),
            );
        }
        clients
    }

    pub async fn activate_all(
        &self,
        registry: &TriggerRegistry,
    ) -> Result<Vec<ActivationHandle>, ConnectorError> {
        let mut handles = Vec::new();
        for (provider, connector) in &self.connectors {
            let bindings = registry.bindings_for(provider);
            if bindings.is_empty() {
                continue;
            }
            let connector = connector.lock().await;
            handles.push(connector.activate(bindings).await?);
        }
        Ok(handles)
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use serde_json::{json, Value as JsonValue};

    use super::super::secret_injection::declared_secret_ids;
    use super::super::{
        ClientError, ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerEvent, TriggerKind,
    };
    use crate::secrets::MemorySecretProvider;

    /// A connector whose client records the args it was dispatched with, so a
    /// test can assert on what the connector would actually have seen.
    struct RecordingConnector {
        provider_id: ProviderId,
        seen: Arc<StdMutex<Vec<JsonValue>>>,
    }

    struct RecordingClient {
        seen: Arc<StdMutex<Vec<JsonValue>>>,
    }

    #[async_trait]
    impl ConnectorClient for RecordingClient {
        async fn call(&self, _method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
            self.seen.lock().expect("seen poisoned").push(args.clone());
            Ok(args)
        }
    }

    #[async_trait]
    impl Connector for RecordingConnector {
        fn provider_id(&self) -> &ProviderId {
            &self.provider_id
        }

        fn kinds(&self) -> &[TriggerKind] {
            &[]
        }

        async fn init(&mut self, _ctx: ConnectorCtx) -> Result<(), ConnectorError> {
            Ok(())
        }

        async fn activate(
            &self,
            bindings: &[TriggerBinding],
        ) -> Result<ActivationHandle, ConnectorError> {
            Ok(ActivationHandle::new(
                self.provider_id.clone(),
                bindings.len(),
            ))
        }

        async fn normalize_inbound(
            &self,
            _raw: RawInbound,
        ) -> Result<TriggerEvent, ConnectorError> {
            Err(ConnectorError::Unsupported("test connector".to_string()))
        }

        fn payload_schema(&self) -> ProviderPayloadSchema {
            ProviderPayloadSchema::named("test")
        }

        fn client(&self) -> Arc<dyn ConnectorClient> {
            Arc::new(RecordingClient {
                seen: Arc::clone(&self.seen),
            })
        }
    }

    /// Regression for harn#6706: a connector operation dispatched with no
    /// credentials in `args` and none in the environment must still reach the
    /// connector authenticated, from the store alone.
    #[tokio::test]
    async fn dispatch_carries_declared_credentials_resolved_from_the_store() {
        let provider = ProviderId::from("gitlab".to_string());
        let seen = Arc::new(StdMutex::new(Vec::new()));

        let mut registry = ConnectorRegistry::empty();
        registry
            .register(Box::new(RecordingConnector {
                provider_id: provider.clone(),
                seen: Arc::clone(&seen),
            }))
            .expect("registration succeeds");
        registry.declare_secrets(
            provider.clone(),
            declared_secret_ids(["gitlab/access-token"]),
        );

        let mut store = MemorySecretProvider::new("test-store");
        store.insert(
            crate::secrets::parse_secret_id("gitlab/access-token").expect("id parses"),
            "stored-token",
        );
        registry.bind_secret_store(Arc::new(store));

        let clients = registry.client_map().await;
        clients
            .get(&provider)
            .expect("client for registered provider")
            .call("graphql", json!({"query": "{ me { id } }"}))
            .await
            .expect("dispatch succeeds");

        let seen = seen.lock().expect("seen poisoned");
        assert_eq!(
            seen[0].pointer("/secrets/access_token"),
            Some(&JsonValue::String("stored-token".to_string())),
            "the registry must inject the connector's declared credential at dispatch"
        );
    }

    /// Hosts override a manifest-declared connector by removing the default
    /// and registering their own. The declaration must survive that cycle
    /// regardless of which side of it the caller declared on.
    #[tokio::test]
    async fn a_declaration_survives_the_override_register_cycle() {
        let provider = ProviderId::from("gitlab".to_string());
        let seen = Arc::new(StdMutex::new(Vec::new()));

        let mut registry = ConnectorRegistry::empty();
        registry.declare_secrets(
            provider.clone(),
            declared_secret_ids(["gitlab/access-token"]),
        );
        registry.remove(&provider);
        registry
            .register(Box::new(RecordingConnector {
                provider_id: provider.clone(),
                seen: Arc::clone(&seen),
            }))
            .expect("registration succeeds");

        let mut store = MemorySecretProvider::new("test-store");
        store.insert(
            crate::secrets::parse_secret_id("gitlab/access-token").expect("id parses"),
            "stored-token",
        );
        registry.bind_secret_store(Arc::new(store));

        let clients = registry.client_map().await;
        clients
            .get(&provider)
            .expect("client for registered provider")
            .call("graphql", json!({}))
            .await
            .expect("dispatch succeeds");

        assert_eq!(
            seen.lock().expect("seen poisoned")[0].pointer("/secrets/access_token"),
            Some(&JsonValue::String("stored-token".to_string()))
        );
    }

    /// Without a bound store the registry cannot resolve anything, so dispatch
    /// must still work and simply carry no injected credential.
    #[tokio::test]
    async fn dispatch_without_a_bound_store_stays_bare() {
        let provider = ProviderId::from("gitlab".to_string());
        let seen = Arc::new(StdMutex::new(Vec::new()));

        let mut registry = ConnectorRegistry::empty();
        registry
            .register(Box::new(RecordingConnector {
                provider_id: provider.clone(),
                seen: Arc::clone(&seen),
            }))
            .expect("registration succeeds");
        registry.declare_secrets(
            provider.clone(),
            declared_secret_ids(["gitlab/access-token"]),
        );

        let clients = registry.client_map().await;
        clients
            .get(&provider)
            .expect("client for registered provider")
            .call("graphql", json!({}))
            .await
            .expect("dispatch succeeds");

        assert_eq!(
            seen.lock().expect("seen poisoned")[0].pointer("/secrets"),
            None
        );
    }
}
