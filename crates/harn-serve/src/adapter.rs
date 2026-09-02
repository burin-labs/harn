use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::task::LocalSet;

use crate::{CallRequest, CallResponse, DispatchCore, DispatchError};

/// A `.harn` dispatch job handed to a [`DispatchRuntime`] worker thread.
struct DispatchJob {
    request: CallRequest,
    response_tx: oneshot::Sender<Result<CallResponse, DispatchError>>,
    _ordering: DispatchOrderingGuard,
    _worker: IdleWorkerLease,
    queued_at: Instant,
}

struct IdleWorkerLease {
    index: usize,
    idle_tx: mpsc::UnboundedSender<usize>,
}

impl Drop for IdleWorkerLease {
    fn drop(&mut self) {
        let _ = self.idle_tx.send(self.index);
    }
}

enum DispatchOrderingGuard {
    Concurrent { _guard: OwnedRwLockReadGuard<()> },
    Exclusive { _guard: OwnedRwLockWriteGuard<()> },
}

/// Bounded executor pool that runs [`DispatchCore::dispatch`] off the axum
/// worker pool.
///
/// `.harn` execution is `!Send` (the VM holds `Arc`-backed values), so it
/// cannot run on tokio's multi-threaded runtime directly. Each worker owns a
/// current-thread runtime and [`LocalSet`]. Only exports explicitly annotated
/// both read-only and idempotent may overlap; a fair read/write barrier keeps
/// unknown and mutating exports globally ordered with the reads around them.
/// Every invocation still receives isolated VM state from the core's prepared
/// generation.
pub(crate) struct DispatchRuntime {
    name: &'static str,
    workers: Vec<mpsc::Sender<DispatchJob>>,
    idle_tx: mpsc::UnboundedSender<usize>,
    idle_rx: Mutex<mpsc::UnboundedReceiver<usize>>,
    ordering: Arc<RwLock<()>>,
    core: Arc<DispatchCore>,
}

impl DispatchRuntime {
    /// Spawn the executor thread. `name` labels the runtime in panic and
    /// error messages (e.g. `"A2A"`, `"MCP"`).
    pub(crate) fn start(name: &'static str, core: Arc<DispatchCore>) -> Self {
        let worker_count = core.dispatch_worker_count();
        let mut workers = Vec::with_capacity(worker_count);
        let (idle_tx, idle_rx) = mpsc::unbounded_channel();
        for index in 0..worker_count {
            let (tx, mut rx) = mpsc::channel::<DispatchJob>(1);
            let worker_core = Arc::clone(&core);
            crate::vm_thread::spawn(format!("{name}-{index}"), move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap_or_else(|error| panic!("build dispatch runtime: {error}"));
                let local = LocalSet::new();
                local.block_on(&runtime, async move {
                    while let Some(job) = rx.recv().await {
                        let core = Arc::clone(&worker_core);
                        tokio::task::spawn_local(async move {
                            let queue_ms = job.queued_at.elapsed().as_millis() as u64;
                            let result = core.dispatch(job.request).await.map(|mut response| {
                                response.dispatch.queue_ms = Some(queue_ms);
                                response
                            });
                            let _ = job.response_tx.send(result);
                        })
                        .await
                        .expect("dispatch worker task panicked");
                    }
                });
            })
            .unwrap_or_else(|error| panic!("spawn {name} VM worker {index}: {error}"));
            workers.push(tx);
            idle_tx
                .send(index)
                .expect("dispatch idle-worker receiver is live");
        }
        Self {
            name,
            workers,
            idle_tx,
            idle_rx: Mutex::new(idle_rx),
            ordering: Arc::new(RwLock::new(())),
            core,
        }
    }

    async fn lease_worker(&self) -> Result<IdleWorkerLease, DispatchError> {
        let index =
            self.idle_rx.lock().await.recv().await.ok_or_else(|| {
                DispatchError::Execution(format!("{} executor stopped", self.name))
            })?;
        Ok(IdleWorkerLease {
            index,
            idle_tx: self.idle_tx.clone(),
        })
    }

    /// Dispatch one request on the executor thread and await its reply.
    pub(crate) async fn call(&self, request: CallRequest) -> Result<CallResponse, DispatchError> {
        let queued_at = Instant::now();
        let worker = self.lease_worker().await?;
        let ordering = if self.core.is_concurrent_dispatch(&request.function) {
            DispatchOrderingGuard::Concurrent {
                _guard: Arc::clone(&self.ordering).read_owned().await,
            }
        } else {
            DispatchOrderingGuard::Exclusive {
                _guard: Arc::clone(&self.ordering).write_owned().await,
            }
        };
        let (response_tx, response_rx) = oneshot::channel();
        let worker_index = worker.index;
        self.workers[worker_index]
            .send(DispatchJob {
                request,
                response_tx,
                _ordering: ordering,
                _worker: worker,
                queued_at,
            })
            .await
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
    use std::future::Future;
    use std::num::NonZeroUsize;

    use crate::auth::AuthRequest;
    use crate::{CallArguments, DispatchCore, DispatchCoreConfig, VmConfigurator};
    use harn_vm::{Vm, VmValue};
    use tokio::sync::Semaphore;

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
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        }
    }

    fn named_request(function: &str, name: &str, value: &str) -> CallRequest {
        let mut request = request(function);
        request.arguments = CallArguments::Named(BTreeMap::from([(
            name.to_string(),
            serde_json::json!(value),
        )]));
        request
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
        assert!(response.dispatch.queue_ms.is_some());
        assert!(response.dispatch.execution_ms.is_some());
    }

    #[tokio::test]
    async fn dispatch_runtime_surfaces_named_executor_when_thread_is_gone() {
        // A `DispatchRuntime` whose worker thread has exited (its channel
        // receiver dropped) must report the failure with its label so the
        // hosting adapter's error envelope is self-describing.
        let (tx, rx) = mpsc::channel::<DispatchJob>(1);
        drop(rx);
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(&script, "pub fn greet() { return nil }\n").expect("write script");
        let core = Arc::new(
            DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("dispatch core"),
        );
        let (idle_tx, idle_rx) = mpsc::unbounded_channel();
        idle_tx.send(0).expect("seed idle worker");
        let runtime = DispatchRuntime {
            name: "TEST",
            workers: vec![tx],
            idle_tx,
            idle_rx: Mutex::new(idle_rx),
            ordering: Arc::new(RwLock::new(())),
            core,
        };
        let error = runtime
            .call(request("greet"))
            .await
            .expect_err("no receiver");
        assert!(
            matches!(&error, DispatchError::Execution(message) if message.contains("TEST executor")),
            "expected named executor error, got: {error:?}"
        );
    }

    struct RendezvousConfigurator {
        entered: tokio::sync::mpsc::UnboundedSender<String>,
        release: Arc<Semaphore>,
    }

    impl VmConfigurator for RendezvousConfigurator {
        fn configure(&self, vm: &mut Vm) -> Result<(), DispatchError> {
            let entered = self.entered.clone();
            let release = Arc::clone(&self.release);
            vm.register_async_builtin("test_rendezvous", move |_ctx, args| {
                let entered = entered.clone();
                let release = Arc::clone(&release);
                let value = args.first().cloned().unwrap_or(VmValue::Nil);
                async move {
                    let _ = entered.send(value.display());
                    release
                        .acquire()
                        .await
                        .expect("release stays open")
                        .forget();
                    Ok(value)
                }
            });
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dispatch_runtime_overlaps_only_explicitly_safe_exports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
@annotations(readOnly: true, idempotent: true)
pub fn rendezvous(value: string) -> string {
  return test_rendezvous(value)
}
",
        )
        .expect("write script");
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let mut config = DispatchCoreConfig::for_script(&script);
        config.max_dispatch_workers = NonZeroUsize::new(2).expect("two workers");
        config.vm_configurator = Arc::new(RendezvousConfigurator {
            entered: entered_tx,
            release: Arc::clone(&release),
        });
        let runtime = Arc::new(DispatchRuntime::start(
            "TEST",
            Arc::new(DispatchCore::new(config).expect("core")),
        ));

        let mut first_request = request("rendezvous");
        first_request.arguments = CallArguments::Named(BTreeMap::from([(
            "value".to_string(),
            serde_json::json!("first"),
        )]));
        let mut second_request = request("rendezvous");
        second_request.arguments = CallArguments::Named(BTreeMap::from([(
            "value".to_string(),
            serde_json::json!("second"),
        )]));

        let first_runtime = Arc::clone(&runtime);
        let first = tokio::spawn(async move { first_runtime.call(first_request).await });
        let second_runtime = Arc::clone(&runtime);
        let second = tokio::spawn(async move { second_runtime.call(second_request).await });

        let entered_a = entered_rx.recv().await.expect("first call entered");
        let entered_b = entered_rx.recv().await.expect("second call entered");
        assert_ne!(entered_a, entered_b);
        release.add_permits(2);

        let first = first.await.expect("first task").expect("first response");
        let second = second.await.expect("second task").expect("second response");
        assert_eq!(first.value, serde_json::json!("first"));
        assert_eq!(second.value, serde_json::json!("second"));
    }

    #[tokio::test]
    async fn queued_exclusive_call_blocks_later_reads_until_it_finishes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
@annotations(readOnly: true, idempotent: true)
pub fn safe(value: string) -> string { return test_rendezvous(value) }

pub fn mutate(value: string) -> string { return test_rendezvous(value) }
",
        )
        .expect("write script");
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let mut config = DispatchCoreConfig::for_script(&script);
        config.max_dispatch_workers = NonZeroUsize::new(2).expect("two workers");
        config.vm_configurator = Arc::new(RendezvousConfigurator {
            entered: entered_tx,
            release: Arc::clone(&release),
        });
        let runtime = Arc::new(DispatchRuntime::start(
            "TEST",
            Arc::new(DispatchCore::new(config).expect("core")),
        ));
        let first_runtime = Arc::clone(&runtime);
        let first = tokio::spawn(async move {
            first_runtime
                .call(named_request("safe", "value", "first"))
                .await
        });
        assert_eq!(entered_rx.recv().await.as_deref(), Some("first"));

        // Reserve the remaining worker slot while the two calls are queued so
        // their admission order is structural rather than scheduler timing.
        let admission_hold = runtime.lease_worker().await.expect("worker available");

        let mutation_runtime = Arc::clone(&runtime);
        let mut mutation_call = Box::pin(async move {
            mutation_runtime
                .call(named_request("mutate", "value", "mutation"))
                .await
        });
        std::future::poll_fn(|context| match mutation_call.as_mut().poll(context) {
            std::task::Poll::Pending => std::task::Poll::Ready(()),
            std::task::Poll::Ready(_) => panic!("exclusive call completed while admission held"),
        })
        .await;
        let mutation = tokio::spawn(mutation_call);
        let later_runtime = Arc::clone(&runtime);
        let later = tokio::spawn(async move {
            later_runtime
                .call(named_request("safe", "value", "later"))
                .await
        });
        drop(admission_hold);

        release.add_permits(1);
        assert_eq!(entered_rx.recv().await.as_deref(), Some("mutation"));
        release.add_permits(1);
        assert_eq!(entered_rx.recv().await.as_deref(), Some("later"));
        release.add_permits(1);

        first.await.expect("first task").expect("first response");
        mutation
            .await
            .expect("mutation task")
            .expect("mutation response");
        later.await.expect("later task").expect("later response");
    }
}
