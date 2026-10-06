//! Cancellation-safe ownership of one pending preparation's grouped approval.

use crate::bridge::HostBridge;

use super::*;

/// One exact preparation waiting for its host's grouped approval.
/// Dropping the wait retires this preparation, never a same-session successor.
#[must_use = "wait for approval or stop the pending preparation"]
pub struct PendingSessionApproval<'a, E> {
    owner: &'a PreparedSession<E>,
    bridge: &'a HostBridge,
    session_id: String,
    batch: ApprovalBatch,
    identity: Arc<Mutex<PendingPreparationState>>,
    receipt: RunAuthorityReceipt,
}

impl<E> PreparedSession<E> {
    /// Bind cleanup before creating the approval future, including an unpolled
    /// future. The bridge and batch must belong to the currently waiting session.
    pub fn pending_approval<'a>(
        &'a self,
        bridge: &'a HostBridge,
        session_id: &str,
        batch: &ApprovalBatch,
    ) -> Result<PendingSessionApproval<'a, E>, String> {
        if bridge.get_session_id() != session_id {
            return Err("pending approval bridge does not match the session".to_string());
        }
        let pending = self
            .pending
            .lock()
            .expect("prepared-session pending state poisoned");
        let preparation = pending
            .get(session_id)
            .filter(|preparation| preparation.batch == *batch)
            .ok_or_else(|| "pending approval does not match the waiting preparation".to_string())?;
        Ok(PendingSessionApproval {
            owner: self,
            bridge,
            session_id: session_id.to_string(),
            batch: batch.clone(),
            identity: preparation.identity.clone(),
            receipt: preparation.receipt.clone(),
        })
    }
}

impl<E> PendingSessionApproval<'_, E> {
    /// Reach the existing ACP parser, then consume only this exact preparation.
    pub async fn wait(self) -> Result<PreparedSessionUpdate, String> {
        match request_session_approval(self.bridge, &self.session_id, &self.batch).await {
            Ok(decision) => {
                Ok(self
                    .owner
                    .decide_bound(&self.session_id, decision, Some(&self.identity)))
            }
            Err(_) => {
                let stopped = self.bridge.is_cancelled();
                self.retire(stopped)?
                    .ok_or_else(|| "pending preparation was superseded".to_string())
            }
        }
    }

    /// Record an accepted Stop without inventing a rejected approval decision.
    pub fn stop(self) -> Result<PreparedSessionUpdate, String> {
        self.retire(true)?
            .ok_or_else(|| "pending preparation was superseded".to_string())
    }

    fn retire(&self, stopped: bool) -> Result<Option<PreparedSessionUpdate>, String> {
        let mut pending = self
            .owner
            .pending
            .lock()
            .expect("prepared-session pending state poisoned");
        let mut state = self
            .identity
            .lock()
            .expect("pending preparation state poisoned");
        if *state != PendingPreparationState::Waiting {
            return Ok(None);
        }
        let current = pending
            .get(&self.session_id)
            .is_some_and(|preparation| Arc::ptr_eq(&preparation.identity, &self.identity));
        if current {
            pending.remove(&self.session_id);
        }
        // A later preparation's Stop cannot classify an older lost wait.
        let stopped = stopped && current && self.bridge.get_session_id() == self.session_id;
        *state = PendingPreparationState::Retired;
        drop(state);
        drop(pending);
        let mut receipt = self.receipt.clone();
        receipt.stage = if stopped {
            AuthorityReceiptStage::Stopped
        } else {
            AuthorityReceiptStage::Terminal
        };
        receipt.status = if stopped {
            AuthorityReceiptStatus::Stopped
        } else {
            AuthorityReceiptStatus::Failed
        };
        receipt.observed_at_ms = (self.owner.run.now_ms)();
        receipt.unused = receipt.granted.clone();
        receipt.diagnostics.push(AuthorityDiagnostic {
            code: if stopped {
                "prepared_session_stop"
            } else {
                "prepared_session_approval_wait_lost"
            }
            .to_string(),
            message: if stopped {
                "prepared session stopped while waiting for approval"
            } else {
                "prepared session approval wait ended without a host decision"
            }
            .to_string(),
            requirement_fingerprint: None,
            actionable: "Prepare a new session before resuming.".to_string(),
        });
        self.owner.run.receipts.persist(&receipt)?;
        Ok(Some(if stopped {
            PreparedSessionUpdate::Stopped {
                session_id: self.session_id.clone(),
                receipt,
            }
        } else {
            PreparedSessionUpdate::Terminal {
                session_id: self.session_id.clone(),
                receipt,
            }
        }))
    }
}

impl<E> Drop for PendingSessionApproval<'_, E> {
    fn drop(&mut self) {
        if let Err(error) = self.retire(self.bridge.is_cancelled()) {
            tracing::error!(
                session_id = %self.session_id,
                code = "prepared_session_pending_terminal_persistence",
                error = %error,
                "Pending preparation terminal accounting failed"
            );
        }
    }
}
