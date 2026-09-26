//! A process-local decision backend for tests.
//!
//! It exists so the evaluator's own behavior can be measured without a
//! provider: what it refuses before dispatch, how many physical requests it
//! actually made, and whether a bad response becomes a typed refusal rather
//! than an answer. The request counter is the instrument the #8540 falsifiers
//! read, so it counts every call that reaches the backend, including ones that
//! then fail.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::backend::{
    DecisionBackend, DecisionRequest, DecisionTransportError, RawDecisionResponse,
};

/// What the mock does when it is called. A script of outcomes is consumed in
/// order; running past the end is a transport failure rather than a silent
/// repeat, so a test that expected one call and got two fails loudly.
pub type MockOutcome = Result<RawDecisionResponse, DecisionTransportError>;

#[derive(Default)]
pub struct MockDecisionBackend {
    scripted: Mutex<Vec<MockOutcome>>,
    requests: Arc<AtomicU32>,
    /// Every request the backend was handed, for tests that assert on what was
    /// sent rather than only on what came back.
    seen: Mutex<Vec<MockSeenRequest>>,
}

/// The parts of a request a test asserts on. The whole `DecisionRequest`
/// borrows, so it cannot be retained.
#[derive(Clone, Debug, PartialEq)]
pub struct MockSeenRequest {
    pub provider: String,
    pub model: String,
    pub question_ids: Vec<String>,
    pub effort: String,
    pub temperature: f64,
    pub state: serde_json::Value,
}

impl MockDecisionBackend {
    /// A backend that serves these outcomes in order.
    pub fn scripted(outcomes: Vec<MockOutcome>) -> Self {
        Self {
            scripted: Mutex::new(outcomes),
            requests: Arc::new(AtomicU32::new(0)),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Physical requests that reached this backend. A pre-dispatch refusal
    /// must leave this at zero, and that zero is only meaningful because the
    /// same counter reads non-zero when a dispatch does happen.
    pub fn request_count(&self) -> u32 {
        self.requests.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<MockSeenRequest> {
        self.seen.lock().expect("mock backend log").clone()
    }
}

#[async_trait::async_trait]
impl DecisionBackend for MockDecisionBackend {
    async fn evaluate(
        &self,
        request: DecisionRequest<'_>,
    ) -> Result<RawDecisionResponse, DecisionTransportError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("mock backend log")
            .push(MockSeenRequest {
                provider: request.provider.to_string(),
                model: request.model.to_string(),
                question_ids: request.questions.ids(),
                effort: request.effort.to_string(),
                temperature: request.temperature,
                state: request.state.clone(),
            });
        let mut scripted = self.scripted.lock().expect("mock backend script");
        if scripted.is_empty() {
            return Err(DecisionTransportError::TransportFailed {
                diagnostic: "mock decision backend was called more times than it was scripted for"
                    .into(),
            });
        }
        scripted.remove(0)
    }
}
