use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use crate::{CallRequest, CallResponse, DispatchCore, DispatchError};

/// A `.harn` dispatch job handed to a [`DispatchRuntime`] worker thread.
struct DispatchJob {
    request: CallRequest,
    response_tx: oneshot::Sender<Result<CallResponse, DispatchError>>,
}

/// Dedicated single-threaded executor that runs [`DispatchCore::dispatch`]
/// off the axum worker pool.
///
/// `.harn` execution is `!Send` (the VM holds `Rc`-backed values), so it
/// cannot run on tokio's multi-threaded runtime directly. Each
/// non-blocking HTTP transport (A2A, MCP) owns one of these: a thread
/// running a current-thread runtime with a [`LocalSet`], fed by an
/// unbounded channel. The transport handler `await`s the per-job
/// [`oneshot`] reply, so the axum task stays cooperative while the VM
/// runs on the dedicated thread.
pub(crate) struct DispatchRuntime {
    name: &'static str,
    tx: mpsc::UnboundedSender<DispatchJob>,
}

impl DispatchRuntime {
    /// Spawn the executor thread. `name` labels the runtime in panic and
    /// error messages (e.g. `"A2A"`, `"MCP"`).
    pub(crate) fn start(name: &'static str, core: Arc<DispatchCore>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DispatchJob>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|error| panic!("build {name} dispatch runtime: {error}"));
            let local = LocalSet::new();
            local.block_on(&runtime, async move {
                while let Some(job) = rx.recv().await {
                    let core = core.clone();
                    tokio::task::spawn_local(async move {
                        let result = core.dispatch(job.request).await;
                        let _ = job.response_tx.send(result);
                    });
                }
            });
        });
        Self { name, tx }
    }

    /// Dispatch one request on the executor thread and await its reply.
    pub(crate) async fn call(&self, request: CallRequest) -> Result<CallResponse, DispatchError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(DispatchJob {
                request,
                response_tx,
            })
            .map_err(|_| {
                DispatchError::Execution(format!("{} executor is not running", self.name))
            })?;
        response_rx.await.map_err(|_| {
            DispatchError::Execution(format!("{} executor dropped response", self.name))
        })?
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: String,
    pub caller_shape: String,
    pub supports_streaming: bool,
    pub supports_cancel: bool,
}

impl AdapterDescriptor {
    pub fn new(id: impl Into<String>, caller_shape: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            caller_shape: caller_shape.into(),
            supports_streaming: false,
            supports_cancel: true,
        }
    }
}

#[async_trait(?Send)]
pub trait TransportAdapter: Send + Sync {
    fn descriptor(&self) -> AdapterDescriptor;

    async fn dispatch(
        &self,
        core: &DispatchCore,
        request: CallRequest,
    ) -> Result<CallResponse, DispatchError> {
        core.dispatch(request).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::auth::AuthRequest;
    use crate::{CallArguments, DispatchCore, DispatchCoreConfig};

    use super::*;

    fn request(function: &str) -> CallRequest {
        CallRequest {
            adapter: "test".to_string(),
            function: function.to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "name".to_string(),
                serde_json::json!("ada"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            progress: None,
            tenant_id: None,
            request_id: None,
        }
    }

    #[tokio::test]
    async fn dispatch_runtime_round_trips_through_dedicated_thread() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            "pub fn greet(name: string) -> string {\n  return name\n}\n",
        )
        .expect("write script");

        let core =
            Arc::new(DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core"));
        let runtime = DispatchRuntime::start("TEST", core);
        let response = runtime.call(request("greet")).await.expect("dispatch");
        assert_eq!(response.value, serde_json::json!("ada"));
    }

    #[tokio::test]
    async fn dispatch_runtime_surfaces_named_executor_when_thread_is_gone() {
        // A `DispatchRuntime` whose worker thread has exited (its channel
        // receiver dropped) must report the failure with its label so the
        // hosting adapter's error envelope is self-describing.
        let (tx, rx) = mpsc::unbounded_channel::<DispatchJob>();
        drop(rx);
        let runtime = DispatchRuntime { name: "TEST", tx };
        let error = runtime
            .call(request("greet"))
            .await
            .expect_err("no receiver");
        assert!(
            matches!(&error, DispatchError::Execution(message) if message.contains("TEST executor")),
            "expected named executor error, got: {error:?}"
        );
    }
}
