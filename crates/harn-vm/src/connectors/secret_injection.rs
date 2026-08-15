//! Resolving a connector's declared credentials at dispatch.
//!
//! Connector manifests declare `required_secrets` as `namespace/name` ids, and
//! every connector's outbound config reads `args.secrets.<name>` before it
//! falls back to anything else. So the runtime resolves those ids from the
//! secret store when a call dispatches and injects them under that key: the
//! caller never handles a secret value, and no host has to re-implement
//! credential resolution to make an authenticated connector call work.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::secrets::{SecretId, SecretProvider};

use super::{ClientError, ConnectorClient, ProviderId};

/// The `args` key connectors read resolved credentials from.
const SECRETS_ARG_KEY: &str = "secrets";

/// The `args.secrets` key a declared secret id is injected under.
///
/// Manifests name secrets `<connector>/<secret-name>`; connectors read them as
/// identifiers, so `gitlab/access-token` arrives as `secrets.access_token`.
fn arg_key_for(id: &SecretId) -> String {
    id.name.replace('-', "_")
}

/// A connector client that fills in the connector's declared credentials from
/// the secret store before forwarding the call.
struct SecretInjectingClient {
    inner: Arc<dyn ConnectorClient>,
    secrets: Arc<dyn SecretProvider>,
    declared: Vec<SecretId>,
}

impl SecretInjectingClient {
    /// Merge store-resolved credentials into `args.secrets`.
    ///
    /// An entry the caller supplied explicitly always wins, so a script can
    /// still override a stored credential. A declared secret that the store
    /// cannot produce is skipped rather than failing the call: manifests list
    /// inbound webhook secrets and outbound API credentials in the same
    /// `required_secrets`, so a single operation never needs all of them, and
    /// the connector reports precisely which credential its operation missed.
    async fn resolve_args(&self, args: JsonValue) -> JsonValue {
        let mut args = match args {
            JsonValue::Object(map) => map,
            JsonValue::Null => JsonMap::new(),
            // A non-object `args` is the connector's contract to reject; pass
            // it through untouched rather than reshaping it here.
            other => return other,
        };
        let mut secrets = match args.get(SECRETS_ARG_KEY) {
            Some(JsonValue::Object(existing)) => existing.clone(),
            _ => JsonMap::new(),
        };

        for id in &self.declared {
            let key = arg_key_for(id);
            if secrets.contains_key(&key) {
                continue;
            }
            let Ok(secret) = self.secrets.get(id).await else {
                continue;
            };
            let rendered =
                secret.with_exposed(|bytes| std::str::from_utf8(bytes).map(str::to_string));
            if let Ok(value) = rendered {
                secrets.insert(key, JsonValue::String(value));
            }
        }

        if !secrets.is_empty() {
            args.insert(SECRETS_ARG_KEY.to_string(), JsonValue::Object(secrets));
        }
        JsonValue::Object(args)
    }
}

#[async_trait]
impl ConnectorClient for SecretInjectingClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        let args = self.resolve_args(args).await;
        self.inner.call(method, args).await
    }
}

/// Wrap `client` so dispatched calls carry `declared`, resolved from `secrets`.
///
/// Returns `client` unchanged when the connector declared no secrets, so
/// connectors that need no credentials keep a bare dispatch path.
pub(super) fn with_declared_secrets(
    client: Arc<dyn ConnectorClient>,
    declared: &[SecretId],
    secrets: Option<&Arc<dyn SecretProvider>>,
) -> Arc<dyn ConnectorClient> {
    let (Some(secrets), false) = (secrets, declared.is_empty()) else {
        return client;
    };
    Arc::new(SecretInjectingClient {
        inner: client,
        secrets: Arc::clone(secrets),
        declared: declared.to_vec(),
    })
}

/// Parse the `namespace/name` secret ids a connector manifest declared.
///
/// Entries that are not in `namespace/name` form are dropped; `harn package
/// verify` is the gate that rejects them at authoring time.
pub fn declared_secret_ids<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<SecretId> {
    raw.into_iter()
        .filter_map(|entry| crate::secrets::parse_secret_id(entry).ok())
        .collect()
}

/// Declared credentials per provider, recorded when connectors are registered.
pub type DeclaredConnectorSecrets = BTreeMap<ProviderId, Vec<SecretId>>;

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::sync::Mutex;

    use crate::secrets::MemorySecretProvider;

    /// Records the args it was dispatched with so a test can assert on what
    /// the connector would actually have seen.
    struct RecordingClient {
        seen: Mutex<Vec<JsonValue>>,
    }

    #[async_trait]
    impl ConnectorClient for RecordingClient {
        async fn call(&self, _method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
            self.seen.lock().expect("seen poisoned").push(args.clone());
            Ok(args)
        }
    }

    fn store_with(entries: &[(&str, &str)]) -> Arc<dyn SecretProvider> {
        let mut provider = MemorySecretProvider::new("test-store");
        for (id, value) in entries {
            let id = crate::secrets::parse_secret_id(id).expect("test secret id parses");
            provider.insert(id, value);
        }
        Arc::new(provider)
    }

    #[tokio::test]
    async fn declared_secrets_reach_the_connector_without_args_or_env() {
        let store = store_with(&[("gitlab/access-token", "stored-token")]);
        let client = with_declared_secrets(
            Arc::new(RecordingClient {
                seen: Mutex::new(Vec::new()),
            }),
            &declared_secret_ids(["gitlab/access-token"]),
            Some(&store),
        );

        let seen = client
            .call("graphql", json!({"query": "{ me { id } }"}))
            .await
            .expect("dispatch succeeds");

        assert_eq!(
            seen.pointer("/secrets/access_token"),
            Some(&JsonValue::String("stored-token".to_string())),
            "the connector must receive its declared credential as args.secrets.access_token"
        );
    }

    #[tokio::test]
    async fn explicit_args_win_over_the_store() {
        let store = store_with(&[("gitlab/access-token", "stored-token")]);
        let client = with_declared_secrets(
            Arc::new(RecordingClient {
                seen: Mutex::new(Vec::new()),
            }),
            &declared_secret_ids(["gitlab/access-token"]),
            Some(&store),
        );

        let seen = client
            .call(
                "graphql",
                json!({"secrets": {"access_token": "caller-token"}}),
            )
            .await
            .expect("dispatch succeeds");

        assert_eq!(
            seen.pointer("/secrets/access_token"),
            Some(&JsonValue::String("caller-token".to_string())),
            "an explicitly passed credential must override the stored one"
        );
    }

    #[tokio::test]
    async fn a_declared_secret_the_store_lacks_does_not_fail_dispatch() {
        // Manifests list inbound webhook secrets beside outbound credentials,
        // so an absent one must not block an operation that does not need it.
        let store = store_with(&[("gitlab/access-token", "stored-token")]);
        let client = with_declared_secrets(
            Arc::new(RecordingClient {
                seen: Mutex::new(Vec::new()),
            }),
            &declared_secret_ids(["gitlab/access-token", "gitlab/webhook-secret"]),
            Some(&store),
        );

        let seen = client
            .call("graphql", JsonValue::Null)
            .await
            .expect("dispatch succeeds despite the missing declared secret");

        assert_eq!(
            seen.pointer("/secrets/access_token"),
            Some(&JsonValue::String("stored-token".to_string()))
        );
        assert_eq!(seen.pointer("/secrets/webhook_secret"), None);
    }

    #[test]
    fn a_connector_declaring_no_secrets_keeps_a_bare_client() {
        let bare: Arc<dyn ConnectorClient> = Arc::new(RecordingClient {
            seen: Mutex::new(Vec::new()),
        });
        let wrapped = with_declared_secrets(Arc::clone(&bare), &[], None);
        assert!(Arc::ptr_eq(&bare, &wrapped));
    }

    #[test]
    fn manifest_secret_names_become_connector_arg_keys() {
        let ids = declared_secret_ids(["gitlab/access-token", "circleci/api-token", "malformed"]);
        let keys: Vec<String> = ids.iter().map(arg_key_for).collect();
        assert_eq!(keys, vec!["access_token", "api_token"]);
    }
}
