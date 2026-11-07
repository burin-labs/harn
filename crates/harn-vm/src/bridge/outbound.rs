//! One connection owns outgoing IDs, waiter lifetime, and transport shutdown.
use super::*;

pub struct OutboundRequests {
    pub(super) next_id: AtomicU64,
    pub(super) pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    pub(super) disconnected: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum RequestError {
    Cancelled,
    Disconnected,
    Closed,
    Timeout(Duration),
    Encoding(String),
    Transport(String),
}

impl Default for OutboundRequests {
    fn default() -> Self {
        Self::from_parts(Default::default(), 1)
    }
}

impl OutboundRequests {
    pub fn from_parts(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        start_id: u64,
    ) -> Self {
        Self {
            next_id: AtomicU64::new(start_id),
            pending,
            disconnected: Arc::default(),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn complete(&self, message: serde_json::Value) {
        if let Some(id) = message["id"].as_u64() {
            if let Some(sender) = self.pending.lock().await.remove(&id) {
                let _ = sender.send(message);
            }
        }
    }

    pub async fn close(&self) {
        let mut pending = self.pending.lock().await;
        self.disconnected.store(true, Ordering::SeqCst);
        pending.clear();
    }

    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    pub async fn call(
        &self,
        writer: HostBridgeWriter,
        method: &str,
        params: serde_json::Value,
        cancellation: Option<(&AtomicBool, &Notify)>,
        timeout: Duration,
        cancel_remote: bool,
    ) -> Result<serde_json::Value, RequestError> {
        if cancellation.is_some_and(|(flag, _)| flag.load(Ordering::SeqCst)) {
            return Err(RequestError::Cancelled);
        }
        let id = self.next_id();
        let request = crate::jsonrpc::request(id, method, params);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if self.disconnected.load(Ordering::SeqCst) {
                return Err(RequestError::Disconnected);
            }
            pending.insert(id, tx);
        }
        let registration = pending_call::PendingCallRegistration::new(self.pending.clone(), id);
        let line =
            serde_json::to_string(&request).map_err(|e| RequestError::Encoding(e.to_string()))?;
        writer(&line).map_err(RequestError::Transport)?;
        let _registration = if cancel_remote {
            registration.with_cancellation(Arc::new(move |request_id| {
                let notification = crate::jsonrpc::notification(
                    "$/cancel_request",
                    serde_json::json!({"requestId":request_id}),
                );
                if let Ok(line) = serde_json::to_string(&notification) {
                    let _ = writer(&line);
                }
            }))
        } else {
            registration
        };
        if cancellation.is_some_and(|(flag, _)| flag.load(Ordering::SeqCst)) {
            return Err(RequestError::Cancelled);
        }
        let cancelled = async {
            if let Some((flag, notify)) = cancellation {
                loop {
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    notified.await;
                }
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            biased;
            () = cancelled => Err(RequestError::Cancelled),
            result = rx => result.map_err(|_| RequestError::Closed),
            timeout = wait_for_bridge_call_timeout(method, &request["params"], timeout) => Err(RequestError::Timeout(timeout)),
        }
    }
}
