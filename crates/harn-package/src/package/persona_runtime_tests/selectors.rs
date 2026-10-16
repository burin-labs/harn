use super::*;
use harn_vm::event_log::{EventLog, Topic};
use harn_vm::Connector;
use std::collections::BTreeMap;
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn normalized_connector_events_select_and_run_manifest_personas() {
    tokio::task::LocalSet::new()
        .run_until(async {
            harn_vm::reset_thread_local_state();
            let tmp = tempfile::tempdir().unwrap();
            let anchor = write_trigger_project(
                tmp.path(),
                r#"
[[personas]]
name = "reviewer"
description = "Reviews opened pull requests."
entry_workflow = "workflow.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pull_request.opened"]
"#,
                None,
            );
            fs::write(
                tmp.path().join("workflow.harn"),
                "pub pipeline run(_harness: Harness, event) { return {executed: true, kind: event.kind} }\n",
            )
            .unwrap();
            let extensions = load_runtime_extensions(&anchor);
            let specs = collect_persona_trigger_binding_specs(&extensions).unwrap();
            assert_eq!(specs.len(), 1);
            let log = harn_vm::event_log::install_memory_for_current_thread(128);
            harn_vm::install_manifest_triggers(specs).await.unwrap();
            let metrics = Arc::new(harn_vm::MetricsRegistry::default());
            let connector_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../conformance/fixtures/personas/event-selector/connector.harn");
            let mut connector = harn_vm::HarnConnector::load(&connector_path).await.unwrap();
            connector
                .init(harn_vm::ConnectorCtx {
                    inbox: Arc::new(harn_vm::InboxIndex::new(log.clone(), metrics.clone()).await.unwrap()),
                    event_log: log.clone(),
                    secrets: Arc::new(harn_vm::secrets::MemorySecretProvider::new("selector-test")),
                    metrics,
                    rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
                })
                .await
                .unwrap();
            let dispatcher = harn_vm::Dispatcher::with_event_log(test_vm(), log.clone());
            for (delivery, action, selected) in [("opened", "opened", true), ("closed", "closed", false)] {
                let raw = harn_vm::RawInbound::new(
                    "",
                    BTreeMap::from([
                        ("X-GitHub-Event".to_string(), "pull_request".to_string()),
                        ("X-GitHub-Delivery".to_string(), delivery.to_string()),
                    ]),
                    serde_json::to_vec(&serde_json::json!({"action": action})).unwrap(),
                );
                let event = connector.normalize_inbound(raw).await.unwrap();
                assert_eq!(event.kind, "pull_request");
                let outcomes = dispatcher.dispatch_event(event).await.unwrap();
                if selected {
                    assert_eq!(outcomes.len(), 1);
                    assert_eq!(outcomes[0].status, harn_vm::DispatchStatus::Succeeded);
                    let receipt = outcomes[0].result.as_ref().unwrap();
                    assert_eq!(receipt["result"]["executed"], true);
                    assert_eq!(receipt["result"]["kind"], "pull_request");
                } else {
                    assert!(outcomes.is_empty(), "wrong action must not run the persona");
                }
            }
            let lifecycle = log
                .read_range(&Topic::new("persona.runtime.events").unwrap(), None, 128)
                .await
                .unwrap();
            assert_eq!(
                lifecycle.iter().filter(|(_, event)| event.kind == "persona.run.completed").count(),
                1,
            );
            connector.shutdown(std::time::Duration::ZERO).await.unwrap();
        })
        .await;
}
