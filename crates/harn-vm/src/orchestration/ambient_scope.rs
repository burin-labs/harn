//! Per-task ambient execution scope.
//!
//! Capability/identity context — execution policy, approval policy, command
//! policy, dynamic permissions, the current host bridge, the bridge-trust +
//! command-hook depths, and the runtime-context overlay — is held in
//! thread-local LIFO stacks or single slots. The same
//! hazard applies to the single-slot `Option` contexts a worker installs for the
//! whole agent loop: the VM execution context (cwd/env/source-dir + the
//! capability path-scope root) and the mutation session (audit/run_id/approval/
//! secret-scope). That model is sound for a single synchronous call stack, but a
//! guard held across an `.await` is **not**: workers are spawned with
//! [`tokio::task::spawn_local`], so several of them interleave on one thread
//! (and, under a work-stealing multi-thread runtime, migrate between threads). A
//! child that installs its policy/context, awaits its model call, and resumes
//! would otherwise read whatever a *sibling* installed in the meantime —
//! cross-wiring each child's file scoping, env, worktree root, tool ceiling,
//! approval, secret scope, and event attribution.
//!
//! [`AmbientExecutionScope`] gives every spawned worker its **own** copy of
//! these stacks. [`scope_ambient`] wraps the worker future so the task's scope
//! is swapped into the thread-locals on poll-enter and swapped back out on
//! poll-exit (the same technique `tracing::Instrument` uses for span context).
//! Only the currently-polling task's scope is ever live on a thread, so the
//! cooperative/work-stealing interleaving is invisible to capability checks.
//! Each swap is an O(1) `mem::replace` of a `Vec`/`usize`/`Option`, so the
//! per-poll cost is a handful of pointer swaps regardless of stack depth.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use super::command_policy::{
    swap_command_policy_hook_depth, swap_command_policy_stack, CommandPolicy,
};
use super::policy::{
    swap_approval_policy_stack, swap_execution_policy_stack, swap_trusted_bridge_depth,
    CapabilityPolicy, ToolApprovalPolicy,
};
use super::{swap_mutation_session, MutationSessionRecord, RunExecutionRecord};
use crate::agent_sessions::swap_current_session_stack;
use crate::autonomy::{swap_autonomy_policy_stack, AutonomyPolicy};
use crate::connectors::harn_module::swap_active_harn_connector_ctx;
use crate::connectors::ConnectorCtx;
use crate::llm::agent_observe::{swap_llm_transcript_ambient, LlmTranscriptAmbient};
use crate::llm::capabilities::{
    swap_user_overrides as swap_capability_overrides, CapabilitiesFile,
};
use crate::llm::mock::{current_llm_mock_context, swap_llm_mock_context, LlmMockContext};
use crate::llm::permissions::{swap_dynamic_permission_stack, DynamicPermissionPolicy};
use crate::llm::swap_current_host_bridge;
use crate::llm_config::{
    swap_runtime_provider_endpoint_overrides, swap_user_overrides as swap_provider_overrides,
    ProvidersConfig, RuntimeProviderEndpointOverrides,
};
use crate::runtime_context::{swap_runtime_context_overlay_stack, RuntimeContextOverlay};
use crate::stdlib::process::{
    swap_session_profile, swap_source_dir, swap_thread_execution_context,
};
use crate::stdlib::template::llm_context::{swap_llm_render_stack, LlmRenderContextFrame};

/// An isolated snapshot of every ambient capability/identity stack a worker
/// task owns while it runs. `Default` is the empty scope (no policies, depth 0).
#[derive(Default, Clone)]
pub(crate) struct AmbientExecutionScope {
    execution: Vec<CapabilityPolicy>,
    approval: Vec<ToolApprovalPolicy>,
    command: Vec<CommandPolicy>,
    permissions: Vec<DynamicPermissionPolicy>,
    runtime_context: Vec<RuntimeContextOverlay>,
    autonomy: Vec<AutonomyPolicy>,
    llm_render: Vec<LlmRenderContextFrame>,
    llm_transcript: LlmTranscriptAmbient,
    /// Inline fixtures and observations shared by one VM execution tree.
    llm_mock: LlmMockContext,
    connector_ctx: Vec<ConnectorCtx>,
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
    /// The capability profile (hermetic / grant-carrying lane) the session
    /// launched under. Same single-slot shape as `execution_context`: a worker
    /// installs it once and reads it across the loop's awaits when spawning a
    /// subprocess, so each fan-out child must hold its own copy. `None` is the
    /// legacy no-profile path.
    session_profile: Option<crate::security::SessionProfile>,
    /// Host capability bridge installed for the current agent loop. Fan-out
    /// workers inherit the parent's bridge so `host_call` remains routed to the
    /// session host even when their workspace differs from the process cwd.
    host_bridge: Option<std::sync::Arc<crate::bridge::HostBridge>>,
    /// The verdict execution-scope owner stack. Unlike `session_stack`, this is
    /// INHERITED by fan-out workers and inline subtasks: they are part of the
    /// SAME program run, so a `run_test` executed in a fan-out body must record
    /// (and later issue) under the run's owner. Without this, a verdict issued
    /// from inside `parallel` would fail closed (the owner would read empty).
    execution_scope: Vec<std::sync::Arc<str>>,
    trusted_depth: usize,
    command_hook_depth: usize,
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
    /// Capture the caller's full ambient context for one top-level VM run and
    /// append a fresh execution owner. The resulting scope is installed by
    /// [`scope_ambient`] around every poll of the VM future, so two top-level
    /// executions interleaved on one thread never observe each other's owner.
    /// Keeping the captured stack beneath the fresh owner preserves nested VM
    /// execution: completion restores the exact outer ambient context.
    pub(crate) fn capture_for_top_level_execution(
        owner: std::sync::Arc<str>,
        llm_mock: LlmMockContext,
    ) -> Self {
        let mut scope = Self::capture_for_inline_subtask();
        scope.execution_scope.push(owner);
        scope.llm_mock = llm_mock;
        scope
    }

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
    /// The execution context, source dir, and mutation session ARE inherited:
    /// the worker overwrites all three at the top of `execute_worker_config`,
    /// but it first awaits (`emit_worker_event`) while they still hold the
    /// parent's values — pre-scoping the raw thread-local already read through to
    /// the parent there, so capturing them keeps the single-worker path
    /// byte-identical while giving each fan-out child its own isolated copy.
    pub(crate) fn capture_inherited() -> Self {
        Self {
            command: clone_via_swap(swap_command_policy_stack),
            permissions: clone_via_swap(swap_dynamic_permission_stack),
            runtime_context: clone_via_swap(swap_runtime_context_overlay_stack),
            autonomy: clone_via_swap(swap_autonomy_policy_stack),
            llm_transcript: clone_via_swap(swap_llm_transcript_ambient),
            llm_mock: current_llm_mock_context(),
            execution_context: clone_via_swap(swap_thread_execution_context),
            source_dir: clone_via_swap(swap_source_dir),
            mutation_session: clone_via_swap(swap_mutation_session),
            session_profile: clone_via_swap(swap_session_profile),
            host_bridge: clone_via_swap(swap_current_host_bridge),
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
            command: clone_via_swap(swap_command_policy_stack),
            permissions: clone_via_swap(swap_dynamic_permission_stack),
            runtime_context: clone_via_swap(swap_runtime_context_overlay_stack),
            autonomy: clone_via_swap(swap_autonomy_policy_stack),
            llm_render: clone_via_swap(swap_llm_render_stack),
            llm_transcript: clone_via_swap(swap_llm_transcript_ambient),
            llm_mock: current_llm_mock_context(),
            connector_ctx: clone_via_swap(swap_active_harn_connector_ctx),
            session_stack: clone_via_swap(swap_current_session_stack),
            execution_context: clone_via_swap(swap_thread_execution_context),
            source_dir: clone_via_swap(swap_source_dir),
            mutation_session: clone_via_swap(swap_mutation_session),
            session_profile: clone_via_swap(swap_session_profile),
            host_bridge: clone_via_swap(swap_current_host_bridge),
            provider_overrides: clone_via_swap(swap_provider_overrides),
            runtime_provider_endpoint_overrides: clone_via_swap(
                swap_runtime_provider_endpoint_overrides,
            ),
            capability_overrides: clone_via_swap(swap_capability_overrides),
            execution_scope: clone_via_swap(
                crate::observability::execution_scope::swap_execution_scope_stack,
            ),
            trusted_depth: clone_via_swap(swap_trusted_bridge_depth),
            command_hook_depth: clone_via_swap(swap_command_policy_hook_depth),
        }
    }

    /// Install this scope into the ambient thread-locals, returning whatever was
    /// installed before so the caller can restore it. O(1) per stack.
    fn swap_in(self) -> Self {
        Self {
            execution: swap_execution_policy_stack(self.execution),
            approval: swap_approval_policy_stack(self.approval),
            command: swap_command_policy_stack(self.command),
            permissions: swap_dynamic_permission_stack(self.permissions),
            runtime_context: swap_runtime_context_overlay_stack(self.runtime_context),
            autonomy: swap_autonomy_policy_stack(self.autonomy),
            llm_render: swap_llm_render_stack(self.llm_render),
            llm_transcript: swap_llm_transcript_ambient(self.llm_transcript),
            llm_mock: swap_llm_mock_context(self.llm_mock),
            connector_ctx: swap_active_harn_connector_ctx(self.connector_ctx),
            session_stack: swap_current_session_stack(self.session_stack),
            execution_context: swap_thread_execution_context(self.execution_context),
            source_dir: swap_source_dir(self.source_dir),
            mutation_session: swap_mutation_session(self.mutation_session),
            session_profile: swap_session_profile(self.session_profile),
            host_bridge: swap_current_host_bridge(self.host_bridge),
            provider_overrides: swap_provider_overrides(self.provider_overrides),
            runtime_provider_endpoint_overrides: swap_runtime_provider_endpoint_overrides(
                self.runtime_provider_endpoint_overrides,
            ),
            capability_overrides: swap_capability_overrides(self.capability_overrides),
            execution_scope: crate::observability::execution_scope::swap_execution_scope_stack(
                self.execution_scope,
            ),
            trusted_depth: swap_trusted_bridge_depth(self.trusted_depth),
            command_hook_depth: swap_command_policy_hook_depth(self.command_hook_depth),
        }
    }
}

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

/// How an ambient-shape thread-local relates to per-task fan-out scoping.
///
/// "Ambient-shape" means a thread-local whose name follows the LIFO-stack
/// (`*_STACK`), recursion-depth (`*_DEPTH`), or single-slot
/// execution/identity context (`*_CONTEXT` / `*_SESSION` / `*_CTX`, plus
/// `VM_SOURCE_DIR`) convention — the family that holds context which a worker
/// future may read across an `.await`. F1/F2 were `RefCell<Option<_>>` context
/// slots that escaped the original `*_STACK`-only audit, so the drift guard
/// covers the single-slot shapes too.
///
/// This catalog is the drift guard's test fixture, so it is `#[cfg(test)]`; it
/// lives next to the scope it documents on purpose — read it before adding any
/// ambient thread-local.
#[cfg(test)]
#[derive(Clone, Copy)]
enum AmbientScoping {
    /// Swapped per-poll by [`AmbientExecutionScope::swap_in`]. The worker keeps
    /// its own copy across `.await`, so cooperatively-scheduled siblings never
    /// cross-wire it.
    Captured,
    /// A human reviewed this thread-local and deliberately left it out of the
    /// scope (today). The string records why. Audited capability/identity stacks
    /// that should be wired in the day they become read-across-await inside
    /// fan-out are also listed in [`AUDITED_LATENT_CAPABILITIES`].
    Uncaptured(&'static str),
}

/// THE decision record for every ambient-shape thread-local in this crate.
///
/// F1/F2 (VM_EXECUTION_CONTEXT + CURRENT_MUTATION_SESSION cross-wiring) happened
/// because the capture set was a hand-maintained allow-list with no forcing
/// function: two same-shape thread-locals existed, were read across a worker's
/// awaits, and nobody noticed they weren't captured. This catalog plus
/// `drift_every_ambient_shape_thread_local_is_cataloged` is that forcing
/// function — a new `*_STACK` / `*_DEPTH` / `*_CONTEXT` / `*_SESSION` / `*_CTX`
/// thread-local FAILS the test until it is classified here, so the author must
/// consciously decide `Captured` vs `Uncaptured`.
///
/// Keep the `Captured` entries in lockstep with the fields of
/// [`AmbientExecutionScope`] and the swaps in `swap_in`; the
/// `captured_catalog_matches_scope_fields` test enforces the set.
#[cfg(test)]
const AMBIENT_THREAD_LOCAL_CATALOG: &[(&str, AmbientScoping)] = &[
    // --- Captured: swapped per-poll into each worker's isolated scope. ---
    ("EXECUTION_POLICY_STACK", AmbientScoping::Captured),
    ("EXECUTION_APPROVAL_POLICY_STACK", AmbientScoping::Captured),
    ("COMMAND_POLICY_STACK", AmbientScoping::Captured),
    ("DYNAMIC_PERMISSION_STACK", AmbientScoping::Captured),
    ("RUNTIME_CONTEXT_OVERLAY_STACK", AmbientScoping::Captured),
    ("AUTONOMY_POLICY_STACK", AmbientScoping::Captured),
    ("LLM_RENDER_STACK", AmbientScoping::Captured),
    ("ACTIVE_HARN_CONNECTOR_CTX", AmbientScoping::Captured),
    ("TRUSTED_BRIDGE_CALL_DEPTH", AmbientScoping::Captured),
    ("COMMAND_POLICY_HOOK_DEPTH", AmbientScoping::Captured),
    // F1: cwd/env/source-dir + capability path-scope root.
    ("VM_EXECUTION_CONTEXT", AmbientScoping::Captured),
    ("VM_SOURCE_DIR", AmbientScoping::Captured),
    // F2: audit/run_id/approval/secret-scope.
    ("CURRENT_MUTATION_SESSION", AmbientScoping::Captured),
    // Session capability profile (hermetic/lane grants): read at subprocess
    // spawn across a worker's awaits, so each fan-out child holds its own copy.
    ("SESSION_PROFILE_CONTEXT", AmbientScoping::Captured),
    // Host capability bridge: fan-out workers need the same host_call routing
    // as the parent agent loop, even when process cwd differs from project root.
    ("CURRENT_HOST_BRIDGE", AmbientScoping::Captured),
    // Files-written/session breadcrumb: isolated per task, but not inherited.
    ("CURRENT_SESSION_STACK", AmbientScoping::Captured),
    // Provider/capability overlays can change transport routing and tool wire
    // policy, so embedded ACP requests carry them across every await.
    ("LLM_CONFIG_OVERRIDES_CONTEXT", AmbientScoping::Captured),
    (
        "LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT",
        AmbientScoping::Captured,
    ),
    ("LLM_CAPABILITY_OVERRIDES_CONTEXT", AmbientScoping::Captured),
    // Inline fixture state follows the VM execution tree across every await.
    ("LLM_MOCK_CONTEXT", AmbientScoping::Captured),
    // Verdict execution-scope owner: INHERITED (unlike the session stack) because
    // a fan-out worker runs the same program run and its run_test must record /
    // issue under that run's owner. See observability/execution_scope.rs.
    ("ACTIVE_EXECUTION_SCOPE_STACK", AmbientScoping::Captured),
    // --- Uncaptured: audited capability/identity context, same shape, NOT yet
    // read across a fan-out child's awaits. Wire each into the scope the day it
    // becomes cross-task-read (mirrors AUDITED_LATENT_CAPABILITIES). ---
    (
        "SECURITY_POLICY_STACK",
        AmbientScoping::Uncaptured(
            "[latent-capability] security/mod.rs MCP-schema/security policy; not \
             set per-worker today. Capture when a fan-out child reads it across an await.",
        ),
    ),
    (
        "ACTIVE_TENANT_STACK",
        AmbientScoping::Uncaptured(
            "[latent-capability] harness_tenant.rs tenant identity; not set per-worker today.",
        ),
    ),
    (
        "ACTIVE_PRINCIPAL_STACK",
        AmbientScoping::Uncaptured(
            "[latent-capability] harness_auth.rs principal identity; not set per-worker today.",
        ),
    ),
    (
        "REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH",
        AmbientScoping::Uncaptured(
            "[latent-capability] egress/mod.rs egress-policy enforcement depth; not entered \
             per-worker today.",
        ),
    ),
    (
        "REQUIRE_SSRF_GUARD_DEPTH",
        AmbientScoping::Uncaptured(
            "[latent-capability] egress/mod.rs SSRF-guard depth; not entered per-worker today.",
        ),
    ),
    (
        "REDACTION_POLICY_STACK",
        AmbientScoping::Uncaptured(
            "[latent-capability] redact/mod.rs redaction policy; pushed around synchronous \
             redaction, not held across a child await today.",
        ),
    ),
    (
        "ACTIVE_REQUEST_ID_STACK",
        AmbientScoping::Uncaptured(
            "[latent-capability] observability/request_id.rs request-id breadcrumb; attribution \
             only, no capability decision rides on it.",
        ),
    ),
    // --- Uncaptured: shape-matches the naming convention but is structurally
    // not cross-task ambient context. ---
    (
        "PERSONA_STACK",
        AmbientScoping::Uncaptured(
            "step_runtime.rs snapshots+restores this at the worker boundary (own isolation \
             path); not read raw across a fan-out child await.",
        ),
    ),
    (
        "STEP_STACK",
        AmbientScoping::Uncaptured(
            "step_runtime.rs snapshots+restores this at the worker boundary (own isolation \
             path); not read raw across a fan-out child await.",
        ),
    ),
    (
        "CURRENT_TOOL_CALL_STACK",
        AmbientScoping::Uncaptured(
            "agent_sessions.rs tool-call breadcrumb; pushed+popped within a single synchronous \
             dispatch frame.",
        ),
    ),
    ("TRANSCRIPT_DIR_STACK", AmbientScoping::Captured),
    (
        "VM_TRACE_STACK",
        AmbientScoping::Uncaptured(
            "stdlib/logging.rs log-trace breadcrumb (trace ids for log lines); attribution \
             only, no capability decision rides on it.",
        ),
    ),
    (
        "ACTIVE_DISPATCH_CONTEXT",
        AmbientScoping::Uncaptured(
            "triggers/dispatcher trigger-dispatch context for the dispatcher runner, not the \
             fan-out worker path.",
        ),
    ),
    (
        "CURRENT_WORKFLOW_SKILL_CONTEXT",
        AmbientScoping::Uncaptured(
            "orchestration/mod.rs workflow skill context; the workflow runner pins itself to \
             one LocalSet task, so every stage observes the same context (see its doc-comment).",
        ),
    ),
];

/// The same-shape capability/identity thread-locals the F1/F2 audit named as
/// latent: NOT captured today, but the next dev to make one cross-task-relevant
/// must wire it into [`AmbientExecutionScope`]. `audited_latent_capabilities_are_cataloged`
/// asserts each stays present and `Uncaptured` so they cannot silently flip.
#[cfg(test)]
const AUDITED_LATENT_CAPABILITIES: &[&str] = &[
    "SECURITY_POLICY_STACK",
    "ACTIVE_TENANT_STACK",
    "ACTIVE_PRINCIPAL_STACK",
    "REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH",
    "REQUIRE_SSRF_GUARD_DEPTH",
];

pin_project! {
    /// A future that runs `inner` with `scope` installed as the ambient
    /// execution scope. See the module docs.
    pub(crate) struct Scoped<F> {
        #[pin]
        inner: F,
        scope: Option<AmbientExecutionScope>,
    }
}

/// Run `inner` with its own isolated [`AmbientExecutionScope`]. The scope is
/// swapped into the thread-locals around every poll, so the task never observes
/// — and never leaks to — a sibling's capability context.
pub(crate) fn scope_ambient<F: Future>(scope: AmbientExecutionScope, inner: F) -> Scoped<F> {
    Scoped {
        inner,
        scope: Some(scope),
    }
}

/// Preserve the caller's complete logical execution scope in a spawned task.
pub(crate) fn scope_inline_subtask<F: Future>(inner: F) -> Scoped<F> {
    scope_ambient(AmbientExecutionScope::capture_for_inline_subtask(), inner)
}

/// Restores the outer scope (and saves the task's own scope back) on drop, so
/// the thread-locals are left correct even if the inner poll panics.
struct RestoreGuard<'a> {
    outer: Option<AmbientExecutionScope>,
    slot: &'a mut Option<AmbientExecutionScope>,
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(outer) = self.outer.take() {
            *self.slot = Some(outer.swap_in());
        }
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        // Install this task's scope, capturing whatever the polling thread had.
        let task_scope = this.scope.take().unwrap_or_default();
        let outer = task_scope.swap_in();
        let _restore = RestoreGuard {
            outer: Some(outer),
            slot: this.scope,
        };
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{current_execution_policy, push_execution_policy};
    use crate::stdlib::template::{
        current_llm_render_context, LlmRenderContext, LlmRenderContextGuard,
    };
    use std::future::pending;

    fn policy_named(tool: &str) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: vec![tool.to_string()],
            ..Default::default()
        }
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

    /// Session-profile isolation: two cooperatively-scheduled tasks install
    /// DISTINCT capability profiles (a hermetic one and a grant-carrying lane)
    /// and yield twice so the sibling runs in between. Each must read back ONLY
    /// its own profile; without per-task scoping the second-polled task's profile
    /// would overwrite the thread-local and the first would resume building its
    /// subprocess environment under the SIBLING's grants — a credential
    /// cross-wire. Mirrors the F1/F2 execution-context/mutation-session guards.
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_session_profile() {
        use crate::security::{GrantSourceSpec, GrantSpec, SessionProfile, SessionProfileKind};
        use crate::stdlib::process::{current_session_profile, set_session_profile};

        let lane_specs = vec![GrantSpec {
            name: "gh".to_string(),
            source: GrantSourceSpec::SecretStore {
                account: "gh".to_string(),
                key: "token".to_string(),
            },
            expose_as_env: Some("GH_TOKEN".to_string()),
        }];
        let lane = SessionProfile::launch(SessionProfileKind::Lane, lane_specs, &|_| None).unwrap();

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        set_session_profile(Some(SessionProfile::hermetic()));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_session_profile().map(|p| (p.kind(), p.grants().len()))
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async move {
                        set_session_profile(Some(lane));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_session_profile().map(|p| (p.kind(), p.grants().len()))
                    },
                ));
                assert_eq!(
                    alpha.await.unwrap(),
                    Some((SessionProfileKind::Hermetic, 0))
                );
                assert_eq!(beta.await.unwrap(), Some((SessionProfileKind::Lane, 1)));
            })
            .await;
        // The outer thread is left clean — neither task's profile leaked out.
        assert!(crate::stdlib::process::current_session_profile().is_none());
    }

    /// F3 drift guard. Walk the crate source, discover every ambient-shape
    /// thread-local (the `*_STACK` / `*_DEPTH` / `*_CONTEXT` / `*_SESSION` /
    /// `*_CTX` / `VM_SOURCE_DIR` family), and assert each is classified in
    /// `AMBIENT_THREAD_LOCAL_CATALOG`. A NEW ambient thread-local fails this test
    /// until the author classifies it `Captured` or `Uncaptured` — the forcing
    /// function F1/F2 lacked. Also fails on stale catalog entries.
    #[test]
    fn drift_every_ambient_shape_thread_local_is_cataloged() {
        use std::collections::BTreeSet;

        fn is_ambient_shape(name: &str) -> bool {
            name == "VM_SOURCE_DIR"
                || name == "CURRENT_HOST_BRIDGE"
                || name.ends_with("_STACK")
                || name.ends_with("_DEPTH")
                || name.ends_with("_CONTEXT")
                || name.ends_with("_SESSION")
                || name.ends_with("_CTX")
        }

        fn collect(dir: &std::path::Path, out: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).expect("read_dir src") {
                let path = entry.expect("dir entry").path();
                // Skip test-support trees so test-only thread-locals (mock
                // clocks, fixtures) never have to enter the production catalog.
                // Match on the component name, not the full absolute path:
                // worktree names such as `release-test-isolation` must not
                // make the drift guard skip the entire `src` tree.
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name == "tests" || name == "test_util" || name.ends_with("_tests")
                    })
                {
                    continue;
                }
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    let content = std::fs::read_to_string(&path).expect("read src file");
                    let mut pending_static: Option<String> = None;
                    for line in content.lines() {
                        // Thread-locals are the only `static _: RefCell<_>` decls
                        // (a bare static RefCell is not Sync, so will not compile).
                        let Some(idx) = line.find("static ") else {
                            if line.contains("RefCell") {
                                if let Some(name) = pending_static.take() {
                                    if is_ambient_shape(&name) {
                                        out.insert(name);
                                    }
                                }
                            }
                            continue;
                        };
                        let after = &line[idx + "static ".len()..];
                        let name: String = after
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        if name.is_empty() {
                            continue;
                        }
                        if line.contains("RefCell") {
                            if is_ambient_shape(&name) {
                                out.insert(name);
                            }
                        } else {
                            pending_static = Some(name);
                        }
                    }
                }
            }
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut discovered = BTreeSet::new();
        collect(&src, &mut discovered);

        let cataloged: BTreeSet<String> = AMBIENT_THREAD_LOCAL_CATALOG
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();

        let missing: Vec<_> = discovered.difference(&cataloged).cloned().collect();
        assert!(
            missing.is_empty(),
            "new ambient-shape thread-local(s) not classified in \
             AMBIENT_THREAD_LOCAL_CATALOG (orchestration/ambient_scope.rs): {missing:?}. Decide \
             whether each must be Captured into AmbientExecutionScope (it is held across a \
             fan-out worker's awaits and would otherwise cross-wire siblings) or is safely \
             Uncaptured, then add it to the catalog. This is the F1/F2 drift guard."
        );

        let stale: Vec<_> = cataloged.difference(&discovered).cloned().collect();
        assert!(
            stale.is_empty(),
            "AMBIENT_THREAD_LOCAL_CATALOG names thread-local(s) no longer in src \
             (renamed/removed?): {stale:?}. Update the catalog."
        );
    }

    /// The catalog's `Captured` set must exactly mirror what the scope actually
    /// swaps. Adding/removing a field+swap in `AmbientExecutionScope` must update
    /// this list and the catalog together; the cross-wire tests above prove the
    /// captured ones isolate.
    #[test]
    fn captured_catalog_matches_scope_fields() {
        use std::collections::BTreeSet;
        let captured: BTreeSet<&str> = AMBIENT_THREAD_LOCAL_CATALOG
            .iter()
            .filter(|(_, scoping)| matches!(scoping, AmbientScoping::Captured))
            .map(|(name, _)| *name)
            .collect();
        let expected: BTreeSet<&str> = [
            "EXECUTION_POLICY_STACK",
            "EXECUTION_APPROVAL_POLICY_STACK",
            "COMMAND_POLICY_STACK",
            "DYNAMIC_PERMISSION_STACK",
            "RUNTIME_CONTEXT_OVERLAY_STACK",
            "AUTONOMY_POLICY_STACK",
            "LLM_RENDER_STACK",
            "ACTIVE_HARN_CONNECTOR_CTX",
            "TRUSTED_BRIDGE_CALL_DEPTH",
            "COMMAND_POLICY_HOOK_DEPTH",
            "VM_EXECUTION_CONTEXT",
            "VM_SOURCE_DIR",
            "CURRENT_MUTATION_SESSION",
            "SESSION_PROFILE_CONTEXT",
            "CURRENT_HOST_BRIDGE",
            "CURRENT_SESSION_STACK",
            "LLM_CONFIG_OVERRIDES_CONTEXT",
            "LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT",
            "LLM_CAPABILITY_OVERRIDES_CONTEXT",
            "LLM_MOCK_CONTEXT",
            "ACTIVE_EXECUTION_SCOPE_STACK",
            "TRANSCRIPT_DIR_STACK",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            captured, expected,
            "the catalog's Captured set diverged from AmbientExecutionScope's swapped fields; \
             keep the struct fields, swap_in, and the catalog in lockstep."
        );
    }

    /// The audited latent capability/identity thread-locals (the F1/F2 audit
    /// named these as same-shape but not-yet-cross-task-read) must stay cataloged
    /// and `Uncaptured` with their tag — a forcing function so a future dev who
    /// makes one read-across-await in fan-out has to revisit the capture decision.
    #[test]
    fn audited_latent_capabilities_are_cataloged() {
        for latent in AUDITED_LATENT_CAPABILITIES {
            let found = AMBIENT_THREAD_LOCAL_CATALOG
                .iter()
                .find(|(name, _)| name == latent);
            let Some((_, scoping)) = found else {
                panic!("{latent} missing from AMBIENT_THREAD_LOCAL_CATALOG");
            };
            match scoping {
                AmbientScoping::Uncaptured(reason) => assert!(
                    reason.contains("[latent-capability]"),
                    "{latent} must keep its [latent-capability] reason tag so the call-out stays visible"
                ),
                AmbientScoping::Captured => panic!(
                    "{latent} is now Captured — wire it fully and drop it from \
                     AUDITED_LATENT_CAPABILITIES"
                ),
            }
        }
    }
}
