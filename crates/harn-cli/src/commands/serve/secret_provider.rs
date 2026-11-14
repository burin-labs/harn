use std::sync::Arc;

use harn_serve::{DispatchError, VmConfigurator};
use harn_vm::secrets::SecretProvider;

struct ServerSecrets(Arc<dyn SecretProvider>);

impl VmConfigurator for ServerSecrets {
    fn configure(&self, vm: &mut harn_vm::Vm) -> Result<(), DispatchError> {
        vm.set_harness(crate::source_exec::default_harness_with_secret_provider(
            self.0.clone(),
        ));
        Ok(())
    }
}

pub(super) fn configurator() -> Result<Arc<dyn VmConfigurator>, String> {
    // Capture the launcher's closed store before HTTP workers leave its task-local scope.
    let harness = crate::source_exec::default_harness()?;
    let provider = harness
        .secret_provider()
        .cloned()
        .ok_or("server launch did not configure a secret provider")?;
    Ok(Arc::new(ServerSecrets(provider)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use harn_serve::{AuthRequest, CallArguments, CallRequest, DispatchCore, DispatchCoreConfig};
    use harn_vm::secrets::{
        with_active_secret_provider, with_parent_secret_handoff, MemorySecretProvider,
        ParentSecretHandoff, SecretId,
    };

    fn read_request() -> CallRequest {
        CallRequest {
            adapter: "site".into(),
            function: "read".into(),
            arguments: CallArguments::Named(BTreeMap::new()),
            auth: AuthRequest::default(),
            caller: "fixture".into(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        }
    }

    #[tokio::test]
    async fn server_dispatch_retains_its_own_closed_parent_store_after_launch() {
        harn_vm::reset_thread_local_state();
        crate::compiler_context::ensure_builtin_signatures_installed();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("server.harn");
        std::fs::write(
            &source,
            r#"
pub fn read(harness: Harness) -> {value: string, missing: bool} {
  const absent = try { harness.secrets.read("fixture/ungranted") }
  return {value: harness.secrets.read("fixture/key"), missing: is_err(absent)}
}
"#,
        )
        .unwrap();
        let mut servers = Vec::new();
        for value in ["server-alpha", "server-beta"] {
            let id = SecretId::new("fixture", "key");
            let store = MemorySecretProvider::new("fixture").with_secret(id.clone(), value);
            let handoff = ParentSecretHandoff::capture(&store, [id]).await.unwrap();
            let config = with_parent_secret_handoff(Some(handoff), async {
                super::super::dispatch_core_config_for_source(source.to_str().unwrap()).unwrap()
            })
            .await;
            servers.push((DispatchCore::new(config).unwrap(), value));
        }
        let ambient = Arc::new(
            MemorySecretProvider::new("ambient")
                .with_secret(SecretId::new("fixture", "key"), "ambient-key")
                .with_secret(SecretId::new("fixture", "ungranted"), "ambient-only"),
        );
        let unbound = DispatchCore::new(DispatchCoreConfig::for_script(&source)).unwrap();
        let mut control_config = DispatchCoreConfig::for_script(&source);
        control_config.vm_configurator = Arc::new(super::ServerSecrets(ambient.clone()));
        let control = DispatchCore::new(control_config).unwrap();
        with_active_secret_provider(Some(ambient), async {
            assert!(unbound.dispatch(read_request()).await.is_err());
            let control = control.dispatch(read_request()).await.unwrap();
            assert_eq!(
                control.value,
                serde_json::json!({"value": "ambient-key", "missing": false})
            );
            for (server, expected) in servers {
                let response = server.dispatch(read_request()).await.unwrap();
                assert_eq!(
                    response.value,
                    serde_json::json!({"value": expected, "missing": true})
                );
            }
        })
        .await;
        harn_vm::reset_thread_local_state();
    }
}
