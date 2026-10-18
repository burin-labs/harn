//! The script-published MCP registry as a source for [`McpServer`].
//!
//! A script's handlers close over one VM, and a VM is `!Send`, so the
//! registry and its VM live on a local task; the server talks to them
//! through [`McpScriptRuntime`], which is `Send` and answers discovery from a
//! snapshot without waiting behind a running handler.
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use harn_vm::mcp_progress::ProgressContext;
use harn_vm::{McpScriptRegistry, McpScriptSurface, Vm};
use tokio::sync::broadcast;

use super::*;

type ScriptJob = Box<
    dyn for<'a> FnOnce(&'a McpScriptRegistry, &'a mut Vm) -> Pin<Box<dyn Future<Output = ()> + 'a>>
        + Send,
>;

/// Handle to a script registry running on a local task.
#[derive(Clone)]
pub struct McpScriptRuntime {
    jobs: mpsc::UnboundedSender<ScriptJob>,
    surface: Arc<RwLock<Arc<McpScriptSurface>>>,
    changes: broadcast::Sender<()>,
    list_changes: bool,
}

/// Replaces the registry a [`McpScriptRuntime`] serves. Local to the thread
/// that spawned the runtime because the replacement carries a VM.
pub struct McpScriptReloader<A> {
    replacements: mpsc::UnboundedSender<ScriptState<A>>,
}

/// Field order is drop order: handlers may retain connector-backed values,
/// so the VM goes before the adapter-owned anchor that keeps those alive.
struct ScriptState<A> {
    registry: McpScriptRegistry,
    vm: Vm,
    _anchor: A,
}

impl McpScriptRuntime {
    /// Serve `registry` with handlers running on `vm`. Must be called from
    /// within a [`tokio::task::LocalSet`]; `anchor` lives exactly as long as
    /// the VM whose handlers use it.
    pub fn spawn_local<A: 'static>(registry: McpScriptRegistry, vm: Vm, anchor: A) -> Self {
        Self::spawn(registry, vm, anchor, false).0
    }

    /// Like [`Self::spawn_local`], but advertises list changes and returns
    /// the reloader that swaps in a revised registry.
    pub fn spawn_local_reloadable<A: 'static>(
        registry: McpScriptRegistry,
        vm: Vm,
        anchor: A,
    ) -> (Self, McpScriptReloader<A>) {
        Self::spawn(registry, vm, anchor, true)
    }

    fn spawn<A: 'static>(
        registry: McpScriptRegistry,
        vm: Vm,
        anchor: A,
        list_changes: bool,
    ) -> (Self, McpScriptReloader<A>) {
        let (jobs, mut jobs_rx) = mpsc::unbounded_channel::<ScriptJob>();
        let (replacements, mut replacements_rx) = mpsc::unbounded_channel::<ScriptState<A>>();
        let surface = Arc::new(RwLock::new(Arc::new(registry.surface())));
        let (changes, _) = broadcast::channel(16);
        let runtime = Self {
            jobs,
            surface: surface.clone(),
            changes: changes.clone(),
            list_changes,
        };
        tokio::task::spawn_local(async move {
            let mut state = ScriptState {
                registry,
                vm,
                _anchor: anchor,
            };
            let mut reloads_open = true;
            loop {
                tokio::select! {
                    job = jobs_rx.recv() => {
                        let Some(job) = job else { break };
                        job(&state.registry, &mut state.vm).await;
                    }
                    replacement = replacements_rx.recv(), if reloads_open => {
                        let Some(replacement) = replacement else {
                            reloads_open = false;
                            continue;
                        };
                        *surface.write().expect("script surface poisoned") =
                            Arc::new(replacement.registry.surface());
                        drop(std::mem::replace(&mut state, replacement));
                        let _ = changes.send(());
                    }
                }
            }
        });
        (runtime, McpScriptReloader { replacements })
    }

    pub(super) fn surface(&self) -> Arc<McpScriptSurface> {
        self.surface
            .read()
            .expect("script surface poisoned")
            .clone()
    }

    pub(super) fn list_changes(&self) -> bool {
        self.list_changes
    }

    /// Fires after each applied reload.
    pub(super) fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }

    /// Run `job` against the registry and its VM, in arrival order.
    async fn run(
        &self,
        id: &JsonValue,
        job: impl for<'a> FnOnce(
                &'a McpScriptRegistry,
                &'a mut Vm,
            ) -> Pin<Box<dyn Future<Output = JsonValue> + 'a>>
            + Send
            + 'static,
    ) -> JsonValue {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let queued: ScriptJob = Box::new(move |registry, vm| {
            Box::pin(async move {
                let _ = response_tx.send(job(registry, vm).await);
            })
        });
        if self.jobs.send(queued).is_err() {
            return harn_vm::jsonrpc::error_response(
                id.clone(),
                -32603,
                "script MCP runtime is not running",
            );
        }
        response_rx.await.unwrap_or_else(|_| {
            harn_vm::jsonrpc::error_response(
                id.clone(),
                -32603,
                "script MCP runtime dropped the request",
            )
        })
    }

    pub(super) async fn call_tool(
        &self,
        id: JsonValue,
        params: JsonValue,
        progress: Option<ProgressContext>,
        as_task: bool,
    ) -> JsonValue {
        let request_id = id.clone();
        self.run(&request_id, move |registry, vm| {
            Box::pin(async move {
                registry
                    .call_tool(&id, &params, vm, progress, as_task)
                    .await
            })
        })
        .await
    }

    /// `None` when the registry would run the call; otherwise its refusal.
    pub(super) async fn admit_tool(&self, id: JsonValue, params: JsonValue) -> Option<JsonValue> {
        let request_id = id.clone();
        let verdict = self
            .run(&request_id, move |registry, _vm| {
                Box::pin(async move {
                    match registry.admit_tool(&id, &params) {
                        Ok(()) => JsonValue::Null,
                        Err(response) => response,
                    }
                })
            })
            .await;
        (!verdict.is_null()).then_some(verdict)
    }

    pub(super) async fn read_resource(&self, id: JsonValue, params: JsonValue) -> JsonValue {
        let request_id = id.clone();
        self.run(&request_id, move |registry, vm| {
            Box::pin(async move { registry.read_resource(&id, &params, vm).await })
        })
        .await
    }

    pub(super) async fn get_prompt(&self, id: JsonValue, params: JsonValue) -> JsonValue {
        let request_id = id.clone();
        self.run(&request_id, move |registry, vm| {
            Box::pin(async move { registry.get_prompt(&id, &params, vm).await })
        })
        .await
    }

    pub(super) async fn complete(&self, id: JsonValue, params: JsonValue) -> JsonValue {
        let request_id = id.clone();
        self.run(&request_id, move |registry, vm| {
            Box::pin(async move { registry.complete(&id, &params, vm).await })
        })
        .await
    }
}

impl<A> McpScriptReloader<A> {
    /// Swap in a fully validated replacement. Connected stdio clients that
    /// finished initialization receive the standard `list_changed`
    /// notifications. Returns `false` once the runtime has stopped.
    pub fn replace(&self, registry: McpScriptRegistry, vm: Vm, anchor: A) -> bool {
        self.replacements
            .send(ScriptState {
                registry,
                vm,
                _anchor: anchor,
            })
            .is_ok()
    }
}
