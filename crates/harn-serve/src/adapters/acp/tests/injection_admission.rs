use super::*;

struct InputAdmission {
    accepted: Mutex<Vec<harn_session_store::ControlEvent>>,
    fail: std::sync::atomic::AtomicBool,
}

impl SessionTaskAdmission for InputAdmission {
    fn admit_input(
        &self,
        _session_id: &str,
        control: &harn_session_store::ControlEvent,
        _task: Option<&TaskInjectionTarget>,
    ) -> Result<serde_json::Value, TaskAdmissionError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(TaskAdmissionError::StorageUnavailable);
        }
        self.accepted.lock().unwrap().push(control.clone());
        Ok(serde_json::json!({"id":control.message_id}))
    }

    fn admit_prompt(
        &self,
        _session_id: &str,
        _task: &TaskPromptTarget,
    ) -> Result<(), TaskAdmissionError> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn normal_session_inject_admits_before_queue_with_same_message_identity() {
    let admission = Arc::new(InputAdmission {
        accepted: Mutex::new(Vec::new()),
        fail: std::sync::atomic::AtomicBool::new(true),
    });
    let mut config = AcpServerConfig::new(None);
    config.task_admission = Some(admission.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"session/new",
            "params":{"cwd":".","environmentPolicy":{"kind":"isolated","grants":[]}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"].as_str().unwrap();
    let bridge = attach_test_host_bridge(&mut server, session_id);
    let input = serde_json::json!({
        "jsonrpc":"2.0","id":2,"method":"session/inject",
        "params":{"sessionId":session_id,"mode":"queue","content":"owned input"},
    });
    server.handle_incoming_message(input.clone()).await;
    let rejected = recv_json(&mut rx).await;
    assert_eq!(
        rejected["error"]["code"],
        TASK_ADMISSION_STORAGE_UNAVAILABLE_RPC_CODE
    );
    assert!(bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::EndOfInteraction)
        .await
        .is_empty());
    assert!(admission.accepted.lock().unwrap().is_empty());

    admission.fail.store(false, Ordering::SeqCst);
    server.handle_incoming_message(input).await;
    let accepted = recv_json(&mut rx).await;
    let queued = bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::EndOfInteraction)
        .await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].content, "owned input");
    let controls = admission.accepted.lock().unwrap();
    assert_eq!(controls.len(), 1);
    assert_eq!(
        controls[0].message_id.as_deref(),
        Some(queued[0].message_id.as_str())
    );
    assert_eq!(accepted["result"]["messageId"], queued[0].message_id);
    assert_eq!(
        accepted["result"]["_meta"]["harn"]["taskMessage"]["id"],
        queued[0].message_id
    );
}
