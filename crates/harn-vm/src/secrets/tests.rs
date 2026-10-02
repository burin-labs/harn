use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;

struct FakeProvider {
    namespace: String,
    result: Mutex<Vec<Result<SecretBytes, SecretError>>>,
}

impl FakeProvider {
    fn new(namespace: impl Into<String>, result: Vec<Result<SecretBytes, SecretError>>) -> Self {
        Self {
            namespace: namespace.into(),
            result: Mutex::new(result),
        }
    }
}

#[async_trait]
impl SecretProvider for FakeProvider {
    async fn get(&self, _id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.result
            .lock()
            .expect("fake provider poisoned")
            .remove(0)
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "put",
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "rotate",
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "list",
        })
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

#[test]
fn secret_bytes_debug_is_redacted() {
    let secret = SecretBytes::from("abcd");
    assert_eq!(format!("{secret:?}"), "SecretBytes { redacted: 4 bytes }");
}

#[test]
fn parse_secret_ref_accepts_namespace_name_and_version() {
    let id = parse_secret_ref("harn-secret://provider/anthropic-api-key@7")
        .expect("parse should succeed")
        .expect("secret ref should be detected");
    assert_eq!(id.namespace, "provider");
    assert_eq!(id.name, "anthropic-api-key");
    assert_eq!(id.version, SecretVersion::Exact(7));
}

#[test]
fn parse_secret_ref_ignores_non_refs_and_rejects_malformed_refs() {
    assert!(parse_secret_ref("plain-api-key")
        .expect("non-ref should be accepted")
        .is_none());
    assert!(parse_secret_ref("harn-secret://missing-name")
        .expect_err("missing slash should fail")
        .to_string()
        .contains("invalid secret reference"));
}

#[test]
fn parse_secret_id_accepts_canonical_and_ref_forms() {
    let canonical = parse_secret_id("google_workspace/access-token@2").expect("canonical id");
    assert_eq!(canonical.namespace, "google_workspace");
    assert_eq!(canonical.name, "access-token");
    assert_eq!(canonical.version, SecretVersion::Exact(2));

    let reference =
        parse_secret_id("harn-secret://google_workspace/refresh-token").expect("ref id");
    assert_eq!(reference, connector_refresh_token_id("google_workspace"));

    assert_eq!(
        connector_oauth_token_id("google_workspace").name,
        CONNECTOR_OAUTH_TOKEN_SECRET_NAME
    );
    assert_eq!(
        connector_access_token_id("google_workspace").name,
        CONNECTOR_ACCESS_TOKEN_SECRET_NAME
    );
}

#[test]
fn secret_bytes_zeroes_on_drop() {
    let probe = Arc::new(Mutex::new(None));
    let mut secret = SecretBytes::from("super-secret");
    secret.attach_drop_probe(probe.clone());
    drop(secret);

    let dropped = probe
        .lock()
        .expect("drop probe poisoned")
        .clone()
        .expect("probe should capture bytes");
    assert!(dropped.iter().all(|byte| *byte == 0));
}

#[tokio::test]
async fn chain_secret_provider_falls_through_to_next_hit() {
    let id = SecretId::new("harn.test", "api-key");
    let first = Arc::new(FakeProvider::new(
        "first",
        vec![Err(SecretError::NotFound {
            provider: "first".to_string(),
            id: id.clone(),
        })],
    ));
    let second = Arc::new(FakeProvider::new(
        "second",
        vec![Ok(SecretBytes::from("value"))],
    ));
    let chain = ChainSecretProvider::new("harn/test", vec![first, second]);

    let secret = chain.get(&id).await.expect("chain should resolve");
    let exposed = secret.with_exposed(|bytes| bytes.to_vec());
    assert_eq!(exposed, b"value");
}

#[tokio::test]
async fn chain_secret_provider_returns_all_errors_when_everything_fails() {
    let id = SecretId::new("harn.test", "missing");
    let first = Arc::new(FakeProvider::new(
        "first",
        vec![Err(SecretError::NotFound {
            provider: "first".to_string(),
            id: id.clone(),
        })],
    ));
    let second = Arc::new(FakeProvider::new(
        "second",
        vec![Err(SecretError::Backend {
            provider: "second".to_string(),
            message: "boom".to_string(),
        })],
    ));
    let chain = ChainSecretProvider::new("harn/test", vec![first, second]);

    let error = chain.get(&id).await.expect_err("chain should fail");
    match error {
        SecretError::All(errors) => {
            assert_eq!(errors.len(), 2);
            assert!(matches!(errors[0], SecretError::NotFound { .. }));
            assert!(matches!(errors[1], SecretError::Backend { .. }));
        }
        other => panic!("expected aggregated errors, got {other:?}"),
    }
}

#[tokio::test]
async fn chain_absence_is_typed_and_names_consulted_and_excluded_providers() {
    let id = SecretId::new("google_workspace", "oauth-token");
    let env = EnvSecretProvider::new("harn.test");
    // Derived rather than spelled out: the environment registry scan treats a
    // literal variable name in source as a registered runtime variable.
    let env_var = env.env_var_name(&id);
    let plan = SecretChainPlan::from_value(Some("env"));
    let chain =
        ChainSecretProvider::new("harn.test", vec![Arc::new(env)]).with_excluded(plan.excluded);

    let error = chain.get(&id).await.expect_err("nothing is stored");
    assert!(error.is_not_found());
    let SecretError::NotFoundInChain(absence) = &error else {
        panic!("expected a typed chain absence, got {error:?}");
    };
    assert_eq!(
        absence.consulted,
        vec![ConsultedSecretProvider {
            provider: "env".to_string(),
            locator: Some(env_var.clone()),
        }]
    );
    assert_eq!(
        absence.excluded,
        vec![ExcludedSecretProvider {
            provider: "keyring".to_string(),
            reason: SecretProviderExclusion::ChainOverride {
                variable: SECRET_PROVIDER_CHAIN_ENV.to_string(),
                value: "env".to_string(),
            },
        }]
    );
    assert_eq!(
        env_var,
        ["HARN", "SECRET", "GOOGLE", "WORKSPACE", "OAUTH", "TOKEN"].join("_")
    );
    assert_eq!(
        error.to_string(),
        format!(
            "secret 'google_workspace/oauth-token' not found in providers: \
             env ({env_var}); keyring disabled by HARN_SECRET_PROVIDERS=env"
        )
    );
}

#[tokio::test]
async fn chain_keeps_every_error_when_any_provider_failed_for_another_reason() {
    let id = SecretId::new("harn.test", "flaky");
    let chain = ChainSecretProvider::new(
        "harn.test",
        vec![
            Arc::new(FakeProvider::new(
                "first",
                vec![Err(SecretError::NotFound {
                    provider: "first".to_string(),
                    id: id.clone(),
                })],
            )),
            Arc::new(FakeProvider::new(
                "second",
                vec![Err(SecretError::Backend {
                    provider: "second".to_string(),
                    message: "locked".to_string(),
                })],
            )),
        ],
    );
    let error = chain.get(&id).await.expect_err("chain should fail");
    assert!(matches!(error, SecretError::All(_)));
    assert!(!error.is_not_found());
}

#[test]
fn chain_plan_reports_default_providers_an_override_leaves_out() {
    let default = SecretChainPlan::from_value(None);
    assert_eq!(default.providers, vec!["env", "keyring"]);
    assert!(default.excluded.is_empty());
    assert_eq!(default.display(), "env -> keyring");

    let explicit_default = SecretChainPlan::from_value(Some("env, keyring"));
    assert!(explicit_default.excluded.is_empty());

    let env_only = SecretChainPlan::from_value(Some("env"));
    assert_eq!(env_only.providers, vec!["env"]);
    assert_eq!(
        env_only.display(),
        "env; keyring disabled by HARN_SECRET_PROVIDERS=env"
    );

    let file_only = SecretChainPlan::from_value(Some("file"));
    let excluded = file_only
        .excluded
        .iter()
        .map(|entry| entry.provider.as_str())
        .collect::<Vec<_>>();
    assert_eq!(excluded, vec!["env", "keyring"]);
}

#[tokio::test]
async fn scoped_secret_access_denies_runtime_reserved_namespaces() {
    let chain = ChainSecretProvider::new(
        "harn/test",
        vec![Arc::new(FakeProvider::new("unused", Vec::new()))],
    );

    for namespace in ["provenance", "harn.provenance", "harn.provenance.agent"] {
        let id = SecretId::new(namespace, "harn-cli.ed25519.seed");
        let error = chain
            .read_scoped(SecretReadRequest {
                id: id.clone(),
                scope: SecretScope::custom("provenance", None),
                audit: SecretAuditContext::default(),
            })
            .await
            .expect_err("reserved namespace should be denied before backend access");
        match error {
            SecretError::AccessDenied {
                operation,
                id: denied_id,
                message,
            } => {
                assert_eq!(operation, "read");
                assert_eq!(denied_id, id);
                assert!(message.contains("reserved for Harn runtime provenance signing"));
            }
            other => panic!("expected access-denied error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn keyring_provider_round_trips_and_zeroes_on_drop() {
    let provider =
        KeyringSecretProvider::with_store("harn.test", keyring_core::mock::Store::new().unwrap());
    let id = SecretId::new("", format!("mock-{}", uuid::Uuid::now_v7()));
    provider
        .put(&id, SecretBytes::from("round-trip-secret"))
        .await
        .expect("mock keyring write should succeed");

    let probe = Arc::new(Mutex::new(None));
    let mut secret = provider
        .get(&id)
        .await
        .expect("mock keyring read should succeed");
    assert_eq!(
        secret.with_exposed(|bytes| bytes.to_vec()),
        b"round-trip-secret"
    );
    secret.attach_drop_probe(probe.clone());
    drop(secret);

    let dropped = probe
        .lock()
        .expect("drop probe poisoned")
        .clone()
        .expect("probe should capture bytes");
    assert!(dropped.iter().all(|byte| *byte == 0));

    provider
        .delete(&id)
        .await
        .expect("mock keyring delete should succeed");
}

fn unique_write_id() -> SecretId {
    SecretId::new(
        "harn_chain_write_probe",
        format!("v{}", uuid::Uuid::now_v7().simple()),
    )
}

fn write_request(id: &SecretId) -> SecretWriteRequest {
    SecretWriteRequest {
        id: id.clone(),
        scope: SecretScope::default(),
        value: SecretBytes::from("synthetic-only"),
        options: SecretWriteOptions::default(),
        audit: SecretAuditContext::default(),
    }
}

#[tokio::test]
async fn chain_writes_skip_the_environment_when_a_persistent_provider_exists() {
    let id = unique_write_id();
    let env = Arc::new(EnvSecretProvider::new("harn.test"));
    let env_var = env.env_var_name(&id);
    let durable = Arc::new(MemorySecretProvider::new("durable-store"));
    let chain = ChainSecretProvider::new("harn.test", vec![env, durable.clone()]);

    let receipt = chain
        .write_scoped(write_request(&id))
        .await
        .expect("the persistent provider accepts the write");

    assert_eq!(receipt.provider, "durable-store");
    assert!(
        std::env::var_os(&env_var).is_none(),
        "the write must not land only in this process's environment"
    );
    let stored = durable.get(&id).await.expect("value reached the store");
    assert_eq!(stored.with_exposed(<[u8]>::to_vec), b"synthetic-only");
}

#[tokio::test]
async fn chain_write_fails_rather_than_falling_back_to_the_environment() {
    let id = unique_write_id();
    let env = Arc::new(EnvSecretProvider::new("harn.test"));
    let env_var = env.env_var_name(&id);
    let failing = Arc::new(FakeProvider::new("unwritable", Vec::new()));
    let chain = ChainSecretProvider::new("harn.test", vec![env, failing]);

    let error = chain
        .write_scoped(write_request(&id))
        .await
        .expect_err("no persistent provider accepted the write");
    assert!(matches!(error, SecretError::All(_)), "{error:?}");
    assert!(std::env::var_os(&env_var).is_none());
}

#[tokio::test]
async fn env_only_chain_still_writes_to_the_environment_and_says_so() {
    let id = unique_write_id();
    let env = Arc::new(EnvSecretProvider::new("harn.test"));
    let env_var = env.env_var_name(&id);
    let chain = ChainSecretProvider::new("harn.test", vec![env]);

    let receipt = chain
        .write_scoped(write_request(&id))
        .await
        .expect("an env-only chain keeps its process-scoped write");
    assert_eq!(receipt.provider, "env");
    assert_eq!(std::env::var(&env_var).as_deref(), Ok("synthetic-only"));
}
