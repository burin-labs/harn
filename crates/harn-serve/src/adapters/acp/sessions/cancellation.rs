//! One admission lifetime for queued, preparing, and executing prompts.
use super::PromptCancellation;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
struct CancellationState {
    prompts: Vec<Weak<PromptCancellation>>,
    cancelled: bool,
    closing: Weak<()>,
}

#[derive(Clone, Default)]
pub(in super::super) struct SessionCancellation {
    state: Arc<Mutex<CancellationState>>,
}

impl SessionCancellation {
    /// Running execution or live admission permits preemption until a close
    /// has been admitted. Cancellation cleanup retains the running behavior.
    pub(in super::super) fn can_preempt_live_control(&self, prompt_active: bool) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closing.strong_count() == 0
            && (prompt_active
                || state.prompts.iter().any(|prompt| {
                    prompt
                        .upgrade()
                        .is_some_and(|prompt| !prompt.cancelled.load(Ordering::SeqCst))
                }))
    }

    pub(in super::super) fn cancel(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let already_cancelled = state.cancelled;
        state.cancelled = true;
        state.prompts.retain(|prompt| {
            let Some(prompt) = prompt.upgrade() else {
                return false;
            };
            prompt.cancel();
            true
        });
        !already_cancelled
    }

    /// Every queued close holds the same lifetime through dispatch. If close
    /// fails or its request is dropped, normal admission resumes automatically.
    pub(in super::super) fn prepare_close(&self) -> Arc<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closing.upgrade().unwrap_or_else(|| {
            let admission = Arc::new(());
            state.closing = Arc::downgrade(&admission);
            admission
        })
    }

    /// The queued request owns its scope until dispatch, then execution owns
    /// it. A new prompt can follow cancel, but cannot pass a pending close.
    pub(in super::super) fn prepare_prompt(&self) -> Arc<PromptCancellation> {
        let prompt = Arc::new(PromptCancellation::default());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closing.strong_count() > 0 {
            prompt.cancel();
            return prompt;
        }
        state.prompts.retain(|prompt| prompt.strong_count() > 0);
        state.prompts.push(Arc::downgrade(&prompt));
        state.cancelled = false;
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pending_close_must_drop_before_prompt_admission_resumes() {
        let cancellation = SessionCancellation::default();
        let active = cancellation.prepare_prompt();
        let first_close = cancellation.prepare_close();
        let second_close = cancellation.prepare_close();
        cancellation.cancel();
        assert!(active.cancelled.load(Ordering::SeqCst));
        assert!(!cancellation.can_preempt_live_control(false));
        let blocked = cancellation.prepare_prompt();
        assert!(blocked.cancelled.load(Ordering::SeqCst));
        drop(first_close);
        let still_blocked = cancellation.prepare_prompt();
        assert!(still_blocked.cancelled.load(Ordering::SeqCst));
        drop(second_close);
        let resumed = cancellation.prepare_prompt();
        assert!(!resumed.cancelled.load(Ordering::SeqCst));
        assert!(cancellation.can_preempt_live_control(false));
        drop(resumed);
        assert!(!cancellation.can_preempt_live_control(false));
    }

    #[test]
    fn running_cancellation_cleanup_keeps_control_until_close_is_admitted() {
        let cancellation = SessionCancellation::default();
        let _prompt = cancellation.prepare_prompt();
        cancellation.cancel();
        assert!(!cancellation.can_preempt_live_control(false));
        assert!(cancellation.can_preempt_live_control(true));
        let _close = cancellation.prepare_close();
        assert!(!cancellation.can_preempt_live_control(true));
    }
}
