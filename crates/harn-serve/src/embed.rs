//! Embedding helpers for running the in-process ACP agent loop.
//!
//! The ACP channel server future is `!Send` — it owns a
//! [`tokio::task::LocalSet`] and `spawn_local`s onto it — so it cannot be
//! `tokio::spawn`ed onto a multi-thread runtime. The canonical way to embed it
//! is therefore: spawn a dedicated OS thread, build a current-thread tokio
//! runtime on that thread, and `block_on` the server future there, talking to
//! it over a pair of unbounded channels.
//!
//! Every in-process embedder (the orchestrator's ACP WebSocket hub, the API
//! adapter, and Burin Code's Rust TUI) re-implements that exact dance.
//! [`EmbeddedAgent`] packages it once: call [`EmbeddedAgent::spawn`], get back
//! the request sender, the response receiver, and an [`AcpChannelHandle`] for
//! graceful shutdown / readiness / termination, and let `Drop` join the worker
//! thread for you.

use std::thread::{self, JoinHandle};

use tokio::sync::mpsc;

use crate::adapters::acp::{
    run_acp_channel_server_with_existing_handle, AcpChannelHandle, AcpServerConfig,
};

/// Default name for the dedicated ACP worker thread.
const DEFAULT_THREAD_NAME: &str = "harn-acp-embed";

/// A running in-process ACP agent: a dedicated worker thread driving the
/// `!Send` channel-server future on its own current-thread tokio runtime.
///
/// Construct one with [`EmbeddedAgent::spawn`]. The embedder communicates over
/// the [request sender](Self::requests) (host -> agent JSON-RPC) and
/// [response receiver](Self::take_responses) (agent -> host JSON-RPC lines),
/// and steers/observes the worker through [`handle`](Self::handle).
///
/// Dropping the `EmbeddedAgent` requests a graceful shutdown and joins the
/// worker thread, so an embedder does not have to manage the `!Send` /
/// dedicated-thread lifecycle by hand. Call [`shutdown`](Self::shutdown)
/// (and optionally [`join`](Self::join)) for explicit, error-observable
/// teardown.
pub struct EmbeddedAgent {
    // `Option` so `into_parts` can hand over the *owning* sender (the worker
    // only ever sees the matching receiver). Dropping the last sender closes
    // the request channel and is the legacy EOF-style teardown.
    request_tx: Option<mpsc::UnboundedSender<serde_json::Value>>,
    response_rx: Option<mpsc::UnboundedReceiver<String>>,
    handle: AcpChannelHandle,
    thread: Option<JoinHandle<()>>,
}

impl EmbeddedAgent {
    /// Spawn an in-process ACP agent on a dedicated worker thread.
    ///
    /// The worker thread builds a current-thread tokio runtime (`enable_all`)
    /// and `block_on`s the channel server. The agent runs until the request
    /// sender is dropped, [`shutdown`](Self::shutdown) is called, or the
    /// `EmbeddedAgent` is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the worker thread or the worker
    /// thread cannot build its tokio runtime — both are unrecoverable
    /// process-level failures at embed time.
    pub fn spawn(config: AcpServerConfig) -> Self {
        Self::spawn_named(config, DEFAULT_THREAD_NAME)
    }

    /// [`spawn`](Self::spawn) with a caller-chosen worker thread name (useful
    /// when an embedder runs several agents and wants them distinguishable in
    /// stack traces and profilers).
    ///
    /// # Panics
    ///
    /// See [`spawn`](Self::spawn).
    pub fn spawn_named(config: AcpServerConfig, thread_name: impl Into<String>) -> Self {
        let (request_tx, request_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let (response_tx, response_rx) = mpsc::unbounded_channel::<String>();
        // The handle is `Send`, so we build it here and hand a clone to the
        // worker. The channel-server future is `!Send`, so it must be built and
        // driven entirely on the worker thread — never moved across the
        // boundary. `config`, the channels, and the handle clone are all
        // `Send`, so only those cross.
        let handle = AcpChannelHandle::default();
        let worker_handle = handle.clone();

        let thread = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("EmbeddedAgent: build current-thread tokio runtime");
                let server_future = run_acp_channel_server_with_existing_handle(
                    config,
                    request_rx,
                    response_tx,
                    worker_handle,
                );
                runtime.block_on(server_future);
            })
            .expect("EmbeddedAgent: spawn ACP worker thread");

        Self {
            request_tx: Some(request_tx),
            response_rx: Some(response_rx),
            handle,
            thread: Some(thread),
        }
    }

    /// A clonable sender for host -> agent JSON-RPC requests
    /// (`session/new`, `session/prompt`, `session/cancel`, …). Dropping every
    /// clone of this sender (and the [`EmbeddedAgent`], which holds the owning
    /// sender) stops the agent, mirroring stdin EOF.
    ///
    /// # Panics
    ///
    /// Panics if called after [`into_parts`](Self::into_parts) took the
    /// owning sender. (`into_parts` consumes `self`, so this only matters for
    /// internal use.)
    pub fn requests(&self) -> mpsc::UnboundedSender<serde_json::Value> {
        self.request_tx
            .as_ref()
            .expect("EmbeddedAgent request sender was taken by into_parts")
            .clone()
    }

    /// Take the agent -> host response receiver (JSON-RPC response and
    /// `session/update` notification lines). Returns `None` if already taken;
    /// the receiver is single-consumer.
    pub fn take_responses(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.response_rx.take()
    }

    /// The cross-thread control handle for readiness, shutdown, and
    /// termination signalling.
    pub fn handle(&self) -> &AcpChannelHandle {
        &self.handle
    }

    /// Consume the agent into its raw parts: the request sender, the response
    /// receiver, and the control handle.
    ///
    /// The worker [`JoinHandle`] is detached when you take the parts this way
    /// — the agent stops when the returned sender is dropped or
    /// [`AcpChannelHandle::shutdown`] is called, and the thread exits on its
    /// own. Use this when you want the bare channels and manage lifetime
    /// through the handle; keep the [`EmbeddedAgent`] if you want `Drop` to
    /// join the thread.
    ///
    /// Returns the response receiver as `None` if it was already taken with
    /// [`take_responses`](Self::take_responses).
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        mut self,
    ) -> (
        mpsc::UnboundedSender<serde_json::Value>,
        Option<mpsc::UnboundedReceiver<String>>,
        AcpChannelHandle,
    ) {
        // Move out the *owning* sender (not a clone) so the worker's
        // `request_rx` closes when the caller drops it — preserving the
        // EOF teardown path.
        let request_tx = self
            .request_tx
            .take()
            .expect("EmbeddedAgent request sender was already taken");
        let response_rx = self.response_rx.take();
        let handle = self.handle.clone();
        // Detach the worker thread; its lifetime is now owned by the handle
        // and the moved-out sender. Clearing `thread` makes the subsequent
        // `Drop` a no-op (it only shuts down / joins when a thread is owned),
        // so no `mem::forget`/leak is needed.
        self.thread.take();
        (request_tx, response_rx, handle)
    }

    /// Request a graceful shutdown of the agent. Idempotent. Does not block;
    /// pair with [`join`](Self::join) to wait for the worker thread to exit.
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// Request shutdown and join the worker thread, returning the thread's
    /// join result. Subsequent calls (and `Drop`) become no-ops.
    pub fn join(&mut self) -> thread::Result<()> {
        self.handle.shutdown();
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

impl Drop for EmbeddedAgent {
    fn drop(&mut self) {
        // Only own teardown when we still hold the worker thread. `into_parts`
        // detaches it (sets `thread` to `None`), handing lifetime control to
        // the returned sender + handle, so dropping the husk must be inert.
        if let Some(thread) = self.thread.take() {
            self.handle.shutdown();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    async fn recv_json(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
        let line = rx.recv().await.expect("ACP response channel closed");
        serde_json::from_str(&line).expect("valid JSON-RPC line")
    }

    #[test]
    fn embedded_agent_round_trips_session_new_and_shuts_down() {
        let mut agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let requests = agent.requests();
        let mut responses = agent.take_responses().expect("responses receiver");

        block_on(agent.handle().wait_ready());

        requests
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "session/new",
                "params": {"cwd": "."},
            }))
            .expect("send session/new");

        let created = block_on(recv_json(&mut responses));
        assert!(
            created["result"]["sessionId"].as_str().is_some(),
            "session/new should return a sessionId, got: {created}"
        );

        // Graceful shutdown stops the worker thread.
        agent.shutdown();
        assert!(agent.join().is_ok(), "worker thread should join cleanly");
        assert!(agent.handle().is_shutdown());
        assert!(agent.handle().is_terminated());
    }

    #[test]
    fn shutdown_handle_terminates_idle_agent() {
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let handle = agent.handle().clone();

        block_on(handle.wait_ready());
        assert!(!handle.is_terminated());

        // A shutdown trigger from a cloned handle (a different owner than the
        // EmbeddedAgent) must stop an otherwise-idle server loop.
        handle.shutdown();
        block_on(handle.wait_terminated());

        drop(agent); // Drop joins the worker; must not hang.
        assert!(handle.is_shutdown());
    }

    #[test]
    fn dropping_request_sender_terminates_agent() {
        // `into_parts` detaches the worker thread and hands over the *only*
        // request sender, so dropping it closes `request_rx`. That must stop
        // the router and the loop even without an explicit shutdown() — the
        // legacy EOF-style teardown that existing callers rely on.
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let (requests, _responses, handle) = agent.into_parts();

        block_on(handle.wait_ready());
        assert!(!handle.is_terminated());

        drop(requests);
        block_on(handle.wait_terminated());
        assert!(
            !handle.is_shutdown(),
            "EOF teardown must not set the shutdown flag"
        );
    }

    #[test]
    fn into_parts_detaches_thread_and_keeps_channels_live() {
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let (requests, responses, handle) = agent.into_parts();
        let mut responses = responses.expect("responses receiver");

        block_on(handle.wait_ready());

        requests
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "session/new",
                "params": {"cwd": "."},
            }))
            .expect("send session/new");
        let created = block_on(recv_json(&mut responses));
        assert!(created["result"]["sessionId"].as_str().is_some());

        handle.shutdown();
        block_on(handle.wait_terminated());
    }
}
