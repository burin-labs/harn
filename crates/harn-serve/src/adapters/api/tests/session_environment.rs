use super::*;
use crate::adapters::acp::{AcpRuntimeConfigurator, AcpSessionEnvironmentConfig};
use harn_vm::secrets::{MemorySecretProvider, SecretId};
use harn_vm::security::{
    EnvironmentPolicyKind, GrantAudience, GrantSourceSpec, GrantSpec, LauncherEnvironment,
};

const CANARY: &str = "api-host-provider-memory-canary";

struct ProviderMemory(Arc<MemorySecretProvider>);

#[async_trait::async_trait(?Send)]
impl AcpRuntimeConfigurator for ProviderMemory {
    fn configure_harness(&self, harness: harn_vm::Harness) -> harn_vm::Harness {
        harness.with_secret_provider(self.0.clone())
    }
}

fn environment() -> AcpSessionEnvironmentConfig {
    AcpSessionEnvironmentConfig {
        kind: EnvironmentPolicyKind::Granted,
        grants: vec![GrantSpec {
            name: "provider-auth".into(),
            source: GrantSourceSpec::Env {
                var: "ANTHROPIC_API_KEY".into(),
            },
            expose_as_env: Some("ANTHROPIC_API_KEY".into()),
            for_command: None,
            expose_to: GrantAudience::InProcess,
        }],
    }
}

fn config(root: &Path) -> ApiServerConfig {
    let script = root.join("provider.harn");
    std::fs::write(
        &script,
        r#"pipeline main(harness: Harness) {
  const providers = harness.llm.available_providers()
  assert(len(providers) > 0, "provider-read-unmeasured")
  for provider in providers {
    if provider == "anthropic" {
      harness.stdio.println("credential-present")
      return "credential-present"
    }
  }
  assert(false, "credential-absent")
}
"#,
    )
    .unwrap();
    let mut memory = MemorySecretProvider::new("api-host");
    memory.insert(SecretId::new("api-host", "anthropic"), CANARY);
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy());
    config.workspace_root = root.into();
    config.acp.launcher_environment = LauncherEnvironment::from_snapshot(BTreeMap::from([(
        "ANTHROPIC_API_KEY".into(),
        "harn-secret://api-host/anthropic".into(),
    )]));
    config.acp.runtime_configurator = Arc::new(ProviderMemory(Arc::new(memory)));
    config
}

async fn session(app: &Router, body: Value) -> String {
    let response = post(app, "/v1/sessions", body).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    read_json(response).await["id"].as_str().unwrap().to_owned()
}

fn assert_no_secret(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_secret(&path);
        } else {
            assert!(
                !String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains(CANARY),
                "provider credential persisted in {}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn trusted_in_process_provider_grant_reaches_new_and_cold_restored_tasks() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path()).with_session_environment(environment());
    let state = ApiServer::new(config.clone()).state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);
    let id = session(&app, json!({"workspace_id":"local"})).await;
    let result = served_agent_turn::submit_and_wait(&app, &mut events, &id, "presence").await;
    assert_eq!(result.event, "task.completed", "{}", result.payload);
    let state = ApiServer::new(config).state;
    assert!(state.inner.lock().unwrap().sessions.is_empty());
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);
    let result =
        served_agent_turn::submit_and_wait(&app, &mut events, &id, "presence after restart").await;
    assert_eq!(result.event, "task.completed", "{}", result.payload);
    assert_no_secret(root.path());
}

#[tokio::test]
async fn default_api_isolation_cannot_be_widened_by_session_json() {
    let root = tempfile::tempdir().unwrap();
    let state = ApiServer::new(config(root.path())).state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);
    for body in [
        json!({"workspace_id":"local"}),
        json!({"workspace_id":"local",
        "environmentPolicy":{"kind":"inherited"},
        "_meta":{"harn":{"environmentPolicy":environment()}}}),
    ] {
        let id = session(&app, body).await;
        let result = served_agent_turn::submit_and_wait(&app, &mut events, &id, "presence").await;
        assert_eq!(result.event, "task.failed", "{}", result.payload);
        assert!(
            result.payload["failure"]["message"]
                .as_str()
                .unwrap()
                .contains("credential-absent"),
            "{}",
            result.payload
        );
    }
    assert_no_secret(root.path());
}

#[tokio::test]
async fn invalid_host_environment_is_refused_before_session_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut invalid = environment();
    invalid.kind = EnvironmentPolicyKind::Isolated;
    let state = ApiServer::new(config(root.path()).with_session_environment(invalid)).state;
    let app = api_router(state.clone());
    let response = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(read_json(response).await["error"]["code"], "acp_error");
    assert!(state.inner.lock().unwrap().sessions.is_empty());
    assert_no_secret(root.path());
}
