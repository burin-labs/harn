//! HITL answer delivery while the ordinary ACP dispatcher awaits a prompt.
use super::*;

pub(super) fn decode_hitl_response(
    output: &AcpOutput,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> Option<harn_vm::HitlHostResponse> {
    match serde_json::from_value(params.clone()) {
        Ok(response) => Some(response),
        Err(_) => {
            output.write_line(
                &harn_vm::jsonrpc::error_response(
                    id.clone(),
                    -32602,
                    "Invalid harn.hitl.respond params",
                )
                .to_string(),
            );
            None
        }
    }
}

pub(super) fn write_hitl_response(
    output: &AcpOutput,
    id: &serde_json::Value,
    result: Result<u64, String>,
) {
    let response = match result {
        Ok(_) => harn_vm::jsonrpc::response(id.clone(), serde_json::json!({"ok":true})),
        Err(error) => harn_vm::jsonrpc::error_response(id.clone(), -32000, &error),
    };
    output.write_line(&response.to_string());
}

impl ConcurrentSessionControls {
    pub(super) async fn preempt_active_hitl_response(
        &self,
        message: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if message.get("method").and_then(serde_json::Value::as_str) != Some("harn.hitl.respond") {
            return false;
        }
        let Some(id) = message.get("id") else {
            return false;
        };
        if self.reject_unauthenticated(id, output) {
            return true;
        }
        let params = message.get("params").unwrap_or(&serde_json::Value::Null);
        let session_id = params.get("sessionId").and_then(serde_json::Value::as_str);
        let Some(control) = self.active_control(session_id) else {
            return false;
        };
        let Some(response) = decode_hitl_response(output, id, params) else {
            return true;
        };
        let result = match control.prompt_event_log() {
            Some(log) => harn_vm::append_hitl_response_on(&log, response).await,
            None => Err("HITL event log is unavailable".into()),
        };
        write_hitl_response(output, id, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::event_log::{EventLog, Topic};

    #[tokio::test(flavor = "current_thread")]
    async fn live_hitl_response_uses_waitpoint_owner_and_preserves_authentication() {
        let owner = harn_vm::event_log::install_memory_for_current_thread(32);
        let request_id = "hitl_question_live_response_owner";
        let chunk = harn_vm::compile_source(
            "fn main(harness: Harness) { harness.runtime.waitpoint_create(\"hitl_question_live_response_owner\") }",
        ).unwrap();
        let mut vm = harn_vm::new_vm();
        harn_vm::register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await.unwrap();
        let waitpoint = Topic::new(format!("waitpoint.state.{request_id}")).unwrap();
        assert_eq!(
            owner
                .read_range(&waitpoint, None, 100)
                .await
                .unwrap()
                .last()
                .unwrap()
                .1
                .payload["status"],
            "open"
        );
        let decoy = harn_vm::event_log::install_memory_for_current_thread(32);
        let controls = ConcurrentSessionControls::new(false, serde_json::json!({}));
        let control = ConcurrentSessionControl::new();
        control.set_prompt_active(true);
        let binding = control
            .admit_and_bind_prompt(None, Arc::default(), || Ok(()))
            .unwrap();
        binding.set_event_log(owner.clone());
        controls.register("owning-session", control.clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);
        let request = serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"harn.hitl.respond",
            "params":{"sessionId":"owning-session", "request_id":request_id,
                "answer":"delivered", "reviewer":"test", "responded_at":"2026-10-11T00:00:00Z"}
        });
        assert!(
            controls
                .preempt_active_hitl_response(&request, &output)
                .await
        );
        let refused: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(refused["error"]["code"], ACP_AUTH_REQUIRED_CODE);
        let topic = Topic::new(harn_vm::HITL_QUESTIONS_TOPIC).unwrap();
        assert!(owner
            .read_range(&topic, None, 100)
            .await
            .unwrap()
            .is_empty());
        controls.mark_authenticated();
        let mut malformed = request.clone();
        malformed["params"]["request_id"] = serde_json::json!(17);
        malformed["params"]["approved"] = serde_json::json!("credential-canary");
        assert!(
            controls
                .preempt_active_hitl_response(&malformed, &output)
                .await
        );
        let invalid = rx.recv().await.unwrap();
        let error: serde_json::Value = serde_json::from_str(&invalid).unwrap();
        assert_eq!(error["error"]["code"], -32602);
        assert!(!invalid.contains("credential-canary"));
        let mut wrong_session = request.clone();
        wrong_session["params"]["sessionId"] = serde_json::json!("other-session");
        assert!(
            !controls
                .preempt_active_hitl_response(&wrong_session, &output)
                .await
        );
        assert!(owner
            .read_range(&topic, None, 100)
            .await
            .unwrap()
            .is_empty());
        assert!(
            controls
                .preempt_active_hitl_response(&request, &output)
                .await
        );
        let accepted: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(accepted["result"]["ok"], true);
        assert_eq!(owner.read_range(&topic, None, 100).await.unwrap().len(), 1);
        assert!(decoy
            .read_range(&topic, None, 100)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            owner
                .read_range(&waitpoint, None, 100)
                .await
                .unwrap()
                .last()
                .unwrap()
                .1
                .payload["status"],
            "completed"
        );
        control.set_prompt_active(false);
        assert!(control.prompt_event_log().is_none());
        assert!(
            !controls
                .preempt_active_hitl_response(&request, &output)
                .await
        );
        assert!(rx.try_recv().is_err());
        harn_vm::event_log::reset_active_event_log();
    }
}
