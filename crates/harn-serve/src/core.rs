use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harn_vm::agent_events::{AgentEvent, AgentEventSink};
use harn_vm::event_log::{
    active_event_log, install_active_event_log, install_default_for_base_dir, AnyEventLog,
};
use harn_vm::mcp_progress::ProgressContext;
use harn_vm::trust_graph::{append_trust_record, TrustOutcome, TrustRecord};
use harn_vm::{inject_leading_authority, ActorChain, TenantId, TraceId, Vm};
use tokio::task::LocalSet;
use tracing::Instrument;

use crate::auth::{AuthPolicy, AuthRequest, AuthenticatedPrincipal, AuthorizationDecision};
use crate::limits::{LimitContext, LimitDecision, LimitGuard, LimitRegistry};
use crate::replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
use crate::{BudgetSpec, DispatchError, ExportedCallableKind};

mod arguments;
mod config;
mod error_classification;
mod event_log;
use event_log::install_scoped_event_log;
mod prepared_generation;
mod prepared_tools;
mod response;
use arguments::{build_vm_args, canonical_arguments_json};
pub use config::DispatchCoreConfig;
use error_classification::classify_vm_error;
use prepared_generation::PreparedDispatchGeneration;
pub use prepared_generation::{DispatchCallReceipt, DispatchGenerationReceipt};
use prepared_tools::PreparedTools;
pub use response::CallResponse;

fn install_dispatch_vm_runtime(
    vm: &mut Vm,
    script_path: &Path,
    source: &str,
    cancel_token: Arc<AtomicBool>,
) {
    harn_vm::register_vm_stdlib(vm);
    #[cfg(feature = "hostlib")]
    crate::install_dispatch_hostlib(vm);
    let store_base = script_path.parent().unwrap_or(Path::new("."));
    harn_vm::register_store_builtins(vm, store_base);
    harn_vm::register_metadata_builtins(vm, store_base);
    vm.set_source_info(&script_path.display().to_string(), source);
    vm.set_source_dir(store_base);
    vm.install_cancel_token(cancel_token);
    vm.set_harness(harn_vm::Harness::real());
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallArguments {
    Named(BTreeMap<String, serde_json::Value>),
    Positional(Vec<serde_json::Value>),
}

#[derive(Clone, Debug)]
pub struct CallRequest {
    pub adapter: String,
    pub function: String,
    pub arguments: CallArguments,
    pub auth: AuthRequest,
    pub caller: String,
    /// Stable identity for an adapter-level retry of the same logical request.
    /// `None` disables replay; a supplied key must be unique within the
    /// configured replay cache and ordinary repeated calls must not share one.
    pub replay_key: Option<String>,
    pub trace_id: Option<TraceId>,
    pub parent_span_id: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub cancel_token: Option<Arc<AtomicBool>>,
    /// Agent-session id to enter for the duration of the dispatch.
    /// When set, `invoke_function` / `invoke_pipeline` push this id
    /// onto the thread-local agent-session stack so worker lifecycle
    /// events fire under it. Adapters use this to scope an
    /// `AgentEventSink` to the request (e.g. A2A maps `task.id` to a
    /// session id and publishes worker/progress updates onto the task
    /// event stream).
    pub agent_session_id: Option<String>,
    /// Optional request-local event sink for live transport streams.
    ///
    /// Unlike the process-global `harn_vm::agent_events` registry, this sink is
    /// installed only for the active dispatch and is filtered to
    /// `agent_session_id`. That keeps long-lived transports such as A2A SSE
    /// streams from losing in-flight events when sibling tests or embedders
    /// reset global Harn VM state.
    pub agent_event_sink: Option<DispatchAgentEventSink>,
    /// Actor chain to bind to the active agent session for this
    /// dispatch. When unset, `DispatchCore` derives the origin from the
    /// authenticated principal after admission.
    pub actor_chain: Option<ActorChain>,
    /// Deterministic local actor to push onto the resolved chain for
    /// adapters that dispatch through a named agent hop.
    pub actor_chain_hop: Option<String>,
    /// Optional progress context — when supplied, the dispatched
    /// function can call the `mcp_report_progress` builtin to emit
    /// `notifications/progress` for the bound `progressToken`. Only
    /// the MCP transport adapter populates this today; other adapters
    /// leave it `None` and the builtin is a no-op.
    pub progress: Option<ProgressContext>,
    /// Tenant the adapter wants this dispatch to run under, overriding
    /// whatever `AuthPolicy` resolves from the credential. Set this
    /// when the transport already owns tenant resolution (e.g. an
    /// upstream cloud gateway that mapped the API key to a tenant in
    /// its own store before forwarding the call). When `None`, the
    /// tenant is sourced from the authenticated principal.
    pub tenant_id: Option<TenantId>,
    /// Request id pushed onto the ambient observability scope for the
    /// dispatched `.harn` callee. The HTTP/ACP/MCP/A2A adapters mint
    /// one per ingress (honouring `X-Request-Id` when present, falling
    /// back to [`crate::http_codec::fresh_request_id`]) so that every
    /// span/log/metric emitted under the dispatch carries the same id
    /// and the standard error envelope (A.4) round-trips it back to
    /// the caller. `None` for tests / in-process callers with no
    /// ingress to mint against.
    pub request_id: Option<String>,
    /// Opaque embedder auth context resolved at admission (e.g. by a
    /// [`crate::SiteAuth`] hook): the API-key record, session claims,
    /// or whatever else the embedder's host-call bridge needs to see
    /// for this request. harn-serve never interprets it; `invoke_*`
    /// installs it as an ambient scope on the VM thread so a
    /// [`harn_vm::HostCallBridge`] can recover it via
    /// [`crate::current_auth_context`] for the duration of the
    /// dispatch. `None` (the default) installs nothing.
    pub auth_context: Option<serde_json::Value>,
    /// Authenticated principal resolved at admission — subject, scheme,
    /// granted scopes, and optional principal kind. Unlike
    /// [`Self::auth_context`] (the opaque embedder blob surfaced only to
    /// the host-call bridge), this is the generic identity harn-serve
    /// itself vouches for; `invoke_*` installs it as the ambient
    /// `harness.auth` handle (see [`harn_vm::enter_auth_principal`]) so a
    /// `.harn` route can read scopes/subject/kind and compose its own
    /// authorization policy. `None` (the default) leaves the dispatch
    /// unauthenticated (`harness.auth.is_authenticated()` is `false`).
    pub auth_principal: Option<harn_vm::AuthPrincipal>,
}

#[derive(Clone)]
pub struct DispatchAgentEventSink {
    inner: Arc<dyn AgentEventSink>,
}

impl DispatchAgentEventSink {
    pub fn new(inner: Arc<dyn AgentEventSink>) -> Self {
        Self { inner }
    }
}

impl fmt::Debug for DispatchAgentEventSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DispatchAgentEventSink(..)")
    }
}

struct SessionScopedAgentEventSink {
    session_id: String,
    inner: Arc<dyn AgentEventSink>,
}

impl AgentEventSink for SessionScopedAgentEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        if event.session_id() != self.session_id {
            return;
        }
        if harn_vm::agent_events::session_has_external_sink(&self.session_id, &self.inner) {
            return;
        }
        self.inner.handle_event(event);
    }
}

fn request_event_sink(request: &CallRequest) -> Option<Arc<dyn AgentEventSink>> {
    let session_id = request.agent_session_id.as_ref()?;
    let sink = request.agent_event_sink.as_ref()?;
    Some(Arc::new(SessionScopedAgentEventSink {
        session_id: session_id.clone(),
        inner: sink.inner.clone(),
    }))
}

fn resolve_request_actor_chain(
    request: &CallRequest,
    principal: &AuthenticatedPrincipal,
) -> Option<ActorChain> {
    let mut chain = request.actor_chain.clone().or_else(|| {
        request
            .auth_principal
            .as_ref()
            .map(|principal| principal.subject.trim())
            .filter(|subject| !subject.is_empty())
            .map(ActorChain::new)
            .or_else(|| {
                let subject = principal.subject.trim();
                (!subject.is_empty()).then(|| ActorChain::new(subject))
            })
    })?;
    if let Some(actor) = request
        .actor_chain_hop
        .as_deref()
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
    {
        chain.push(actor);
    }
    Some(chain)
}

#[async_trait(?Send)]
pub trait VmConfigurator: Send + Sync {
    fn configure(&self, _vm: &mut Vm) -> Result<(), DispatchError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NoopVmConfigurator;

#[async_trait(?Send)]
impl VmConfigurator for NoopVmConfigurator {}

pub struct DispatchCore {
    config: DispatchCoreConfig,
    tools: PreparedTools,
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    generation: PreparedDispatchGeneration,
}

impl DispatchCore {
    pub fn new(config: DispatchCoreConfig) -> Result<Self, DispatchError> {
        let tools = PreparedTools::prepare(&config.script_path)?;
        let event_log = install_default_for_base_dir(&config.base_dir).map_err(|error| {
            DispatchError::Io(format!(
                "failed to initialize event log for {}: {error}",
                config.base_dir.display()
            ))
        })?;
        let generation = PreparedDispatchGeneration::prepare(&config, tools.exports())?;
        Ok(Self {
            config,
            tools,
            event_log,
            generation,
        })
    }

    pub fn auth_policy(&self) -> &AuthPolicy {
        &self.config.auth_policy
    }

    pub(crate) fn event_log(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub async fn dispatch(&self, mut request: CallRequest) -> Result<CallResponse, DispatchError> {
        let _environment =
            crate::dispatch_environment::declare(self.config.host_inference_boundary);
        let trace_id = request.trace_id.clone().unwrap_or_default();
        let function_scopes = self
            .catalog()
            .function(&request.function)
            .map(|function| function.required_scopes.clone())
            .unwrap_or_default();
        let authorization = self
            .config
            .auth_policy
            .authorize_with_scopes(&request.auth, &function_scopes)
            .await;
        match authorization {
            AuthorizationDecision::Authorized(principal) => {
                // Adapter-supplied tenants override; otherwise the
                // authenticated principal's tenant wins. Resolving here
                // (not later inside `invoke_*`) keeps trust records and
                // span attributes consistent with the value the .harn
                // callee actually sees.
                if request.tenant_id.is_none() {
                    request.tenant_id = principal.tenant_id.clone();
                }
                // Surface the same authenticated identity to the `.harn`
                // callee as the ambient `harness.auth` handle. An adapter
                // that already resolved the principal (e.g. the site
                // adapter's `SiteAuth` hook) wins; otherwise project the
                // policy-resolved principal. The synthetic anonymous
                // principal (allow-all, no credential) binds nothing, so a
                // `.harn` route reads `harness.auth.is_authenticated() ==
                // false`. The AuthPolicy path carries no embedder-assigned
                // `kind`, so it stays `None`.
                if request.auth_principal.is_none() && !principal.is_anonymous() {
                    request.auth_principal = Some(harn_vm::AuthPrincipal {
                        subject: principal.subject.clone(),
                        scheme: principal.scheme.clone(),
                        scopes: principal.granted_scopes.clone(),
                        kind: None,
                    });
                }
                request.actor_chain = resolve_request_actor_chain(&request, &principal);
            }
            AuthorizationDecision::Rejected(message) => {
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Denied,
                    Some(message.clone()),
                )
                .await?;
                return Err(DispatchError::Unauthorized(message));
            }
            AuthorizationDecision::MissingScope { required, granted } => {
                let error = DispatchError::Forbidden { required, granted };
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Denied,
                    Some(error.message()),
                )
                .await?;
                return Err(error);
            }
            // MCP allowlist checks are enforced at the `harness.mcp.*`
            // dispatch boundary inside harn-vm, not on the HTTP edge;
            // surfacing the variant here would mean the policy was
            // queried with a server/tool pair, which the HTTP dispatch
            // path never does. Treat any leak as a policy bug.
            AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Denied,
                    Some(reason.clone()),
                )
                .await?;
                return Err(DispatchError::Unauthorized(reason));
            }
        }

        let function = self.catalog().function(&request.function).ok_or_else(|| {
            DispatchError::MissingExport(format!(
                "function '{}' is not exported by {}",
                request.function,
                self.catalog().script_path.display()
            ))
        })?;
        let canonical_input = canonical_arguments_json(&request.arguments, function)?;
        self.prepared_tool_catalog()
            .validate_input(&request.function, &canonical_input)
            .map_err(|error| DispatchError::Validation(error.to_string()))?;

        // Rate-limit + backpressure gate. Every dispatch attempt passes this
        // gate before replay lookup. The in-flight counter decrements when the
        // guard drops, including on cached replies and panics.
        let _limit_guard = self.check_limits(&request, function)?;

        let replay_key = request.replay_key.clone().map(ReplayKey);
        if let Some(key) = replay_key.as_ref() {
            if let Some(cached) = self.config.replay_cache.get(key).await? {
                self.prepared_tool_catalog()
                    .validate_output(&request.function, &cached.value)
                    .map_err(|error| {
                        DispatchError::Execution(format!(
                            "replay cache contains a value outside the current tool contract: {error}"
                        ))
                    })?;
                return Ok(CallResponse::from_replay(
                    request.function.clone(),
                    cached,
                    trace_id,
                ));
            }
        }

        // Per-dispatch resource budget caps live on `function.budget` and are
        // installed inside `invoke_function` / `invoke_pipeline`. Their ceiling
        // and `Arc` count travel together in one `CallBudget`, and
        // `AmbientExecutionScope` carries it into every subtask. Fan-out spends
        // one allowance regardless of which OS thread each branch uses.

        // tenant_id is a low-cardinality routing key (one entry per
        // tenant), not PII — safe to record as a span attribute so
        // exporters can filter traces by tenant. `Empty` until populated
        // so the absent case isn't recorded as the literal string
        // `"None"`. Recorded once after the span opens, mirroring how
        // OTEL bindings expect span attributes to be set.
        let span = tracing::info_span!(
            target: "harn.serve",
            "harn_serve.dispatch",
            adapter = %request.adapter,
            function = %request.function,
            caller = %request.caller,
            trace_id = %trace_id.0,
            tenant_id = tracing::field::Empty,
        );
        if let Some(tenant) = request.tenant_id.as_ref() {
            span.record("tenant_id", tenant.0.as_str());
        }
        let _ = harn_vm::observability::otel::set_span_parent(
            &span,
            &trace_id,
            request.parent_span_id.as_deref(),
        );

        let started = Instant::now();
        let invocation = async {
            let value = match function.kind {
                ExportedCallableKind::Function => self.invoke_function(&request, function).await?,
                ExportedCallableKind::Pipeline => {
                    let value = self.invoke_pipeline(&request, function).await?;
                    self.prepared_tool_catalog()
                        .validate_output(&request.function, &value.0)
                        .map_err(DispatchError::Contract)?;
                    (value.0, value.1, None)
                }
            };
            Ok::<_, DispatchError>(value)
        }
        .instrument(span)
        .await;

        match invocation {
            Ok((value, printed_output, feedback)) => {
                let duration_ms = started.elapsed().as_millis();
                self.record_trust(&request, &trace_id, TrustOutcome::Success, None)
                    .await?;
                if let Some(key) = replay_key {
                    self.config
                        .replay_cache
                        .put(
                            key,
                            ReplayCacheEntry {
                                value: value.clone(),
                                printed_output: printed_output.clone(),
                                feedback: feedback.clone(),
                            },
                        )
                        .await?;
                }
                Ok(CallResponse {
                    function: request.function,
                    value,
                    printed_output,
                    feedback,
                    trace_id,
                    cached: false,
                    duration_ms,
                    dispatch: DispatchCallReceipt {
                        generation_cache_hit: Some(true),
                        queue_ms: None,
                        execution_ms: Some(duration_ms as u64),
                    },
                })
            }
            Err(error) => {
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Failure,
                    Some(error.to_string()),
                )
                .await?;
                Err(error)
            }
        }
    }

    /// Consult the rate-limit + backpressure registry for this dispatch.
    /// Returns a guard that decrements the in-flight counter on drop
    /// when the registry admits the call; returns
    /// `DispatchError::RateLimited` otherwise.
    fn check_limits(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<LimitGuard, DispatchError> {
        let Some(registry) = self.config.limit_registry.as_ref() else {
            return Ok(LimitGuard::unbounded_for_caller());
        };
        let Some(limits) = function.limits.as_ref() else {
            return Ok(LimitGuard::unbounded_for_caller());
        };
        let ctx = LimitContext {
            route: &request.function,
            tenant_id: request.tenant_id.as_ref(),
            scopes: &function.required_scopes,
        };
        match registry.check(&ctx, limits) {
            LimitDecision::Allowed(guard) => Ok(guard),
            LimitDecision::Rejected {
                scope,
                retry_after_ms,
            } => Err(DispatchError::RateLimited {
                scope: scope.as_str().to_string(),
                retry_after_ms,
            }),
        }
    }

    async fn invoke_function(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<(serde_json::Value, String, Option<String>), DispatchError> {
        let script_path = self.config.script_path.clone();
        let cancel_token = request
            .cancel_token
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let agent_session_id = request.agent_session_id.clone();
        let agent_event_sink = request_event_sink(request);
        let actor_chain = request.actor_chain.clone();
        let progress = request.progress.clone();

        let tenant_id = request.tenant_id.clone();
        let budget = function.budget.clone();
        let request_id = request.request_id.clone();
        let auth_context = request.auth_context.clone();
        let auth_principal = request.auth_principal.clone();
        let local = LocalSet::new();
        local
            .run_until(harn_vm::mcp_progress::scope_context(progress, async move {
                harn_vm::llm::scope_agent_event_sink(
                    agent_event_sink,
                    BudgetSpec::scope_dispatch(
                        budget,
                        Box::pin(async move {
                            let _event_log = install_scoped_event_log(self.event_log.clone());
                            let _session_guard = match agent_session_id.as_deref() {
                                Some(session_id) => {
                                    harn_vm::agent_sessions::open_or_create_with_actor_chain(
                                        Some(session_id.to_string()),
                                        actor_chain.clone(),
                                    )
                                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                                    Some(harn_vm::agent_sessions::enter_current_session(
                                        session_id.to_string(),
                                    ))
                                }
                                None => None,
                            };
                            let _tenant_guard = tenant_id.map(harn_vm::enter_tenant);
                            let _request_id_guard = request_id.map(harn_vm::enter_request_id);
                            let _auth_context_guard = auth_context.map(crate::enter_auth_context);
                            let _auth_principal_guard =
                                auth_principal.map(harn_vm::enter_auth_principal);

                            let mut vm = self.generation.instantiate(cancel_token);
                            self.config.vm_configurator.configure(&mut vm)?;

                            let exports = vm
                                .load_prepared_module_exports_from_source(
                                    &script_path,
                                    self.generation.source(),
                                )
                                .await
                                .map_err(classify_vm_error)?;
                            let Some(closure) = exports.get(&request.function) else {
                                return Err(DispatchError::MissingExport(format!(
                                    "function '{}' is not exported by {}",
                                    request.function,
                                    script_path.display()
                                )));
                            };
                            let mut args = inject_leading_authority(
                                &vm,
                                closure,
                                &[],
                                &format!("serve export `{}`", request.function),
                            )
                            .map_err(classify_vm_error)?;
                            let user_args = build_vm_args(&request.arguments, function)?;
                            args.extend(user_args);
                            let result = vm.call_closure_pub(closure, &args).await;

                            let (json, feedback) =
                                self.tools.classify_result(&request.function, result)?;
                            Ok((json, vm.output().to_string(), feedback))
                        }),
                    ),
                )
                .await
            }))
            .await
    }

    async fn invoke_pipeline(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<(serde_json::Value, String), DispatchError> {
        let source = self.generation.source();
        let arguments = request.arguments.clone();
        let function = function.clone();
        let script_path = self.config.script_path.clone();
        let cancel_token = request
            .cancel_token
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let agent_session_id = request.agent_session_id.clone();
        let agent_event_sink = request_event_sink(request);
        let actor_chain = request.actor_chain.clone();
        let progress = request.progress.clone();

        let tenant_id = request.tenant_id.clone();
        let budget = function.budget.clone();
        let request_id = request.request_id.clone();
        let auth_context = request.auth_context.clone();
        let auth_principal = request.auth_principal.clone();
        let local = LocalSet::new();
        local
            .run_until(harn_vm::mcp_progress::scope_context(progress, async move {
                harn_vm::llm::scope_agent_event_sink(
                    agent_event_sink,
                    BudgetSpec::scope_dispatch(
                        budget,
                        Box::pin(async move {
                            let _event_log = install_scoped_event_log(self.event_log.clone());
                            let _session_guard = match agent_session_id.as_deref() {
                                Some(session_id) => {
                                    harn_vm::agent_sessions::open_or_create_with_actor_chain(
                                        Some(session_id.to_string()),
                                        actor_chain.clone(),
                                    )
                                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                                    Some(harn_vm::agent_sessions::enter_current_session(
                                        session_id.to_string(),
                                    ))
                                }
                                None => None,
                            };
                            let _tenant_guard = tenant_id.map(harn_vm::enter_tenant);
                            let _request_id_guard = request_id.map(harn_vm::enter_request_id);
                            let _auth_context_guard = auth_context.map(crate::enter_auth_context);
                            let _auth_principal_guard =
                                auth_principal.map(harn_vm::enter_auth_principal);

                            let mut vm = self.generation.instantiate(cancel_token);
                            self.config.vm_configurator.configure(&mut vm)?;
                            let closure = vm
                                .load_module_callable_from_source(
                                    &script_path,
                                    source,
                                    &function.name,
                                )
                                .await
                                .map_err(classify_vm_error)?;
                            let closure = closure.ok_or_else(|| {
                                DispatchError::MissingExport(function.name.clone())
                            })?;
                            let mut args = inject_leading_authority(
                                &vm,
                                &closure,
                                &[],
                                &format!("serve pipeline `{}`", function.name),
                            )
                            .map_err(classify_vm_error)?;
                            let user_args = build_vm_args(&arguments, &function)?;
                            args.extend(user_args);
                            let result = vm.call_closure_pub(&closure, &args).await;

                            match result {
                                Ok(_) => {
                                    let output = vm.output().to_string();
                                    Ok((serde_json::Value::String(output.clone()), output))
                                }
                                Err(error) => {
                                    Err(self.tools.classify_failure(&function.name, error))
                                }
                            }
                        }),
                    ),
                )
                .await
            }))
            .await
    }

    async fn record_trust(
        &self,
        request: &CallRequest,
        trace_id: &TraceId,
        outcome: TrustOutcome,
        error: Option<String>,
    ) -> Result<(), DispatchError> {
        let mut record = TrustRecord::new(
            self.config.service_name.clone(),
            format!("invoke.{}", request.function),
            None,
            outcome,
            trace_id.0.clone(),
            self.config.autonomy_tier,
        );
        record
            .metadata
            .insert("adapter".to_string(), serde_json::json!(request.adapter));
        record
            .metadata
            .insert("caller".to_string(), serde_json::json!(request.caller));
        record
            .metadata
            .insert("function".to_string(), serde_json::json!(request.function));
        if let Some(actor_chain) = request.actor_chain.as_ref() {
            record.set_actor_chain(Some(actor_chain.clone()));
        }
        if let Some(tenant) = request.tenant_id.as_ref() {
            record
                .metadata
                .insert("tenant_id".to_string(), serde_json::json!(tenant.0));
        }
        if let Some(error) = error {
            record
                .metadata
                .insert("error".to_string(), serde_json::json!(error));
        }
        append_trust_record(&self.event_log, &record)
            .await
            .map(|_| ())
            .map_err(|error| {
                DispatchError::Execution(format!("failed to append trust record: {error}"))
            })
    }
}

#[cfg(test)]
mod tests;
