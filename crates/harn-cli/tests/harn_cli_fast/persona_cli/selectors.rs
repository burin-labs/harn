use super::*;
use std::collections::BTreeMap;

#[tokio::test(flavor = "current_thread")]
async fn scaffolded_connector_personas_select_opened_events_only() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for (template, kind, selector) in [
                ("deterministic-sweeper", "pull_request", "github.pull_request.opened"),
                ("hybrid-classify-then-act", "issues", "github.issues.opened"),
            ] {
                harn_vm::reset_thread_local_state();
                let temp = TempDir::new().unwrap();
                let scaffold = persona_scaffold::scaffold_persona_package(
                    "event_captain", template, &temp.path().join("personas"), false,
                )
                .await
                .unwrap();
                let manifest = scaffold.root.join("harn.toml");
                let inspected = persona::inspect_payload(Some(&manifest), "event_captain").unwrap();
                assert_eq!(inspected["triggers"], serde_json::json!([selector]));
                // Keep the generated manifest and entry coordinate, isolating
                // event selection from the template's model-driven workflow.
                let entry = scaffold.root.join("src/event_captain.harn");
                fs::write(&entry, "pub pipeline run(_harness: Harness, event) { return {executed: true, kind: event.kind} }\n").unwrap();
                let extensions = harn_cli::package::try_load_runtime_extensions(&entry).unwrap();
                let log = harn_vm::event_log::install_memory_for_current_thread(128);
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                harn_cli::package::install_manifest_triggers(&mut vm, &extensions).await.unwrap();
                assert_eq!(harn_vm::snapshot_trigger_bindings().len(), 1);
                let dispatcher = harn_vm::Dispatcher::with_event_log(vm, log);
                for action in ["opened", "closed"] {
                    let event = harn_vm::TriggerEvent::new(
                        harn_vm::ProviderId::from("github"), kind, None, action, None,
                        BTreeMap::new(),
                        harn_vm::ProviderPayload::Extension(harn_vm::ExtensionProviderPayload {
                            provider: "github".to_string(), schema_name: "GitHubEventPayload".to_string(),
                            raw: serde_json::json!({"action": action}),
                        }),
                        harn_vm::SignatureStatus::Verified,
                    );
                    let outcomes = dispatcher.dispatch_event(event).await.unwrap();
                    if action == "opened" {
                        assert_eq!(outcomes.len(), 1, "{template}");
                        assert_eq!(outcomes[0].status, harn_vm::DispatchStatus::Succeeded, "{template}: {outcomes:?}");
                        let receipt = outcomes[0].result.as_ref().unwrap();
                        assert_eq!(receipt["result"]["executed"], true);
                        assert_eq!(receipt["result"]["kind"], kind);
                    } else {
                        assert!(outcomes.is_empty(), "{template} must exclude closed events");
                    }
                }
            }
        })
        .await;
}
