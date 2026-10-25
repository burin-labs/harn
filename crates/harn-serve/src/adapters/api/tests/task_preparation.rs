use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct GateVmPreparation {
    gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    executions: AtomicUsize,
}

#[async_trait::async_trait(?Send)]
impl crate::adapters::acp::AcpRuntimeConfigurator for GateVmPreparation {
    async fn configure(&self, _vm: &mut harn_vm::Vm, _source: Option<&Path>) -> Result<(), String> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            assert!(entered.send(()).is_ok());
            release.await.unwrap();
        }
        Ok(())
    }

    fn configure_harness(&self, harness: harn_vm::Harness) -> harn_vm::Harness {
        self.executions.fetch_add(1, Ordering::SeqCst);
        harness
    }
}

#[tokio::test]
async fn canceled_task_preparation_cannot_enter_actual_vm_execution() {
    canceled_task_preparation(false).await;
}

#[tokio::test]
async fn canceled_inline_task_preparation_cannot_enter_actual_vm_execution() {
    canceled_task_preparation(true).await;
}

async fn canceled_task_preparation(inline: bool) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_preparation.harn")).unwrap();
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let runtime = Arc::new(GateVmPreparation {
        gate: Mutex::new(Some((entered_tx, release_rx))),
        executions: AtomicUsize::new(0),
    });
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    if inline {
        config.acp.pipeline = None;
    }
    config.workspace_root = root.path().into();
    config.default_session_mode = "code".into();
    config.acp.runtime_configurator = runtime.clone();
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let mut events = state.events_tx.subscribe();
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let route = format!("/v1/sessions/{session_id}/tasks");
    let input = if inline {
        "harness.fs.write_text(\"task-preparation.txt\", \"vm-executed\")"
    } else {
        "must not execute"
    };
    let response = post(&app, &route, json!({"input":input})).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    entered.await.unwrap();
    assert_eq!(
        runtime.executions.load(Ordering::SeqCst),
        0,
        "actual VM has not started at the preparation gate"
    );
    let canceled = post(&app, &format!("/v1/tasks/{task_id}/cancel"), json!({})).await;
    assert_eq!(canceled.status(), StatusCode::OK);
    assert_eq!(read_json(canceled).await["status"], "CANCELED");
    assert!(release.send(()).is_ok());
    // A normal read is queued behind the real prompt. Its response proves that
    // preparation and any execution finished, rather than racing a zero read.
    state.acp.call("session/list", json!({})).await.unwrap();
    let marker = root.path().join("task-preparation.txt");
    assert_eq!(
        runtime.executions.load(Ordering::SeqCst),
        0,
        "canceled task reached actual VM setup; script marker={:?}",
        std::fs::read_to_string(&marker).ok()
    );
    assert!(!marker.exists());

    let response = post(&app, &route, json!({"input":input})).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let positive_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    loop {
        let event = events.recv().await.unwrap();
        assert_ne!(event.event, "task.failed", "{}", event.payload);
        if event.event == "task.completed" && event.task_id.as_deref() == Some(positive_id.as_str())
        {
            break;
        }
    }
    assert_eq!(runtime.executions.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "vm-executed");
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task(&task_id)
            .unwrap()
            .status
            .as_str(),
        "CANCELED"
    );
}
