//! Resolving a connector's declared credentials at dispatch.
//!
//! Connector manifests declare `required_secrets` as `namespace/name` ids, and
//! every connector's outbound config reads `args.secrets.<name>` before it
//! falls back to anything else. So the runtime resolves those ids from the
//! execution's secret provider when a call dispatches, falling back to the
//! registry's store only when no execution provider was supplied. Hosts can
//! stage credentials before execution without retaining a durable-store path
//! during the turn. The caller never handles a secret value.

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
    secrets: Option<Arc<dyn SecretProvider>>,
    declared: Vec<SecretId>,
}

impl SecretInjectingClient {
    /// Merge store-resolved credentials into `args.secrets`.
    ///
    /// An entry the caller supplied explicitly always wins, so a script can
    /// still override a stored credential. A declared credential that the
    /// store cannot produce is skipped rather than failing the call, and the
    /// connector reports precisely which credential its operation missed.
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
        let provider = crate::secrets::active_secret_provider().or_else(|| self.secrets.clone());

        for id in &self.declared {
            let key = arg_key_for(id);
            if secrets.contains_key(&key) {
                continue;
            }
            let Some(provider) = &provider else { continue };
            let Ok(secret) = provider.get(id).await else {
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
    if declared.is_empty() {
        return client;
    }
    Arc::new(SecretInjectingClient {
        inner: client,
        secrets: secrets.cloned(),
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::NativeVmExt;

    use crate::secrets::{
        with_active_secret_provider, MemorySecretProvider, RotationHandle, SecretBytes,
        SecretError, SecretMeta,
    };

    struct CountingStore {
        memory: MemorySecretProvider,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl SecretProvider for CountingStore {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.memory.get(id).await
        }

        async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
            self.memory.put(id, value).await
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            self.memory.rotate(id).await
        }

        async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            self.memory.list(prefix).await
        }

        fn namespace(&self) -> &str {
            self.memory.namespace()
        }

        fn supports_versions(&self) -> bool {
            self.memory.supports_versions()
        }
    }

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
    async fn execution_snapshot_keeps_repeated_dispatch_out_of_the_durable_store() {
        let declared = declared_secret_ids(["fixture/token", "fixture/missing"]);
        let durable = Arc::new(CountingStore {
            memory: MemorySecretProvider::empty()
                .with_secret(declared[0].clone(), "synthetic-startup-credential"),
            reads: AtomicUsize::new(0),
        });
        let mut staged = MemorySecretProvider::new("session");
        let value = durable
            .get(&declared[0])
            .await
            .expect("startup read reached");
        value.with_exposed(|bytes| staged.insert(declared[0].clone(), bytes));
        assert!(durable.get(&declared[1]).await.unwrap_err().is_not_found());
        assert_eq!(durable.reads.load(Ordering::SeqCst), 2);
        assert!(staged.contains(&declared[0]).await.unwrap());
        assert!(!staged.contains(&declared[1]).await.unwrap());

        // A credential appearing later cannot expand the execution's snapshot.
        durable
            .put(
                &declared[1],
                SecretBytes::from("synthetic-later-credential"),
            )
            .await
            .unwrap();
        let fallback: Arc<dyn SecretProvider> = durable.clone();
        let execution: Arc<dyn SecretProvider> = Arc::new(staged);
        let outer = store_with(&[("fixture/token", "synthetic-outer-credential")]);
        let chunk = crate::compile_source(
            r#"fn main(harness: Harness) {
                harness.net.connector_call("fixture", "read", {})
                harness.llm.catalog()
                harness.net.connector_call("fixture", "read", {})
            }"#,
        )
        .expect("connector script compiles");
        for default_store in [Some(&fallback), None] {
            let recording = Arc::new(RecordingClient {
                seen: Mutex::new(Vec::new()),
            });
            let client = with_declared_secrets(recording.clone(), &declared, default_store);
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            vm.set_harness(crate::Harness::real().with_secret_provider(execution.clone()));
            vm.set_connector_clients(crate::connectors::VmConnectorClients::new(
                BTreeMap::from([(ProviderId::from("fixture"), client)]),
                None,
            ));
            let llm_calls = Arc::new(AtomicUsize::new(0));
            let llm_calls_seen = llm_calls.clone();
            let llm_provider = execution.clone();
            vm.register_async_capability_method(
                harn_builtin_meta::CapabilityId::Llm,
                "catalog",
                move |_, _| {
                    let expected = llm_provider.clone();
                    let calls = llm_calls_seen.clone();
                    async move {
                        let active = crate::secrets::active_secret_provider()
                            .expect("LLM dispatch has its receiver's provider");
                        assert!(Arc::ptr_eq(&active, &expected));
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(crate::VmValue::Nil)
                    }
                },
            );
            with_active_secret_provider(Some(outer.clone()), async {
                tokio::task::LocalSet::new()
                    .run_until(vm.execute(&chunk))
                    .await
                    .expect("actual Harness dispatch succeeds");
                let restored = crate::secrets::active_secret_provider().unwrap();
                assert!(
                    Arc::ptr_eq(&restored, &outer),
                    "nested receiver restores its parent"
                );
            })
            .await;
            assert_eq!(llm_calls.load(Ordering::SeqCst), 1, "LLM control reached");
            let seen = recording.seen.lock().unwrap();
            assert_eq!(
                seen.len(),
                2,
                "both real connector calls reached the client"
            );
            for args in seen.iter() {
                assert_eq!(args["secrets"]["token"], "synthetic-startup-credential");
                assert!(args.pointer("/secrets/missing").is_none());
            }
            assert_eq!(durable.reads.load(Ordering::SeqCst), 2);
        }

        // Outside an execution override, the same default still supplies secrets.
        let client = with_declared_secrets(
            Arc::new(RecordingClient {
                seen: Mutex::new(Vec::new()),
            }),
            &declared,
            Some(&fallback),
        );
        let args = client.call("read", JsonValue::Null).await.unwrap();
        assert!(args.pointer("/secrets/missing").is_some());
        assert_eq!(durable.reads.load(Ordering::SeqCst), 4);
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
        // A package may declare several outbound credentials, while an
        // individual operation needs only a subset of them.
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
