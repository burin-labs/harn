use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harn_vm::event_log::{
    active_event_log, install_active_event_log, install_default_for_base_dir, AnyEventLog,
};
use harn_vm::llm::vm_value_to_json;
use harn_vm::mcp_progress::ProgressContext;
use harn_vm::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use harn_vm::{ActorChain, TenantId, TraceId, Vm, VmValue};
use tokio::task::LocalSet;
use tracing::Instrument;

use crate::auth::{AuthPolicy, AuthRequest, AuthenticatedPrincipal, AuthorizationDecision};
use crate::limits::{LimitContext, LimitDecision, LimitGuard, LimitRegistry};
use crate::replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
use crate::{BudgetSpec, DispatchError, ExportCatalog, ExportedCallableKind};

struct ActiveEventLogGuard {
    previous: Option<Arc<AnyEventLog>>,
}

impl Drop for ActiveEventLogGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(log) => {
                install_active_event_log(log);
            }
            None => {
                harn_vm::event_log::reset_active_event_log();
            }
        }
    }
}

fn install_scoped_event_log(log: Arc<AnyEventLog>) -> ActiveEventLogGuard {
    let previous = active_event_log();
    install_active_event_log(log);
    ActiveEventLogGuard { previous }
}

/// Translate a VM-level error into the dispatcher's typed error.
///
/// Three signals get hoisted out of `Generic` so adapters can render
/// each correctly:
///
/// * `ErrorCategory::Cancelled` — caller-initiated cancel (HTTP 499).
/// * `ErrorCategory::BudgetExceeded` — a `@budget(...)` ceiling fired
///   (HTTP 429, `code = "budget_exceeded"`).
/// * everything else → `Execution` (HTTP 500).
fn classify_vm_error(error: harn_vm::VmError) -> DispatchError {
    let category = harn_vm::error_to_category(&error);
    let message = error.to_string();
    match category {
        harn_vm::ErrorCategory::Cancelled => DispatchError::Cancelled(message),
        harn_vm::ErrorCategory::BudgetExceeded => DispatchError::BudgetExceeded {
            category: budget_category_from_error(&error)
                .unwrap_or_else(|| "llm_cost_usd".to_string()),
            message,
        },
        _ => DispatchError::Execution(message),
    }
}

/// Best-effort attempt to recover the specific budget dimension that
/// fired (one of `llm_cost_usd`, `llm_tokens`, `mcp_calls`,
/// `pg_queries`) from a `VmError` so per-class rejection telemetry stays
/// accurate. The structured form (`VmError::Thrown(Dict)` — the
/// preflight LLM check and the mcp/pg call-count guards) carries it as
/// the `limit` field. The LLM cost/token guards raise the categorised
/// mid-call variant instead, where we disambiguate on the message.
fn budget_category_from_error(error: &harn_vm::VmError) -> Option<String> {
    match error {
        harn_vm::VmError::Thrown(harn_vm::VmValue::Dict(d)) => d
            .get("limit")
            .map(|value| value.display())
            .filter(|s| !s.is_empty()),
        harn_vm::VmError::CategorizedError { message, .. } if message.contains("LLM") => {
            if message.contains("token") {
                Some("llm_tokens".to_string())
            } else {
                Some("llm_cost_usd".to_string())
            }
        }
        _ => None,
    }
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
    /// session id and registers a sink that publishes worker updates
    /// onto the task event stream).
    pub agent_session_id: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallResponse {
    pub function: String,
    pub value: serde_json::Value,
    pub printed_output: String,
    pub trace_id: TraceId,
    pub cached: bool,
    pub duration_ms: u128,
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

pub struct DispatchCoreConfig {
    pub script_path: PathBuf,
    pub base_dir: PathBuf,
    pub service_name: String,
    pub autonomy_tier: AutonomyTier,
    pub auth_policy: AuthPolicy,
    pub replay_cache: Arc<dyn ReplayCache>,
    pub vm_configurator: Arc<dyn VmConfigurator>,
    /// Rate-limit + backpressure orchestrator. `None` short-circuits
    /// the limits check (every dispatch admitted unconditionally),
    /// matching legacy `harn-serve` behaviour. Production deployments
    /// install [`LimitRegistry::in_memory`] (single-node default) or a
    /// cluster-aware impl that wraps a remote counter.
    pub limit_registry: Option<Arc<LimitRegistry>>,
}

impl DispatchCoreConfig {
    pub fn for_script(path: impl Into<PathBuf>) -> Self {
        let script_path = path.into();
        let base_dir = script_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let service_name = script_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("harn-serve")
            .to_string();
        Self {
            script_path,
            base_dir,
            service_name,
            autonomy_tier: AutonomyTier::ActAuto,
            auth_policy: AuthPolicy::allow_all(),
            replay_cache: Arc::new(InMemoryReplayCache::new()),
            vm_configurator: Arc::new(NoopVmConfigurator),
            limit_registry: None,
        }
    }
}

pub struct DispatchCore {
    config: DispatchCoreConfig,
    catalog: ExportCatalog,
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
}

impl DispatchCore {
    pub fn new(config: DispatchCoreConfig) -> Result<Self, DispatchError> {
        let catalog = ExportCatalog::from_path(&config.script_path)?;
        let event_log = install_default_for_base_dir(&config.base_dir).map_err(|error| {
            DispatchError::Io(format!(
                "failed to initialize event log for {}: {error}",
                config.base_dir.display()
            ))
        })?;
        Ok(Self {
            config,
            catalog,
            event_log,
        })
    }

    pub fn catalog(&self) -> &ExportCatalog {
        &self.catalog
    }

    pub fn auth_policy(&self) -> &AuthPolicy {
        &self.config.auth_policy
    }

    pub(crate) fn event_log(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub async fn dispatch(&self, mut request: CallRequest) -> Result<CallResponse, DispatchError> {
        let trace_id = request.trace_id.clone().unwrap_or_default();
        let function_scopes = self
            .catalog
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

        let function = self.catalog.function(&request.function).ok_or_else(|| {
            DispatchError::MissingExport(format!(
                "function '{}' is not exported by {}",
                request.function,
                self.catalog.script_path.display()
            ))
        })?;

        // Rate-limit + backpressure gate. Held across the dispatch so
        // the in-flight counter decrements on drop (including panics).
        // Cached replies skip the gate to keep replay-cache hits free
        // and avoid double-charging buckets the original call already
        // paid for.
        let _limit_guard = self.check_limits(&request, function)?;

        let replay_key = request
            .replay_key
            .clone()
            .map(ReplayKey)
            .or_else(|| Some(self.default_replay_key(&request)));
        if let Some(key) = replay_key.as_ref() {
            if let Some(cached) = self.config.replay_cache.get(key).await? {
                return Ok(CallResponse {
                    function: request.function.clone(),
                    value: cached.value,
                    printed_output: cached.printed_output,
                    trace_id,
                    cached: true,
                    duration_ms: 0,
                });
            }
        }

        // Per-dispatch resource budget caps live on `function.budget`
        // and are installed inside `invoke_function` / `invoke_pipeline`
        // — the thread-local backing (`BudgetSpec::install`)
        // must be set on the same OS thread the VM runs on, which the
        // tokio `LocalSet` inside each invoker pins.

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
                ExportedCallableKind::Pipeline => self.invoke_pipeline(&request, function).await?,
            };
            Ok::<_, DispatchError>(value)
        }
        .instrument(span)
        .await;

        match invocation {
            Ok((value, printed_output)) => {
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
                            },
                        )
                        .await?;
                }
                Ok(CallResponse {
                    function: request.function,
                    value,
                    printed_output,
                    trace_id,
                    cached: false,
                    duration_ms,
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

    fn default_replay_key(&self, request: &CallRequest) -> ReplayKey {
        let args = match &request.arguments {
            CallArguments::Named(values) => serde_json::Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            CallArguments::Positional(values) => serde_json::Value::Array(values.clone()),
        };
        let key = serde_json::json!({
            "adapter": &request.adapter,
            "function": &request.function,
            "actor_chain": request.actor_chain.as_ref().map(ActorChain::to_json_value),
            "arguments": harn_vm::mcp_file_upload::redact_data_uris_for_logs(&args),
        });
        ReplayKey(serde_json::to_string(&key).unwrap_or_default())
    }

    async fn invoke_function(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<(serde_json::Value, String), DispatchError> {
        let source = tokio::fs::read_to_string(&self.config.script_path)
            .await
            .map_err(|error| {
                DispatchError::Io(format!(
                    "failed to read {}: {error}",
                    self.config.script_path.display()
                ))
            })?;
        let script_path = self.config.script_path.clone();
        let cancel_token = request
            .cancel_token
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let agent_session_id = request.agent_session_id.clone();
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
                let _event_log = install_scoped_event_log(self.event_log.clone());
                let _session_guard = agent_session_id.as_deref().map(|session_id| {
                    harn_vm::agent_sessions::open_or_create_with_actor_chain(
                        Some(session_id.to_string()),
                        actor_chain.clone(),
                    );
                    harn_vm::agent_sessions::enter_current_session(session_id.to_string())
                });
                let _tenant_guard = tenant_id.map(harn_vm::enter_tenant);
                let _budget_guard = budget.as_ref().and_then(BudgetSpec::install);
                let _request_id_guard = request_id.map(harn_vm::enter_request_id);
                let _auth_context_guard = auth_context.map(crate::enter_auth_context);
                let _auth_principal_guard = auth_principal.map(harn_vm::enter_auth_principal);

                let mut vm = Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let store_base = script_path.parent().unwrap_or(Path::new("."));
                harn_vm::register_store_builtins(&mut vm, store_base);
                harn_vm::register_metadata_builtins(&mut vm, store_base);
                vm.set_source_info(&script_path.display().to_string(), &source);
                vm.set_source_dir(store_base);
                vm.install_cancel_token(cancel_token);
                vm.set_harness(harn_vm::Harness::real());
                self.config.vm_configurator.configure(&mut vm)?;

                let exports = vm
                    .load_module_exports(&script_path)
                    .await
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                let Some(closure) = exports.get(&request.function) else {
                    return Err(DispatchError::MissingExport(format!(
                        "function '{}' is not exported by {}",
                        request.function,
                        script_path.display()
                    )));
                };
                let args = build_vm_args(&request.arguments, function, &vm)?;
                let result = vm.call_closure_pub(closure, &args).await;

                match result {
                    Ok(value) => Ok((vm_value_to_json(&value), vm.output().to_string())),
                    Err(error) => Err(classify_vm_error(error)),
                }
            }))
            .await
    }

    async fn invoke_pipeline(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<(serde_json::Value, String), DispatchError> {
        let source = tokio::fs::read_to_string(&self.config.script_path)
            .await
            .map_err(|error| {
                DispatchError::Io(format!(
                    "failed to read {}: {error}",
                    self.config.script_path.display()
                ))
            })?;
        let program = harn_parser::parse_source(&source).map_err(|error| {
            DispatchError::Validation(format!(
                "failed to parse {}: {error}",
                self.config.script_path.display()
            ))
        })?;
        let chunk = Arc::new(
            harn_vm::Compiler::new()
                .compile_named(&program, &function.name)
                .map_err(|error| DispatchError::Validation(format!("compile error: {error}")))?,
        );
        let globals = build_pipeline_globals(&request.arguments, function)?;
        let script_path = self.config.script_path.clone();
        let cancel_token = request
            .cancel_token
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let agent_session_id = request.agent_session_id.clone();
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
                let _event_log = install_scoped_event_log(self.event_log.clone());
                let _session_guard = agent_session_id.as_deref().map(|session_id| {
                    harn_vm::agent_sessions::open_or_create_with_actor_chain(
                        Some(session_id.to_string()),
                        actor_chain.clone(),
                    );
                    harn_vm::agent_sessions::enter_current_session(session_id.to_string())
                });
                let _tenant_guard = tenant_id.map(harn_vm::enter_tenant);
                let _budget_guard = budget.as_ref().and_then(BudgetSpec::install);
                let _request_id_guard = request_id.map(harn_vm::enter_request_id);
                let _auth_context_guard = auth_context.map(crate::enter_auth_context);
                let _auth_principal_guard = auth_principal.map(harn_vm::enter_auth_principal);

                let mut vm = Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let store_base = script_path.parent().unwrap_or(Path::new("."));
                harn_vm::register_store_builtins(&mut vm, store_base);
                harn_vm::register_metadata_builtins(&mut vm, store_base);
                vm.set_source_info(&script_path.display().to_string(), &source);
                vm.set_source_dir(store_base);
                vm.install_cancel_token(cancel_token);
                vm.set_harness(harn_vm::Harness::real());
                self.config.vm_configurator.configure(&mut vm)?;
                for (name, value) in globals {
                    vm.set_global(&name, value);
                }

                let result = vm.execute_arc(Arc::clone(&chunk)).await;

                match result {
                    Ok(_) => {
                        let output = vm.output().to_string();
                        Ok((serde_json::Value::String(output.clone()), output))
                    }
                    Err(error) => Err(classify_vm_error(error)),
                }
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

fn build_vm_args(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
    vm: &Vm,
) -> Result<Vec<VmValue>, DispatchError> {
    let mut params = function.params.as_slice();
    let mut prefix = Vec::new();
    // Exported `pub fn foo(harness: Harness, ...)` opts the function
    // into the runtime-supplied capability handle the same way
    // top-level `fn main(harness: Harness)` does. The dispatch surface
    // hands JSON in, so the host fills the slot from
    // `vm.set_harness(...)` instead of asking the caller to encode a
    // Harness through CallArguments. Only the first positional slot
    // qualifies (matches the language convention).
    if first_param_is_harness(function) {
        let harness = vm
            .global("harness")
            .ok_or_else(|| {
                DispatchError::Execution(
                    "Harness handle not installed; DispatchCore must call vm.set_harness() before invoking exported functions that take a harness param"
                        .to_string(),
                )
            })?
            .clone();
        prefix.push(harness);
        params = &params[1..];
    }

    let rest = match arguments {
        CallArguments::Positional(values) => {
            values.iter().map(json_to_vm_value).collect::<Vec<_>>()
        }
        CallArguments::Named(values) => {
            let mut args = Vec::new();
            let mut saw_gap = false;
            for param in params {
                let value = values.get(&param.name);
                match value {
                    Some(value) => {
                        if saw_gap {
                            return Err(DispatchError::Validation(format!(
                                "named arguments for '{}' skipped '{}' before later arguments",
                                function.name, param.name
                            )));
                        }
                        args.push(json_to_vm_value(value));
                    }
                    None if param.has_default => {
                        saw_gap = true;
                    }
                    None => {
                        return Err(DispatchError::Validation(format!(
                            "missing required argument '{}' for '{}'",
                            param.name, function.name
                        )));
                    }
                }
            }
            trim_trailing_defaults(args)
        }
    };

    prefix.extend(rest);
    Ok(prefix)
}

/// `true` when the first exported param is the canonical `harness`
/// capability handle slot. Type annotation is optional (most pubs use
/// untyped `harness` in stdlib) so we only check the name; the
/// typechecker still enforces the `Harness` type in declared signatures.
fn first_param_is_harness(function: &crate::ExportedFunction) -> bool {
    function
        .params
        .first()
        .map(|param| param.name == "harness")
        .unwrap_or(false)
}

fn build_pipeline_globals(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
) -> Result<harn_vm::value::DictMap, DispatchError> {
    let mut globals = harn_vm::value::DictMap::new();
    match arguments {
        CallArguments::Positional(values) => {
            for (index, param) in function.params.iter().enumerate() {
                match values.get(index) {
                    Some(value) => {
                        globals.insert(
                            harn_vm::value::intern_key(&param.name),
                            json_to_vm_value(value),
                        );
                    }
                    None if param.has_default => {}
                    None => {
                        return Err(DispatchError::Validation(format!(
                            "missing required argument '{}' for '{}'",
                            param.name, function.name
                        )));
                    }
                }
            }
        }
        CallArguments::Named(values) => {
            for param in &function.params {
                match values.get(&param.name) {
                    Some(value) => {
                        globals.insert(
                            harn_vm::value::intern_key(&param.name),
                            json_to_vm_value(value),
                        );
                    }
                    None if param.has_default => {}
                    None => {
                        return Err(DispatchError::Validation(format!(
                            "missing required argument '{}' for '{}'",
                            param.name, function.name
                        )));
                    }
                }
            }
        }
    }
    Ok(globals)
}

fn trim_trailing_defaults(mut args: Vec<VmValue>) -> Vec<VmValue> {
    let mut tail = VecDeque::from(args);
    while matches!(tail.back(), Some(VmValue::Nil)) {
        tail.pop_back();
    }
    args = tail.into_iter().collect();
    args
}

fn json_to_vm_value(value: &serde_json::Value) -> VmValue {
    match value {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(value) => VmValue::Bool(*value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(VmValue::Int)
            .or_else(|| value.as_f64().map(VmValue::Float))
            .unwrap_or(VmValue::Nil),
        serde_json::Value::String(value) => VmValue::String(arcstr::ArcStr::from(value.as_str())),
        serde_json::Value::Array(items) => VmValue::List(Arc::new(
            items.iter().map(json_to_vm_value).collect::<Vec<_>>(),
        )),
        serde_json::Value::Object(map) => VmValue::dict(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_vm_value(value)))
                .collect::<harn_vm::value::DictMap>(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_executes_exported_function() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn greet(name: string) -> string {
  return name
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let response = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "greet".to_string(),
                arguments: CallArguments::Named(BTreeMap::from([(
                    "name".to_string(),
                    serde_json::json!("alice"),
                )])),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: None,
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("alice"));
        assert!(!response.cached);
    }

    #[tokio::test]
    async fn dispatch_executes_legacy_pipeline_when_no_public_exports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pipeline default(task) {
  __io_println(json_stringify({task: task}))
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let response = core
            .dispatch(CallRequest {
                adapter: "a2a".to_string(),
                function: "default".to_string(),
                arguments: CallArguments::Named(BTreeMap::from([(
                    "task".to_string(),
                    serde_json::json!("payload"),
                )])),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: None,
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(
            response.value,
            serde_json::json!("{\"task\":\"payload\"}\n")
        );
        assert_eq!(response.printed_output, "{\"task\":\"payload\"}\n");
    }

    #[tokio::test]
    async fn dispatch_uses_replay_cache_before_reinvoking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return "fresh"
}
"#,
        )
        .expect("write script");

        let cache = Arc::new(InMemoryReplayCache::new());
        cache
            .put(
                ReplayKey("fixed-key".to_string()),
                ReplayCacheEntry {
                    value: serde_json::json!("cached"),
                    printed_output: String::new(),
                },
            )
            .await
            .expect("seed cache");

        let mut config = DispatchCoreConfig::for_script(&script);
        config.replay_cache = cache;
        let core = DispatchCore::new(config).expect("core");
        let response = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "greet".to_string(),
                arguments: CallArguments::Named(BTreeMap::from([(
                    "name".to_string(),
                    serde_json::json!("alice"),
                )])),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: Some("fixed-key".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("cached"));
        assert!(response.cached);
    }

    #[test]
    fn default_replay_key_redacts_data_uri_payloads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn inspect(upload: string) -> string {
  return upload
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let request = |payload: serde_json::Value| CallRequest {
            adapter: "mcp".to_string(),
            function: "inspect".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([("upload".to_string(), payload)])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        };

        let first = core
            .default_replay_key(&request(serde_json::json!(
                "data:text/plain;base64,aGVsbG8="
            )))
            .0;
        let second = core
            .default_replay_key(&request(serde_json::json!(
                "data:text/plain;base64,d29ybGQ="
            )))
            .0;

        assert!(first.contains("data:text/plain;redacted;sha256="));
        assert!(!first.contains("aGVsbG8="));
        assert!(!second.contains("d29ybGQ="));
        assert_ne!(first, second);
    }

    #[test]
    fn default_replay_key_includes_actor_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn inspect(value: string) -> string {
  return value
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let request = |actor_chain: ActorChain| CallRequest {
            adapter: "mcp".to_string(),
            function: "inspect".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "value".to_string(),
                serde_json::json!("same"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            actor_chain: Some(actor_chain),
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        };

        let first = core
            .default_replay_key(&request(
                ActorChain::new("user:kenneth").pushed("agent:root"),
            ))
            .0;
        let second = core
            .default_replay_key(&request(ActorChain::new("user:maya").pushed("agent:root")))
            .0;

        assert_ne!(first, second);
        assert!(first.contains(r#""sub":"user:kenneth""#));
        assert!(second.contains(r#""sub":"user:maya""#));
    }

    #[tokio::test]
    async fn dispatch_records_trust_graph_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn greet(name: string) -> string {
  return name
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let response = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "greet".to_string(),
                arguments: CallArguments::Named(BTreeMap::from([(
                    "name".to_string(),
                    serde_json::json!("alice"),
                )])),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: Some("trust-key".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        let records =
            harn_vm::query_trust_records(&core.event_log, &harn_vm::TrustQueryFilters::default())
                .await
                .expect("records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].trace_id, response.trace_id.0);
        assert_eq!(records[0].metadata["adapter"], "mcp");
    }

    #[tokio::test]
    async fn dispatch_propagates_cancelled_execution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn spin() -> string {
  while true {
    if is_cancelled() {
      return "stopped"
    }
  }
}
"#,
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let cancel_token = Arc::new(AtomicBool::new(true));
        let response = core
            .dispatch(CallRequest {
                adapter: "acp".to_string(),
                function: "spin".to_string(),
                arguments: CallArguments::Positional(Vec::new()),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: Some("cancel-key".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: Some(cancel_token),
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("stopped"));
    }

    /// `.harn` callees see the tenant the host bound via
    /// `AuthPolicy` — the `ApiKeyEntry` was configured with a tenant,
    /// the principal carries it forward, and `DispatchCore::dispatch`
    /// installs the [`harn_vm::enter_tenant`] guard so the script's
    /// `harness.tenant.id()` returns the same id end-to-end.
    #[tokio::test]
    async fn dispatch_threads_api_key_tenant_into_harness_and_trust_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
        )
        .expect("write script");

        let mut config = DispatchCoreConfig::for_script(&script);
        config.auth_policy = crate::auth::AuthPolicy {
            methods: vec![crate::auth::AuthMethodConfig::ApiKey(
                crate::auth::ApiKeyAuthConfig {
                    keys: vec![
                        crate::auth::ApiKeyEntry::new("alice-key", []).with_tenant("acme-corp")
                    ],
                },
            )],
            mcp_allowlist: None,
        };
        let core = DispatchCore::new(config).expect("core");

        let response = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "whoami".to_string(),
                arguments: CallArguments::Positional(Vec::new()),
                auth: AuthRequest {
                    headers: BTreeMap::from([(
                        "authorization".to_string(),
                        "Bearer alice-key".to_string(),
                    )]),
                    ..AuthRequest::default()
                },
                caller: "tester".to_string(),
                replay_key: Some("tenant-whoami".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("acme-corp"));

        let records =
            harn_vm::query_trust_records(&core.event_log, &harn_vm::TrustQueryFilters::default())
                .await
                .expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metadata["tenant_id"], "acme-corp");
    }

    #[tokio::test]
    async fn dispatch_threads_actor_chain_into_agent_session() {
        harn_vm::reset_thread_local_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn actor_chain() -> any {
  return agent_session_actor_chain()
}
",
        )
        .expect("write script");

        let mut config = DispatchCoreConfig::for_script(&script);
        config.auth_policy = crate::auth::AuthPolicy {
            methods: vec![crate::auth::AuthMethodConfig::ApiKey(
                crate::auth::ApiKeyAuthConfig {
                    keys: vec![crate::auth::ApiKeyEntry::new("actor-key", [])],
                },
            )],
            mcp_allowlist: None,
        };
        let core = DispatchCore::new(config).expect("core");

        let response = core
            .dispatch(CallRequest {
                adapter: "a2a".to_string(),
                function: "actor_chain".to_string(),
                arguments: CallArguments::Positional(Vec::new()),
                auth: AuthRequest {
                    headers: BTreeMap::from([(
                        "authorization".to_string(),
                        "Bearer actor-key".to_string(),
                    )]),
                    ..AuthRequest::default()
                },
                caller: "tester".to_string(),
                replay_key: Some("actor-chain".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: Some("dispatch-actor-chain".to_string()),
                actor_chain: None,
                actor_chain_hop: Some("agent:merge-captain".to_string()),
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        let expected = serde_json::json!({
            "sub": "api-key",
            "act": {
                "sub": "agent:merge-captain"
            }
        });
        assert_eq!(response.value, expected);
        assert_eq!(
            harn_vm::agent_sessions::actor_chain("dispatch-actor-chain")
                .map(|chain| chain.to_json_value()),
            Some(expected)
        );
    }

    /// `harness.tenant.id()` raises a typed runtime error (categorized
    /// as `auth`) when the dispatch was not bound to a tenant. The
    /// dispatch surface then maps it through the standard `Execution`
    /// error envelope so callers see the canonical message.
    #[tokio::test]
    async fn dispatch_missing_tenant_raises_typed_runtime_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
        )
        .expect("write script");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let error = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "whoami".to_string(),
                arguments: CallArguments::Positional(Vec::new()),
                auth: AuthRequest::default(),
                caller: "tester".to_string(),
                replay_key: Some("missing-tenant".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: None,
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect_err("missing tenant should error");

        let message = error.message();
        assert!(
            message.contains("harness.tenant.id()"),
            "expected typed tenant error, got: {message}"
        );
    }

    /// `CallRequest.tenant_id` overrides the principal-supplied tenant
    /// — covers the case where an upstream gateway already resolved
    /// tenancy out-of-band and hands the answer to harn-serve.
    #[tokio::test]
    async fn dispatch_request_tenant_overrides_principal_tenant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
        )
        .expect("write script");

        let mut config = DispatchCoreConfig::for_script(&script);
        config.auth_policy = crate::auth::AuthPolicy {
            methods: vec![crate::auth::AuthMethodConfig::ApiKey(
                crate::auth::ApiKeyAuthConfig {
                    keys: vec![
                        crate::auth::ApiKeyEntry::new("key", []).with_tenant("principal-tenant")
                    ],
                },
            )],
            mcp_allowlist: None,
        };
        let core = DispatchCore::new(config).expect("core");

        let response = core
            .dispatch(CallRequest {
                adapter: "mcp".to_string(),
                function: "whoami".to_string(),
                arguments: CallArguments::Positional(Vec::new()),
                auth: AuthRequest {
                    headers: BTreeMap::from([(
                        "authorization".to_string(),
                        "Bearer key".to_string(),
                    )]),
                    ..AuthRequest::default()
                },
                caller: "tester".to_string(),
                replay_key: Some("override-tenant".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
                agent_session_id: None,
                actor_chain: None,
                actor_chain_hop: None,
                progress: None,
                tenant_id: Some(harn_vm::TenantId::new("override-tenant")),
                request_id: None,
                auth_context: None,
                auth_principal: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("override-tenant"));
    }

    #[test]
    fn budget_category_recovers_every_dimension() {
        // Structured guards (mcp/pg call counts, LLM preflight) carry the
        // dimension on the `limit` field.
        let structured = |limit: &str| {
            harn_vm::VmError::Thrown(harn_vm::VmValue::dict(std::collections::BTreeMap::from([
                (
                    "category".to_string(),
                    harn_vm::VmValue::String(arcstr::ArcStr::from("budget_exceeded")),
                ),
                (
                    "limit".to_string(),
                    harn_vm::VmValue::String(arcstr::ArcStr::from(limit)),
                ),
            ])))
        };
        assert_eq!(
            budget_category_from_error(&structured("mcp_calls")).as_deref(),
            Some("mcp_calls"),
        );
        assert_eq!(
            budget_category_from_error(&structured("pg_queries")).as_deref(),
            Some("pg_queries"),
        );

        // LLM cost/token mid-call exhaustion raises the categorised
        // variant; the message disambiguates cost from tokens so the
        // per-class telemetry is accurate.
        let categorized = |message: &str| harn_vm::VmError::CategorizedError {
            message: message.to_string(),
            category: harn_vm::ErrorCategory::BudgetExceeded,
        };
        assert_eq!(
            budget_category_from_error(&categorized("LLM budget exceeded: spent $0.01 of $0.00"))
                .as_deref(),
            Some("llm_cost_usd"),
        );
        assert_eq!(
            budget_category_from_error(&categorized(
                "LLM token budget exceeded: spent 11 of 10 tokens"
            ))
            .as_deref(),
            Some("llm_tokens"),
        );
    }
}
