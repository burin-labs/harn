use std::sync::Arc;

use futures::channel::mpsc::UnboundedSender;
use serde_json::Value as JsonValue;
use tokio::sync::{mpsc, oneshot};

use super::types::{ConnectionState, McpOrchestratorService, RpcBridge, RpcRequest};

impl RpcBridge {
    pub(super) fn start(service: Arc<McpOrchestratorService>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<RpcRequest>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build MCP worker runtime");
            runtime.block_on(async move {
                while let Some(request) = rx.recv().await {
                    let mut session = request.session;
                    let progress_bus = request.progress_sender.map(|sender| {
                        harn_vm::mcp_progress::ProgressBus::new(Arc::new(move |message| {
                            let _ = sender.unbounded_send(message);
                        }))
                    });
                    let _bus_guard = harn_vm::mcp_progress::ActiveBusGuard::install(progress_bus);
                    let response = service.handle_request(&mut session, request.request).await;
                    let _ = request.response_tx.send((session, response));
                }
            });
        });
        Self { tx }
    }

    pub(super) async fn call(
        &self,
        session: ConnectionState,
        request: JsonValue,
    ) -> Result<(ConnectionState, JsonValue), String> {
        self.call_with_progress(session, request, None).await
    }

    pub(super) async fn call_with_progress(
        &self,
        session: ConnectionState,
        request: JsonValue,
        progress_sender: Option<UnboundedSender<JsonValue>>,
    ) -> Result<(ConnectionState, JsonValue), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RpcRequest {
                session,
                request,
                response_tx,
                progress_sender,
            })
            .map_err(|_| "MCP worker is not running".to_string())?;
        response_rx
            .await
            .map_err(|_| "MCP worker dropped the response channel".to_string())
    }
}
