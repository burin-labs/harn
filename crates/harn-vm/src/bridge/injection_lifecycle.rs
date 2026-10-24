//! Delivery lifetime for messages in the existing session input queue.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserMessageDeliveryOwner {
    Session,
    Task(String),
}

impl HostBridgeInjectionState {
    /// Retire only undelivered inputs belonging to the completed task.
    /// Session-scoped messages and reminders continue across prompt replacement.
    pub async fn revoke_task_user_messages(&self, task_id: &str) -> Vec<QueuedUserMessage> {
        let mut state = self.inner.lock().await;
        let mut revoked = Vec::new();
        state.queue.retain(|injection| {
            if let QueuedTranscriptInjection::User(message) = injection {
                if matches!(&message.delivery_owner, UserMessageDeliveryOwner::Task(owner) if owner == task_id) {
                    revoked.push(message.clone());
                    return false;
                }
            }
            true
        });
        state
            .revoked_user_message_ids
            .extend(revoked.iter().map(|message| message.message_id.clone()));
        revoked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn task_cleanup_preserves_session_inputs_and_other_tasks() {
        let queue = HostBridgeInjectionState::new();
        let session_id = queue
            .push_pending_user_message(
                "session input".into(),
                serde_json::json!("session input"),
                "queue",
            )
            .await;
        let task_a = queue
            .push_pending_user_message_admitted(
                None,
                "task A".into(),
                serde_json::json!("task A"),
                QueuedUserMessageMode::FinishStep,
                |_| Ok::<_, ()>(UserMessageDeliveryOwner::Task("A".into())),
            )
            .await
            .unwrap();
        let task_b = queue
            .push_pending_user_message_admitted(
                None,
                "task B".into(),
                serde_json::json!("task B"),
                QueuedUserMessageMode::FinishStep,
                |_| Ok::<_, ()>(UserMessageDeliveryOwner::Task("B".into())),
            )
            .await
            .unwrap();
        let reminder = queued_session_remind_from_params(
            &serde_json::json!({"body":"session reminder","mode":"queue"}),
        )
        .unwrap();
        queue
            .inner
            .lock()
            .await
            .queue
            .push_back(QueuedTranscriptInjection::Reminder(reminder));
        let before = queue.pending_injections_json().await;
        assert_eq!(before["pendingCount"], 4);
        let revoked = queue.revoke_task_user_messages("A").await;
        assert_eq!(revoked.len(), 1);
        assert_eq!(revoked[0].message_id, task_a);
        assert_eq!(
            queue.revoke_pending_user_message(&task_a).await,
            PendingUserMessageMutationResult::AlreadyRevoked
        );
        assert!(queue.revoke_task_user_messages("A").await.is_empty());
        let after = queue.pending_injections_json().await;
        assert_eq!(after["pendingCount"], 3);
        assert_eq!(after["injections"][0]["messageId"], session_id);
        assert_eq!(after["injections"][1]["messageId"], task_b);
        assert_eq!(after["injections"][2]["kind"], "reminder");
        let result = queue
            .push_pending_user_message_admitted(
                None,
                "rejected".into(),
                serde_json::json!("rejected"),
                QueuedUserMessageMode::FinishStep,
                |_| Err::<UserMessageDeliveryOwner, _>("storage unavailable"),
            )
            .await;
        assert_eq!(result.unwrap_err(), "storage unavailable");
        assert_eq!(queue.pending_injections_json().await, after);
    }
}
