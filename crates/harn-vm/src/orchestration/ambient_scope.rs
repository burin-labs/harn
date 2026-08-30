//! Per-task ambient execution scope.
//!
//! Capability/identity context — execution policy, approval policy, command
//! policy, dynamic permissions, the current host bridge, the bridge-trust +
//! command-hook depths, and the runtime-context overlay — is held in
//! thread-local LIFO stacks or single slots, including contexts installed for
//! the whole agent loop. That model is sound for a synchronous call stack, but
//! a guard held across an `.await` is **not**: tasks can interleave on one thread
//! or migrate between workers. A child would otherwise read a *sibling's* file
//! scope, env, worktree, tool ceiling, approval, secrets, or event attribution.
//!
//! [`AmbientExecutionScope`] gives every spawned worker its **own** copy of
//! these stacks. [`scope_ambient`] wraps the worker future so the task's scope
//! is swapped into the thread-locals on poll-enter and swapped back out on
//! poll-exit (the same technique `tracing::Instrument` uses for span context).
//! Only the currently-polling task's scope is ever live on a thread, so the
//! cooperative/work-stealing interleaving is invisible to capability checks.
//! Swaps are O(1) pointer moves regardless of stack depth.
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use super::approval_reviewer::{swap_approval_reviewer_depth, swap_approval_reviewer_stack};
use super::command_policy::{
    swap_command_policy_hook_depth, swap_command_policy_stack, CommandPolicy,
};
use super::policy::{
    swap_approval_policy_stack, swap_execution_policy_stack, swap_operator_approval_grant_stack,
    swap_trusted_bridge_depth, CapabilityPolicy, OperatorApprovalGrant, ToolApprovalPolicy,
};
use super::tool_precheck::{swap_tool_precheck_depth, swap_tool_precheck_stack};
use super::{swap_mutation_session, MutationSessionRecord, RunExecutionRecord};
use crate::agent_sessions::swap_current_session_stack;
use crate::autonomy::{swap_autonomy_policy_stack, AutonomyPolicy};
use crate::connectors::harn_module::swap_active_harn_connector_ctx;
use crate::connectors::ConnectorCtx;
use crate::egress::{swap_policy_context, EgressPolicyContext};
use crate::llm::agent_observe::{swap_llm_transcript_ambient, LlmTranscriptAmbient};
use crate::llm::capabilities::{
    swap_user_overrides as swap_capability_overrides, CapabilitiesFile,
};
use crate::llm::mock::{current_llm_mock_context, swap_llm_mock_context, LlmMockContext};
use crate::llm::permissions::{swap_dynamic_permission_stack, DynamicPermissionPolicy};
use crate::llm::{swap_current_host_bridge, swap_current_loop_sinks};
use crate::llm_config::{
    swap_runtime_provider_endpoint_overrides, swap_user_overrides as swap_provider_overrides,
    ProvidersConfig, RuntimeProviderEndpointOverrides,
};
use crate::run_events::{swap_run_event_sink, RunEventSink};
use crate::runtime_context::{swap_runtime_context_overlay_stack, RuntimeContextOverlay};
use crate::stdlib::host::process_admission::{
    swap_process_admission_context, ProcessAdmissionContext,
};
use crate::stdlib::process::{
    swap_session_environment, swap_source_dir, swap_thread_execution_context,
};
use crate::stdlib::template::llm_context::{swap_llm_render_stack, LlmRenderContextFrame};
pub(crate) mod blocking;
mod subtask_state;
mod top_level;

use subtask_state::SubtaskAmbientState;

/// An isolated snapshot of every ambient capability/identity stack a worker
/// task owns while it runs. `Default` is the empty scope (no policies, depth 0).
#[derive(Default, Clone)]
pub(crate) struct AmbientExecutionScope {
    execution: Vec<CapabilityPolicy>,
    approval: Vec<ToolApprovalPolicy>,
    operator_approval_grants: Vec<OperatorApprovalGrant>,
    command: Vec<CommandPolicy>,
    permissions: Vec<DynamicPermissionPolicy>,
    runtime_context: Vec<RuntimeContextOverlay>,
    autonomy: Vec<AutonomyPolicy>,
    /// Active `@step` and persona frames. Inline work inherits them; spawned
    /// workers start empty and establish their own frames.
    active_step_context: crate::step_runtime::ActiveContextSnapshot,
    active_context_suspensions: Vec<u64>,
    llm_render: Vec<LlmRenderContextFrame>,
    llm_transcript: LlmTranscriptAmbient,
    /// Inline fixtures and observations shared by one VM execution tree.
    llm_mock: LlmMockContext,
    connector_ctx: Vec<ConnectorCtx>,
    /// Outbound-network policy shared by one pipeline execution tree.
    egress_policy: Option<EgressPolicyContext>,
    /// Provider catalog overlay for this execution. An ACP host can install a
    /// verified endpoint without mutating the process or a sibling server.
    provider_overrides: Option<ProvidersConfig>,
    /// Host-verified endpoint sidecar paired with `provider_overrides`.
    runtime_provider_endpoint_overrides: RuntimeProviderEndpointOverrides,
    /// Capability matrix overlay paired with `provider_overrides`.
    capability_overrides: Option<CapabilitiesFile>,
    /// Active agent-session breadcrumb. It starts empty for a spawned worker
    /// (never inherited from the parent), then the child's own
    /// `begin_agent_session` push is saved/restored across awaits.
    session_stack: Vec<String>,
    /// The thread execution context (cwd/env/source-dir + capability path-scope
    /// root). Not a LIFO stack — a single `Option` the worker sets at startup and
    /// holds across the whole agent loop's awaits, so it cross-wires fan-out
    /// siblings without per-task scoping.
    execution_context: Option<RunExecutionRecord>,
    /// The VM source directory, which anchors source-relative path resolution.
    source_dir: Option<PathBuf>,
    /// The current mutation session (audit/run_id/approval/secret-scope). Same
    /// shape as `execution_context`: one `Option` held across the loop's awaits.
    mutation_session: Option<MutationSessionRecord>,
    /// The resolved environment policy for the session
    /// launched under. Same single-slot shape as `execution_context`: a worker
    /// installs it once and reads it across the loop's awaits when spawning a
    /// subprocess, so each fan-out child must hold its own copy. `None` means
    /// execution is outside a launched session.
    session_environment: Option<crate::security::SessionEnvironment>,
    /// One test case's process-admission gate and union receipt. All inline and
    /// delegated VM work shares this case-local owner without leaking it to a
    /// concurrently-polled sibling case.
    process_admission: Option<ProcessAdmissionContext>,
    /// Host capability bridge installed for the current agent loop. Fan-out
    /// workers inherit the parent's bridge so `host_call` remains routed to the
    /// session host even when their workspace differs from the process cwd.
    host_bridge: Option<std::sync::Arc<crate::bridge::HostBridge>>,
    /// Scoped observation sinks follow all work started inside the capture
    /// closure, including delegated agents. A child may layer its own sink on
    /// top without losing the outer capture when no narrower sink is present.
    loop_sinks: Vec<std::sync::Arc<dyn crate::agent_events::AgentEventSink>>,
    /// The verdict execution-scope owner stack. Unlike `session_stack`, this is
    /// INHERITED by fan-out workers and inline subtasks: they are part of the
    /// SAME program run, so a `run_test` executed in a fan-out body must record
    /// (and later issue) under the run's owner. Without this, a verdict issued
    /// from inside `parallel` would fail closed (the owner would read empty).
    execution_scope: Vec<std::sync::Arc<str>>,
    /// Ordered observable events for this execution tree. JSON runs install a
    /// sink once; inline and spawned work inherit it without exposing sibling
    /// executions to the stream.
    run_event_sink: Option<std::sync::Arc<dyn RunEventSink>>,
    trusted_depth: usize,
    command_hook_depth: usize,
    /// Deterministic pre-approval tool-deny closures. Inherited by a spawned
    /// worker (like the command policy) so a sub-agent honors the same
    /// prechecks unless it installs its own; the re-entrancy depth begins
    /// fresh for a new logical call stack.
    precheck: Vec<std::sync::Arc<crate::value::VmClosure>>,
    precheck_depth: usize,
    /// The `AutoReview` answerer. Inherited for the same reason the precheck
    /// is: a spawned worker that inherited a policy it cannot satisfy, but not
    /// the reviewer that could answer for it, would refuse work its parent
    /// would have been allowed to do. The re-entrancy depth begins fresh,
    /// because the child is a new logical call stack.
    approval_reviewer: Vec<std::sync::Arc<crate::value::VmClosure>>,
    approval_reviewer_depth: usize,
    /// State whose inheritance is specific to child-interpreter subtasks.
    /// Grouping it makes capture and per-poll restoration one typed contract.
    subtask: SubtaskAmbientState,
}

/// Clone the contents of one ambient slot without disturbing it: swap it out,
/// clone, swap it back. Works for both the LIFO `Vec` stacks and the single
/// `Option` contexts (their `Default` is the empty value the swap leaves
/// behind). Used only at spawn time (rare), so the double swap is immaterial.
fn clone_via_swap<T: Clone + Default>(swap: impl Fn(T) -> T) -> T {
    let owned = swap(T::default());
    let cloned = owned.clone();
    let _ = swap(owned);
    cloned
}

impl AmbientExecutionScope {
    /// Snapshot the ambient context a child inherits from its parent at spawn
    /// time: the command-policy stack, dynamic-permission stack, and the
    /// runtime-context overlay (so the child's events keep the parent's
    /// `run_id`/`workflow_id` while it layers its own `worker_id` on top).
    ///
    /// Execution and approval policy are deliberately *not* captured here: the
    /// worker re-establishes its own base execution policy and approval policy
    /// explicitly at startup, and the bridge-trust / command-hook depths begin
    /// fresh for a new logical call stack. The transient per-call frames
    /// (`llm_render`, `connector_ctx`) start empty too — they are pushed by the
    /// child's own `llm_call` / connector export — and only need isolation, not
    /// inheritance. Autonomy policy IS inherited (the child runs under the
    /// parent's autonomy tier).
    /// `CURRENT_SESSION_STACK` is also isolated but deliberately starts empty:
    /// inheriting the parent's active session would attribute child writes to
    /// the parent. The child's own agent-session init pushes its session id into
    /// this task-local copy, and `swap_in` preserves that breadcrumb across
    /// subsequent awaits.
    ///
    /// The execution context, source dir, mutation session, and operator grants
    /// ARE inherited:
    /// the worker overwrites all three at the top of `execute_worker_config`,
    /// but it first awaits (`emit_worker_event`) while they still hold the
    /// parent's values — pre-scoping the raw thread-local already read through to
    /// the parent there, so capturing them keeps the single-worker path
    /// byte-identical while giving each fan-out child its own isolated copy.
    pub(crate) fn capture_inherited() -> Self {
        Self {
            operator_approval_grants: clone_via_swap(swap_operator_approval_grant_stack),
            command: clone_via_swap(swap_command_policy_stack),
            precheck: clone_via_swap(swap_tool_precheck_stack),
            approval_reviewer: clone_via_swap(swap_approval_reviewer_stack),
            approval_reviewer_depth: 0,
            permissions: clone_via_swap(swap_dynamic_permission_stack),
            runtime_context: clone_via_swap(swap_runtime_context_overlay_stack),
            autonomy: clone_via_swap(swap_autonomy_policy_stack),
            llm_transcript: clone_via_swap(swap_llm_transcript_ambient),
            llm_mock: current_llm_mock_context(),
            egress_policy: clone_via_swap(swap_policy_context),
            execution_context: clone_via_swap(swap_thread_execution_context),
            source_dir: clone_via_swap(swap_source_dir),
            mutation_session: clone_via_swap(swap_mutation_session),
            session_environment: clone_via_swap(swap_session_environment),
            process_admission: clone_via_swap(swap_process_admission_context),
            host_bridge: clone_via_swap(swap_current_host_bridge),
            loop_sinks: clone_via_swap(swap_current_loop_sinks),
            provider_overrides: clone_via_swap(swap_provider_overrides),
            runtime_provider_endpoint_overrides: clone_via_swap(
                swap_runtime_provider_endpoint_overrides,
            ),
            capability_overrides: clone_via_swap(swap_capability_overrides),
            // The program-run owner IS inherited: a fan-out worker executes the
            // same run's tools, so its `run_test` records under (and issues in)
            // that run's scope.
            execution_scope: clone_via_swap(
                crate::observability::execution_scope::swap_execution_scope_stack,
            ),
            run_event_sink: clone_via_swap(swap_run_event_sink),
            subtask: SubtaskAmbientState::capture(),
            ..Self::default()
        }
    }

    /// Capture an isolated COPY of the FULL current ambient context for an
    /// inline concurrent subtask — the `parallel` / `parallel_each` /
    /// `parallel settle` primitives that an agent loop uses to dispatch a turn's
    /// tool calls (and that user pipelines use for fan-out map bodies).
    ///
    /// Unlike [`capture_inherited`] — which resets `session_stack` and leaves the
    /// re-pushed policy/render slots empty because a fan-out WORKER opens its own
    /// agent session and pushes its own execution/approval policy — an inline
    /// subtask is the SAME logical agent as the task that spawned it. It runs
    /// that agent's tool calls concurrently and never re-establishes any of this
    /// context itself, so it must inherit every slot, INCLUDING the active
    /// session. That session is what the hostlib write chokepoint
    /// (`fs_snapshot::auto_capture_for_write` -> `active_session_id`) reads to
    /// record `files_written`.
    ///
    /// Without this, a subtask spawned while the parent's `scope_ambient` is
    /// swapped out (e.g. a fan-out worker awaiting the parallel-tool join — the
    /// subtasks are polled by the `LocalSet` as independent tasks, NOT nested in
    /// the parent's poll) reads an empty or sibling `CURRENT_SESSION_STACK`, so
    /// its writes are dropped from the session's changed-path record and the
    /// sub-agent receipt reports `files_written: []` for a child that really did
    /// edit files. The single-worker path hid this because nothing competes for
    /// the thread-local there.
    pub(crate) fn capture_for_inline_subtask() -> Self {
        Self {
            execution: clone_via_swap(swap_execution_policy_stack),
            approval: clone_via_swap(swap_approval_policy_stack),
            operator_approval_grants: clone_via_swap(swap_operator_approval_grant_stack),
            command: clone_via_swap(swap_command_policy_stack),
            permissions: clone_via_swap(swap_dynamic_permission_stack),
            runtime_context: clone_via_swap(swap_runtime_context_overlay_stack),
            autonomy: clone_via_swap(swap_autonomy_policy_stack),
            active_step_context: clone_via_swap(crate::step_runtime::swap_active_context),
            active_context_suspensions: clone_via_swap(
                crate::step_runtime::swap_active_context_suspension_stack,
            ),
            llm_render: clone_via_swap(swap_llm_render_stack),
            llm_transcript: clone_via_swap(swap_llm_transcript_ambient),
            llm_mock: current_llm_mock_context(),
            connector_ctx: clone_via_swap(swap_active_harn_connector_ctx),
            egress_policy: clone_via_swap(swap_policy_context),
            session_stack: clone_via_swap(swap_current_session_stack),
            execution_context: clone_via_swap(swap_thread_execution_context),
            source_dir: clone_via_swap(swap_source_dir),
            mutation_session: clone_via_swap(swap_mutation_session),
            session_environment: clone_via_swap(swap_session_environment),
            process_admission: clone_via_swap(swap_process_admission_context),
            host_bridge: clone_via_swap(swap_current_host_bridge),
            loop_sinks: clone_via_swap(swap_current_loop_sinks),
            provider_overrides: clone_via_swap(swap_provider_overrides),
            runtime_provider_endpoint_overrides: clone_via_swap(
                swap_runtime_provider_endpoint_overrides,
            ),
            capability_overrides: clone_via_swap(swap_capability_overrides),
            execution_scope: clone_via_swap(
                crate::observability::execution_scope::swap_execution_scope_stack,
            ),
            run_event_sink: clone_via_swap(swap_run_event_sink),
            trusted_depth: clone_via_swap(swap_trusted_bridge_depth),
            command_hook_depth: clone_via_swap(swap_command_policy_hook_depth),
            precheck: clone_via_swap(swap_tool_precheck_stack),
            precheck_depth: clone_via_swap(swap_tool_precheck_depth),
            approval_reviewer: clone_via_swap(swap_approval_reviewer_stack),
            approval_reviewer_depth: clone_via_swap(swap_approval_reviewer_depth),
            subtask: SubtaskAmbientState::capture(),
        }
    }

    /// Set the placement every subtask of this scope's execution tree uses.
    pub(crate) fn set_subtask_placement(
        &mut self,
        placement: Option<crate::vm::subtask::SubtaskPlacement>,
    ) {
        self.subtask.set_placement(placement);
    }

    /// Swap this scope with the ambient thread-locals one field at a time.
    /// Avoiding a whole-`Self` temporary keeps the poll stack bounded as new
    /// ambient capabilities are added.
    fn swap_in_place(&mut self) {
        fn swap_slot<T: Default>(slot: &mut T, swap: impl FnOnce(T) -> T) {
            *slot = swap(std::mem::take(slot));
        }

        swap_slot(&mut self.execution, swap_execution_policy_stack);
        swap_slot(&mut self.approval, swap_approval_policy_stack);
        swap_slot(
            &mut self.operator_approval_grants,
            swap_operator_approval_grant_stack,
        );
        swap_slot(&mut self.command, swap_command_policy_stack);
        swap_slot(&mut self.permissions, swap_dynamic_permission_stack);
        swap_slot(
            &mut self.runtime_context,
            swap_runtime_context_overlay_stack,
        );
        swap_slot(&mut self.autonomy, swap_autonomy_policy_stack);
        swap_slot(
            &mut self.active_step_context,
            crate::step_runtime::swap_active_context,
        );
        swap_slot(
            &mut self.active_context_suspensions,
            crate::step_runtime::swap_active_context_suspension_stack,
        );
        swap_slot(&mut self.llm_render, swap_llm_render_stack);
        swap_slot(&mut self.llm_transcript, swap_llm_transcript_ambient);
        swap_slot(&mut self.llm_mock, swap_llm_mock_context);
        swap_slot(&mut self.connector_ctx, swap_active_harn_connector_ctx);
        swap_slot(&mut self.egress_policy, swap_policy_context);
        swap_slot(&mut self.session_stack, swap_current_session_stack);
        swap_slot(&mut self.execution_context, swap_thread_execution_context);
        swap_slot(&mut self.source_dir, swap_source_dir);
        swap_slot(&mut self.mutation_session, swap_mutation_session);
        swap_slot(&mut self.session_environment, swap_session_environment);
        swap_slot(&mut self.process_admission, swap_process_admission_context);
        swap_slot(&mut self.host_bridge, swap_current_host_bridge);
        swap_slot(&mut self.loop_sinks, swap_current_loop_sinks);
        swap_slot(&mut self.provider_overrides, swap_provider_overrides);
        swap_slot(
            &mut self.runtime_provider_endpoint_overrides,
            swap_runtime_provider_endpoint_overrides,
        );
        swap_slot(&mut self.capability_overrides, swap_capability_overrides);
        swap_slot(
            &mut self.execution_scope,
            crate::observability::execution_scope::swap_execution_scope_stack,
        );
        swap_slot(&mut self.run_event_sink, swap_run_event_sink);
        swap_slot(&mut self.trusted_depth, swap_trusted_bridge_depth);
        swap_slot(&mut self.command_hook_depth, swap_command_policy_hook_depth);
        swap_slot(&mut self.precheck, swap_tool_precheck_stack);
        swap_slot(&mut self.precheck_depth, swap_tool_precheck_depth);
        swap_slot(&mut self.approval_reviewer, swap_approval_reviewer_stack);
        swap_slot(
            &mut self.approval_reviewer_depth,
            swap_approval_reviewer_depth,
        );
        self.subtask.swap_in_place();
    }
}

pin_project! {
    /// A future that runs `inner` with one ambient slot — the VM source dir —
    /// swapped in around every poll.
    ///
    /// [`Scoped`] swaps every slot, which is what a fan-out worker needs: it
    /// must not observe, or leak to, a sibling's capability context. A
    /// generator or stream body is a weaker case. It is the same logical caller
    /// running concurrently and reads capabilities live; the only ambient fact
    /// it owns is where its own module sits.
    ///
    /// The distinction is worth a separate type because of poll frequency. A
    /// fan-out worker is polled a handful of times, so a whole-scope swap is
    /// free. A generator body is polled once per yielded value, and swapping
    /// the full scope there measured +42% CPU on a 200k-yield loop.
    ///
    /// `scoped` holds the directory to install while `inner` runs. Between
    /// polls it holds the polling thread's own directory — the swap invariant
    /// seen from outside.
    pub(crate) struct SourceDirScoped<F> {
        #[pin]
        inner: F,
        scoped: Option<PathBuf>,
        active: bool,
    }
}

/// Put the polling thread's source dir back when `inner` returns or panics.
/// Same role as [`RestoreGuard`], over the one slot.
struct SourceDirRestore<'a> {
    slot: &'a mut Option<PathBuf>,
}

impl Drop for SourceDirRestore<'_> {
    fn drop(&mut self) {
        *self.slot = swap_source_dir(self.slot.take());
    }
}

impl<F: Future> Future for SourceDirScoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        if !*this.active {
            return this.inner.poll(cx);
        }
        *this.scoped = swap_source_dir(this.scoped.take());
        let _restore = SourceDirRestore { slot: this.scoped };
        this.inner.poll(cx)
    }
}

/// Run a spawned VM body anchored on the module that defined it.
///
/// A generator or stream body resolves `render`, asset paths, and
/// `source_dir()` against its defining module, not the module that called it.
/// Writing the thread-local at spawn time and unwinding it when the body's
/// frame pops cannot express that: the two happen on different tasks, so the
/// creator resumes with the callee's directory still installed and the eventual
/// restore lands on whichever task is running by then. Per-poll swapping is why
/// `VM_SOURCE_DIR` is cataloged `Captured`.
///
/// `None` means the closure has no module of its own and keeps whatever the
/// polling thread has, so the wrapper stays inert.
pub(crate) fn scope_spawned_source_dir<F: Future>(
    source_dir: Option<PathBuf>,
    inner: F,
) -> SourceDirScoped<F> {
    SourceDirScoped {
        inner,
        active: source_dir.is_some(),
        scoped: source_dir,
    }
}

/// Run `inner` with an execution-owned run-event sink.
pub(crate) fn scope_run_event_sink<F: Future>(
    sink: std::sync::Arc<dyn RunEventSink>,
    inner: F,
) -> Scoped<F> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    scope.run_event_sink = Some(sink);
    scope_ambient(scope, inner)
}

mod policy_scope;
pub use policy_scope::scope_execution_policy;
pub(crate) use policy_scope::{
    scope_approval_policy, scope_autonomy_policy, scope_command_policy, scope_dynamic_permissions,
};

/// Run `inner` with the exact provider and capability overlays supplied by an
/// embedding host.
///
/// The wrapper preserves all other ambient execution context and swaps these
/// two overlay slots on every future poll. It therefore remains correct when
/// two local ACP servers or agent workers interleave on one Tokio thread.
pub fn scope_llm_runtime_overrides<F: Future>(
    provider_overrides: Option<ProvidersConfig>,
    capability_overrides: Option<CapabilitiesFile>,
    inner: F,
) -> impl Future<Output = F::Output> {
    scope_llm_runtime_overrides_with_provider_endpoints(
        provider_overrides,
        capability_overrides,
        RuntimeProviderEndpointOverrides::default(),
        inner,
    )
}

/// Run `inner` with exact catalog, capability, and host-verified endpoint
/// overlays. The sidecar is poll-scoped with the catalog, so concurrent ACP
/// servers cannot cross-route one another while their futures interleave.
pub fn scope_llm_runtime_overrides_with_provider_endpoints<F: Future>(
    provider_overrides: Option<ProvidersConfig>,
    capability_overrides: Option<CapabilitiesFile>,
    runtime_provider_endpoint_overrides: RuntimeProviderEndpointOverrides,
    inner: F,
) -> impl Future<Output = F::Output> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    scope.provider_overrides = provider_overrides;
    scope.capability_overrides = capability_overrides;
    scope.runtime_provider_endpoint_overrides = runtime_provider_endpoint_overrides;
    scope_ambient(scope, inner)
}

/// Run one entrypoint future with an execution-owned trigger registry.
///
/// The registry is swapped around every poll with the rest of the ambient
/// execution scope. Manifest reconciliation and dynamic registrations made by
/// the entrypoint therefore cannot leak into a later in-process run, while the
/// caller's exact registry is restored when the future yields or completes.
pub fn scope_fresh_trigger_registry<F: Future>(inner: F) -> impl Future<Output = F::Output> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    scope
        .subtask
        .set_trigger_registry(crate::triggers::registry::runtime::fresh_trigger_registry());
    scope_ambient(scope, inner)
}

pin_project! {
    /// A future that runs `inner` with `scope` installed as the ambient
    /// execution scope. See the module docs.
    pub(crate) struct Scoped<F> {
        #[pin]
        inner: F,
        // Keep the large, extensible ambient snapshot out of the future's
        // inline state. Deep VM futures already run close to nextest's thread
        // stack limit; adding one captured capability must not enlarge every
        // scoped future frame.
        scope: Box<AmbientExecutionScope>,
        commit_on_ready: bool,
    }
}

/// Run `inner` with its own isolated [`AmbientExecutionScope`]. The scope is
/// swapped into the thread-locals around every poll, so the task never observes
/// — and never leaks to — a sibling's capability context.
pub(crate) fn scope_ambient<F: Future>(scope: AmbientExecutionScope, inner: F) -> Scoped<F> {
    Scoped {
        inner,
        scope: Box::new(scope),
        commit_on_ready: false,
    }
}

/// Run `inner` as a transaction over the current ambient execution scope.
///
/// A pending or panicking poll restores the exact caller scope. Once `inner`
/// reaches `Ready`, its final ambient state stays installed, preserving the
/// synchronous completion semantics of the wrapped operation.
pub(crate) fn scope_ambient_transaction<F: Future>(inner: F) -> Scoped<F> {
    Scoped {
        inner,
        scope: Box::new(AmbientExecutionScope::capture_for_inline_subtask()),
        commit_on_ready: true,
    }
}

/// Preserve the caller's complete logical execution scope in a spawned task.
pub(crate) fn scope_inline_subtask<F: Future>(inner: F) -> Scoped<F> {
    scope_ambient(AmbientExecutionScope::capture_for_inline_subtask(), inner)
}

/// Run one asynchronous tool execution under its resolved agent session.
///
/// Direct tool dispatch can supply a `session_id` even when the caller has no
/// matching ambient session (or is temporarily nested under another one).
/// Host capabilities such as staged filesystem writes and mutation receipts
/// resolve their owner from the ambient session, so the execution boundary
/// must install the dispatcher's resolved identity around every poll. Keeping
/// this as a specialization of [`scope_ambient`] avoids holding a raw
/// thread-local guard across an `.await`.
pub(crate) fn scope_agent_session<F: Future>(session_id: String, inner: F) -> Scoped<F> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    if scope.session_stack.last() != Some(&session_id) {
        scope.session_stack.push(session_id);
    }
    scope_ambient(scope, inner)
}

/// Restores the outer scope (and saves the task's own scope back) on drop, so
/// the thread-locals are left correct even if the inner poll panics.
struct RestoreGuard<'a> {
    scope: &'a mut AmbientExecutionScope,
    armed: bool,
}

impl RestoreGuard<'_> {
    fn commit(&mut self) {
        self.armed = false;
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.scope.swap_in_place();
        }
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        // Install this task's scope, capturing whatever the polling thread had.
        this.scope.swap_in_place();
        let mut restore = RestoreGuard {
            scope: this.scope,
            armed: true,
        };
        let result = this.inner.poll(cx);
        if result.is_ready() && *this.commit_on_ready {
            restore.commit();
        }
        result
    }
}

#[cfg(test)]
mod cancellation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, current_operator_approval_grant,
        install_operator_approval_grant, push_execution_policy, OperatorApprovalGrant,
    };
    use crate::stdlib::template::{
        current_llm_render_context, LlmRenderContext, LlmRenderContextGuard,
    };
    use std::future::pending;
    use std::sync::Arc;

    fn policy_named(tool: &str) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: vec![tool.to_string()],
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_trigger_registry_scope_survives_task_migration() {
        let stable = scope_fresh_trigger_registry(async {
            let owner = crate::triggers::registry::active_trigger_registry();
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            Arc::ptr_eq(
                &owner,
                &crate::triggers::registry::active_trigger_registry(),
            )
        })
        .await;

        assert!(stable, "one logical run must retain one trigger registry");
    }

    #[tokio::test]
    async fn fresh_trigger_registry_scopes_are_isolated_and_restore_the_caller() {
        let caller = crate::triggers::registry::active_trigger_registry();
        let (alpha, beta) = tokio::join!(
            scope_fresh_trigger_registry(async {
                let owner = crate::triggers::registry::active_trigger_registry();
                tokio::task::yield_now().await;
                owner
            }),
            scope_fresh_trigger_registry(async {
                let owner = crate::triggers::registry::active_trigger_registry();
                tokio::task::yield_now().await;
                owner
            }),
        );

        assert!(!Arc::ptr_eq(&alpha, &beta));
        assert!(!Arc::ptr_eq(&alpha, &caller));
        assert!(!Arc::ptr_eq(&beta, &caller));
        assert!(Arc::ptr_eq(
            &caller,
            &crate::triggers::registry::active_trigger_registry(),
        ));
    }

    #[tokio::test]
    async fn ambient_transaction_commits_on_ready() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_named("outer"));

        scope_ambient_transaction(async {
            push_execution_policy(policy_named("committed"));
        })
        .await;

        assert_eq!(
            current_execution_policy().unwrap().tools,
            vec!["committed".to_string()]
        );
        clear_execution_policy_stacks();
    }

    #[tokio::test]
    async fn agent_session_scope_owns_every_poll_and_restores_the_caller() {
        crate::agent_sessions::reset_session_store();
        let _outer = crate::agent_sessions::enter_current_session("ambient-session");

        let observed = scope_agent_session("resolved-dispatch-session".to_string(), async {
            assert_eq!(
                crate::agent_sessions::current_session_id().as_deref(),
                Some("resolved-dispatch-session")
            );
            tokio::task::yield_now().await;
            crate::agent_sessions::current_session_id()
        })
        .await;

        assert_eq!(observed.as_deref(), Some("resolved-dispatch-session"));
        assert_eq!(
            crate::agent_sessions::current_session_id().as_deref(),
            Some("ambient-session")
        );
    }

    async fn spawn_pending_llm_context_task(
        provider: &'static str,
        model: &'static str,
    ) -> tokio::task::JoinHandle<()> {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::task::spawn_local(scope_ambient(
            AmbientExecutionScope::default(),
            async move {
                let _guard =
                    LlmRenderContextGuard::enter(LlmRenderContext::resolve(provider, model));
                let _ = entered_tx.send(());
                pending::<()>().await;
            },
        ));
        entered_rx.await.expect("child entered LLM render context");
        handle
    }

    #[tokio::test]
    async fn cancelling_swapped_out_llm_context_guard_does_not_panic() {
        crate::stdlib::template::llm_context::reset_llm_render_stack();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let handle = spawn_pending_llm_context_task("anthropic", "claude-opus-4-7").await;
                assert!(current_llm_render_context().is_none());
                handle.abort();
                let error = handle
                    .await
                    .expect_err("aborted child should report cancellation");
                assert!(error.is_cancelled(), "unexpected join error: {error}");
            })
            .await;
        assert!(current_llm_render_context().is_none());
    }

    #[tokio::test]
    async fn cancelling_child_llm_context_guard_preserves_parent_context() {
        crate::stdlib::template::llm_context::reset_llm_render_stack();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let parent =
                    LlmRenderContextGuard::enter(LlmRenderContext::resolve("openai", "gpt-5.4"));
                assert_eq!(
                    current_llm_render_context().map(|ctx| ctx.provider),
                    Some("openai".to_string())
                );

                let handle = spawn_pending_llm_context_task("anthropic", "claude-opus-4-7").await;
                assert_eq!(
                    current_llm_render_context().map(|ctx| ctx.provider),
                    Some("openai".to_string()),
                    "child poll should restore the parent LLM render context"
                );
                handle.abort();
                let error = handle
                    .await
                    .expect_err("aborted child should report cancellation");
                assert!(error.is_cancelled(), "unexpected join error: {error}");
                assert_eq!(
                    current_llm_render_context().map(|ctx| ctx.provider),
                    Some("openai".to_string()),
                    "cancelled child guard must not pop the parent LLM render context"
                );

                drop(parent);
                assert!(current_llm_render_context().is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn cancelling_swapped_out_active_context_guard_preserves_parent() {
        crate::step_runtime::reset_thread_local_state();
        crate::step_runtime::register_persona(
            "parent_persona",
            crate::step_runtime::PersonaDefinition {
                name: "parent".into(),
                ..Default::default()
            },
        );
        assert!(crate::step_runtime::maybe_push_active_persona(
            "parent_persona",
            1
        ));

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let scope = AmbientExecutionScope::capture_for_inline_subtask();
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let handle = tokio::task::spawn_local(scope_ambient(scope, async move {
                    let _guard = crate::step_runtime::suspend_active_context();
                    let _ = entered_tx.send(());
                    pending::<()>().await;
                }));
                entered_rx.await.expect("child suspended active context");
                assert_eq!(
                    crate::step_runtime::current_persona_name().as_deref(),
                    Some("parent")
                );

                handle.abort();
                assert!(handle.await.unwrap_err().is_cancelled());
                assert_eq!(
                    crate::step_runtime::current_persona_name().as_deref(),
                    Some("parent")
                );
            })
            .await;
        crate::step_runtime::reset_thread_local_state();
    }

    /// Two cooperatively-scheduled tasks on one `LocalSet`, each pushing a
    /// distinct execution policy and then yielding twice so the sibling runs in
    /// between. With per-task scoping each task must read back ONLY its own
    /// policy; the un-scoped thread-local stack would hand a task its sibling's
    /// top-of-stack (the fan-out cross-wiring bug).
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_execution_policy() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        push_execution_policy(policy_named("alpha"));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_policy().map(|p| p.tools)
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        push_execution_policy(policy_named("beta"));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_policy().map(|p| p.tools)
                    },
                ));
                assert_eq!(alpha.await.unwrap(), Some(vec!["alpha".to_string()]));
                assert_eq!(beta.await.unwrap(), Some(vec!["beta".to_string()]));
            })
            .await;
        // The outer thread is left clean — neither task's policy leaked out.
        assert!(current_execution_policy().is_none());
    }

    #[tokio::test]
    async fn inherited_scope_carries_operator_grant_across_awaits() {
        clear_execution_policy_stacks();
        let grant = OperatorApprovalGrant::from_cli_operations(["git.push".to_string()])
            .expect("valid operation")
            .expect("non-empty grant");
        let grant_guard = install_operator_approval_grant(grant);
        let child_scope = AmbientExecutionScope::capture_inherited();

        let local = tokio::task::LocalSet::new();
        let receipt = local
            .run_until(scope_ambient(child_scope, async {
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
                current_operator_approval_grant().and_then(|grant| grant.receipt_for("git.push"))
            }))
            .await
            .expect("child inherited operator grant");

        assert_eq!(receipt["operation"], "git.push");
        assert!(current_operator_approval_grant().is_some());
        drop(grant_guard);
        assert!(current_operator_approval_grant().is_none());
    }

    /// Files-written attribution regression: fan-out workers are spawned while a
    /// parent session may be current, but they must NOT inherit that session.
    /// Each child opens its own current session and yields before recording a
    /// write. The child's session breadcrumb must survive those awaits and each
    /// path must drain under the child session, not the parent or sibling.
    #[tokio::test]
    async fn scoped_tasks_preserve_child_current_session_for_write_attribution() {
        let parent_session = format!("parent-{}", uuid::Uuid::now_v7());
        let alpha_session = format!("alpha-{}", uuid::Uuid::now_v7());
        let beta_session = format!("beta-{}", uuid::Uuid::now_v7());
        for session in [&parent_session, &alpha_session, &beta_session] {
            crate::agent_sessions::clear_session_changed_paths(session);
        }

        let _parent_guard = crate::agent_sessions::enter_current_session(parent_session.clone());
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alpha_scope = AmbientExecutionScope::capture_inherited();
                let beta_scope = AmbientExecutionScope::capture_inherited();
                let alpha_id = alpha_session.clone();
                let beta_id = beta_session.clone();

                let alpha = tokio::task::spawn_local(scope_ambient(alpha_scope, async move {
                    assert!(
                        crate::agent_sessions::current_session_id().is_none(),
                        "child scope must not inherit the parent session"
                    );
                    let _guard = crate::agent_sessions::enter_current_session(alpha_id.clone());
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    let current = crate::agent_sessions::current_session_id()
                        .expect("child session survives await");
                    crate::agent_sessions::record_session_changed_path(&current, "src/alpha.rs");
                    current
                }));
                let beta = tokio::task::spawn_local(scope_ambient(beta_scope, async move {
                    assert!(
                        crate::agent_sessions::current_session_id().is_none(),
                        "child scope must not inherit the parent session"
                    );
                    let _guard = crate::agent_sessions::enter_current_session(beta_id.clone());
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    let current = crate::agent_sessions::current_session_id()
                        .expect("child session survives await");
                    crate::agent_sessions::record_session_changed_path(&current, "src/beta.rs");
                    current
                }));

                assert_eq!(alpha.await.unwrap(), alpha_session);
                assert_eq!(beta.await.unwrap(), beta_session);
            })
            .await;

        assert_eq!(
            crate::agent_sessions::current_session_id().as_deref(),
            Some(parent_session.as_str()),
            "parent session restored after child polls"
        );
        assert_eq!(
            crate::agent_sessions::take_session_changed_paths(&alpha_session),
            vec!["src/alpha.rs".to_string()]
        );
        assert_eq!(
            crate::agent_sessions::take_session_changed_paths(&beta_session),
            vec!["src/beta.rs".to_string()]
        );
        assert!(
            crate::agent_sessions::take_session_changed_paths(&parent_session).is_empty(),
            "child writes must not attribute to parent"
        );
    }

    /// `files_written` fan-out regression: an agent loop dispatches a turn's tool
    /// calls through `parallel` / `parallel settle`, which run each call as an
    /// INDEPENDENT `LocalSet` task — not nested in the worker's poll. While the
    /// worker awaits that join its `scope_ambient` is swapped out, so a subtask
    /// that does NOT carry the worker's session reads an empty/sibling
    /// `CURRENT_SESSION_STACK` and its write is dropped from the receipt
    /// (the real bug behind a "wrote 0 file(s)" / "0/N units completed" report).
    /// [`capture_for_inline_subtask`] is what `vm::ops::parallel` wraps each
    /// subtask with so the write attributes to the agent's session. Two
    /// contending workers prove each subtask sees ITS worker's session — and a
    /// `capture_inherited` CONTROL proves the contention is real (an
    /// un-inherited subtask genuinely sees no session, so the assertion is not
    /// vacuous).
    #[tokio::test]
    async fn inline_subtask_scope_carries_worker_session_under_contention() {
        let parent_session = format!("parent-{}", uuid::Uuid::now_v7());
        let alpha_session = format!("alpha-{}", uuid::Uuid::now_v7());
        let beta_session = format!("beta-{}", uuid::Uuid::now_v7());
        for session in [&parent_session, &alpha_session, &beta_session] {
            crate::agent_sessions::clear_session_changed_paths(session);
        }

        let _parent_guard = crate::agent_sessions::enter_current_session(parent_session.clone());
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // One fan-out worker: opens its own session (never the parent's),
                // yields, then dispatches an inline subtask exactly like the agent
                // loop's `parallel settle` tool dispatch.
                let run_worker = |worker_session: String, path: &'static str| {
                    let worker_scope = AmbientExecutionScope::capture_inherited();
                    tokio::task::spawn_local(scope_ambient(worker_scope, async move {
                        assert!(
                            crate::agent_sessions::current_session_id().is_none(),
                            "fan-out worker must not inherit the parent session"
                        );
                        let _guard =
                            crate::agent_sessions::enter_current_session(worker_session.clone());
                        tokio::task::yield_now().await;

                        // CONTROL: a subtask spawned WITHOUT inline-subtask
                        // scoping does not see the worker session — this is the
                        // dropped-write bug, and proves the contention is real.
                        let control = tokio::task::spawn_local(scope_ambient(
                            AmbientExecutionScope::capture_inherited(),
                            async move {
                                tokio::task::yield_now().await;
                                tokio::task::yield_now().await;
                                crate::agent_sessions::current_session_id()
                            },
                        ))
                        .await
                        .unwrap();
                        assert!(
                            control.is_none(),
                            "control subtask must NOT inherit the worker session"
                        );

                        // FIX: the inline-subtask scope carries the worker session,
                        // so the dispatched tool's write records against it.
                        let observed = tokio::task::spawn_local(scope_ambient(
                            AmbientExecutionScope::capture_for_inline_subtask(),
                            async move {
                                tokio::task::yield_now().await;
                                tokio::task::yield_now().await;
                                let session = crate::agent_sessions::current_session_id();
                                if let Some(ref session) = session {
                                    crate::agent_sessions::record_session_changed_path(
                                        session, path,
                                    );
                                }
                                session
                            },
                        ))
                        .await
                        .unwrap();
                        observed
                    }))
                };

                let alpha = run_worker(alpha_session.clone(), "src/alpha.rs");
                let beta = run_worker(beta_session.clone(), "src/beta.rs");
                assert_eq!(
                    alpha.await.unwrap().as_deref(),
                    Some(alpha_session.as_str()),
                    "alpha's inline subtask must observe alpha's worker session"
                );
                assert_eq!(
                    beta.await.unwrap().as_deref(),
                    Some(beta_session.as_str()),
                    "beta's inline subtask must observe beta's worker session"
                );
            })
            .await;

        assert_eq!(
            crate::agent_sessions::take_session_changed_paths(&alpha_session),
            vec!["src/alpha.rs".to_string()],
            "alpha's dispatched write attributes to alpha's session"
        );
        assert_eq!(
            crate::agent_sessions::take_session_changed_paths(&beta_session),
            vec!["src/beta.rs".to_string()],
            "beta's dispatched write attributes to beta's session"
        );
        assert!(
            crate::agent_sessions::take_session_changed_paths(&parent_session).is_empty(),
            "inline-subtask writes must not attribute to the parent"
        );
    }

    /// A task's scope must not leak into work that runs after it on the same
    /// thread once the scoped future has completed.
    #[tokio::test]
    async fn scope_is_restored_after_completion() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                tokio::task::spawn_local(scope_ambient(AmbientExecutionScope::default(), async {
                    push_execution_policy(policy_named("gamma"));
                    tokio::task::yield_now().await;
                }))
                .await
                .unwrap();
            })
            .await;
        assert!(current_execution_policy().is_none());
    }

    fn execution_context_named(name: &str) -> RunExecutionRecord {
        let mut env = std::collections::BTreeMap::new();
        env.insert("WORKER".to_string(), name.to_string());
        RunExecutionRecord {
            cwd: Some(format!("/worktrees/{name}")),
            env,
            ..Default::default()
        }
    }

    fn mutation_session_named(name: &str) -> MutationSessionRecord {
        MutationSessionRecord {
            session_id: format!("session-{name}"),
            run_id: Some(format!("run-{name}")),
            ..Default::default()
        }
    }

    fn test_host_bridge(start_id: u64) -> std::sync::Arc<crate::bridge::HostBridge> {
        let pending =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer: crate::bridge::HostBridgeWriter = std::sync::Arc::new(|_| Ok(()));
        std::sync::Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
            pending, cancelled, writer, start_id,
        ))
    }

    /// F4 regression: fan-out workers must inherit the current host bridge so
    /// `host_call("workspace.project_root")` and other host capabilities still
    /// route to the Burin session host inside children. Each task may install
    /// its own bridge while it runs, and that must not cross-wire a sibling or
    /// leak back to the parent thread-local.
    #[tokio::test]
    async fn scoped_tasks_inherit_and_isolate_current_host_bridge() {
        crate::llm::clear_current_host_bridge();
        let parent_bridge = test_host_bridge(100);
        crate::llm::install_current_host_bridge(parent_bridge.clone());

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let run_worker = |name: &'static str, start_id: u64| {
                    let scope = AmbientExecutionScope::capture_inherited();
                    let expected_parent = parent_bridge.clone();
                    let worker_bridge = test_host_bridge(start_id);
                    let expected_worker = worker_bridge.clone();
                    tokio::task::spawn_local(scope_ambient(scope, async move {
                        let inherited = crate::llm::current_host_bridge()
                            .expect("worker inherits parent host bridge");
                        assert!(
                            std::sync::Arc::ptr_eq(&inherited, &expected_parent),
                            "{name} must inherit the parent host bridge before installing its own"
                        );

                        crate::llm::install_current_host_bridge(worker_bridge);
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;

                        let observed = crate::llm::current_host_bridge()
                            .expect("worker host bridge survives awaits");
                        assert!(
                            std::sync::Arc::ptr_eq(&observed, &expected_worker),
                            "{name} must keep its own host bridge after sibling interleaving"
                        );
                    }))
                };

                let alpha = run_worker("alpha", 200);
                let beta = run_worker("beta", 300);
                alpha.await.unwrap();
                beta.await.unwrap();
            })
            .await;

        let restored =
            crate::llm::current_host_bridge().expect("parent host bridge restored after workers");
        assert!(std::sync::Arc::ptr_eq(&restored, &parent_bridge));
        crate::llm::clear_current_host_bridge();
    }

    /// F1 regression: two cooperatively-scheduled tasks set DISTINCT execution
    /// contexts (distinct cwd + env) and yield twice so the sibling runs in
    /// between. Each task must read back ONLY its own cwd/env. Without scoping
    /// the second-polled task's context overwrites the thread-local and the
    /// first task resumes reading the SIBLING's worktree root + env — the
    /// write-capable fan-out cross-wire.
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_execution_context() {
        use crate::stdlib::process::{current_execution_context, set_thread_execution_context};
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        set_thread_execution_context(Some(execution_context_named("alpha")));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_context()
                            .map(|ctx| (ctx.cwd, ctx.env.get("WORKER").cloned()))
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        set_thread_execution_context(Some(execution_context_named("beta")));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_context()
                            .map(|ctx| (ctx.cwd, ctx.env.get("WORKER").cloned()))
                    },
                ));
                assert_eq!(
                    alpha.await.unwrap(),
                    Some((
                        Some("/worktrees/alpha".to_string()),
                        Some("alpha".to_string())
                    ))
                );
                assert_eq!(
                    beta.await.unwrap(),
                    Some((
                        Some("/worktrees/beta".to_string()),
                        Some("beta".to_string())
                    ))
                );
            })
            .await;
        // The outer thread is left clean — neither task's context leaked out.
        assert!(crate::stdlib::process::current_execution_context().is_none());
    }

    /// F2 regression: two cooperatively-scheduled tasks install DISTINCT
    /// mutation sessions and yield twice. Each must read back ONLY its own
    /// session; without scoping the interleaving children overwrite each other's
    /// session and audit/run-id/secret-access attribution lands under the wrong
    /// child.
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_mutation_session() {
        use crate::orchestration::{current_mutation_session, install_current_mutation_session};
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        install_current_mutation_session(Some(mutation_session_named("alpha")));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_mutation_session().map(|s| (s.session_id, s.run_id))
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        install_current_mutation_session(Some(mutation_session_named("beta")));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_mutation_session().map(|s| (s.session_id, s.run_id))
                    },
                ));
                assert_eq!(
                    alpha.await.unwrap(),
                    Some(("session-alpha".to_string(), Some("run-alpha".to_string())))
                );
                assert_eq!(
                    beta.await.unwrap(),
                    Some(("session-beta".to_string(), Some("run-beta".to_string())))
                );
            })
            .await;
        // The outer thread is left clean — neither task's session leaked out.
        assert!(crate::orchestration::current_mutation_session().is_none());
    }

    /// Session-environment isolation: two cooperatively-scheduled tasks install
    /// distinct policies and yield twice so the sibling runs in between. Each
    /// must read back only its own environment; without per-task scoping, the
    /// second-polled task would overwrite the thread-local and the first would
    /// resume building its
    /// subprocess environment under the SIBLING's grants — a credential
    /// cross-wire. Mirrors the F1/F2 execution-context/mutation-session guards.
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_session_environment() {
        use crate::security::{
            EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment,
        };
        use crate::stdlib::process::{current_session_environment, set_session_environment};

        let grant_specs = vec![GrantSpec {
            name: "gh".to_string(),
            source: GrantSourceSpec::SecretStore {
                account: "gh".to_string(),
                key: "token".to_string(),
            },
            expose_as_env: Some("GH_TOKEN".to_string()),
            for_command: None,
        }];
        let granted =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, grant_specs, &|_| None)
                .unwrap();

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        set_session_environment(Some(SessionEnvironment::isolated()));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_session_environment().map(|p| (p.kind(), p.grants().len()))
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async move {
                        set_session_environment(Some(granted));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_session_environment().map(|p| (p.kind(), p.grants().len()))
                    },
                ));
                assert_eq!(
                    alpha.await.unwrap(),
                    Some((EnvironmentPolicyKind::Isolated, 0))
                );
                assert_eq!(
                    beta.await.unwrap(),
                    Some((EnvironmentPolicyKind::Granted, 1))
                );
            })
            .await;
        // The outer thread is left clean — neither task's profile leaked out.
        assert!(crate::stdlib::process::current_session_environment().is_none());
    }
}
