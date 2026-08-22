capability_method!(
    llm_catalog,
    "harness.llm.catalog",
    ["llm.read@const=catalog"],
    harn_builtin_meta::signatures::LLM_CATALOG.with_name("__cap_llm_catalog"),
    "Read the model catalog."
);
capability_method!(
    llm_catalog_refresh,
    "harness.llm.catalog_refresh",
    ["llm.mutate@const=catalog"],
    harn_builtin_meta::signatures::LLM_CATALOG_REFRESH.with_name("__cap_llm_catalog_refresh"),
    "Refresh and read the model catalog."
);
capability_method!(
    llm_providers,
    "harness.llm.providers",
    ["llm.read@const=providers"],
    harn_builtin_meta::signatures::LLM_PROVIDER_STATUS.with_name("__cap_llm_providers"),
    "Read provider status."
);
capability_method!(
    llm_call,
    "harness.llm.call",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::LLM_CALL.with_name("__cap_llm_call"),
    "Execute one routed model call."
);
capability_method!(
    llm_self_certainty,
    "harness.llm.self_certainty",
    ["llm.write@arg1.provider", "llm.write@arg1.model"],
    "__cap_llm_self_certainty(text_or_result: string|dict, options?: dict|nil) -> float",
    "Measure certainty from supplied log probabilities or one model call."
);
capability_method!(
    llm_call_safe,
    "harness.llm.call_safe",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::LLM_CALL_SAFE.with_name("__cap_llm_call_safe"),
    "Execute one routed model call and return a non-throwing envelope."
);
capability_method!(
    llm_call_structured,
    "harness.llm.call_structured",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED.with_name("__cap_llm_call_structured"),
    "Execute a schema-constrained model call."
);
capability_method!(
    llm_call_structured_safe,
    "harness.llm.call_structured_safe",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED_SAFE
        .with_name("__cap_llm_call_structured_safe"),
    "Execute a schema-constrained model call and return a non-throwing envelope."
);
capability_method!(
    llm_call_structured_result,
    "harness.llm.call_structured_result",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED_RESULT
        .with_name("__cap_llm_call_structured_result"),
    "Execute a schema-constrained model call and return its diagnostic envelope."
);
capability_method!(
    llm_schema_recover,
    "harness.llm.recover_schema",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    harn_builtin_meta::signatures::SCHEMA_RECOVER.with_name("__cap_llm_recover_schema"),
    "Recover malformed structured output, optionally using model repair."
);
capability_method!(
    llm_completion,
    "harness.llm.completion",
    ["llm.write@arg3.provider", "llm.write@arg3.model"],
    harn_builtin_meta::signatures::LLM_COMPLETION.with_name("__cap_llm_completion"),
    "Execute a fill-in-the-middle model completion."
);
capability_method!(
    llm_stream,
    "harness.llm.stream",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    "__cap_llm_stream(prompt: string, system?: string, options?: dict) -> channel",
    "Execute a channel-based streaming model request."
);
capability_method!(
    llm_with_rate_limit,
    "harness.llm.with_rate_limit",
    ["state.mutate@arg0", "llm.write@arg0"],
    "__cap_llm_with_rate_limit(provider: string, callback: closure, options?: dict) -> any",
    "Run a closure under the provider rate limiter."
);
capability_method!(
    llm_stream_call,
    "harness.llm.stream_call",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    "__cap_llm_stream_call(prompt: string, system?: string, options?: dict) -> stream",
    "Execute one routed streaming model call."
);
capability_method!(
    llm_mock_clear,
    "harness.llm.mock_clear",
    ["state.mutate@const=llm-fixture"],
    "__cap_llm_mock_clear() -> nil",
    "Clear the current LLM fixture queue and call log."
);
capability_method!(
    llm_mock_enqueue,
    "harness.llm.mock_enqueue",
    ["state.write@const=llm-fixture"],
    "__cap_llm_mock_enqueue(response: dict) -> nil",
    "Append one response to the current LLM fixture queue."
);
capability_method!(
    llm_mock_calls,
    "harness.llm.mock_calls",
    ["state.read@const=llm-fixture"],
    "__cap_llm_mock_calls() -> list",
    "Read calls captured by the current LLM fixture."
);
capability_method!(
    llm_mock_snapshot,
    "harness.llm.mock_snapshot",
    ["state.read@const=llm-fixture"],
    "__cap_llm_mock_snapshot() -> dict",
    "Snapshot the current LLM fixture queues and receipts."
);
capability_method!(
    llm_mock_push_scope,
    "harness.llm.mock_push_scope",
    ["state.mutate@const=llm-fixture"],
    "__cap_llm_mock_push_scope() -> nil",
    "Push an isolated LLM fixture scope."
);
capability_method!(
    llm_mock_pop_scope,
    "harness.llm.mock_pop_scope",
    ["state.mutate@const=llm-fixture"],
    "__cap_llm_mock_pop_scope() -> nil",
    "Restore the enclosing LLM fixture scope."
);
capability_method!(
    llm_upload_file,
    "harness.llm.upload_file",
    [
        "fs.read@arg0",
        "network.write@arg1",
        "state.mutate@const=llm-file-upload-cache"
    ],
    "__cap_llm_upload_file(path: string, provider: string) -> string",
    "Upload a local file to a model provider's reusable Files API."
);
capability_method!(
    llm_session_cost,
    "harness.llm.session_cost",
    ["state.observe@const=llm-cost-ledger"],
    "__cap_llm_session_cost() -> dict",
    "Read accumulated LLM usage and cost for the current execution."
);
capability_method!(
    llm_budget,
    "harness.llm.budget",
    ["state.mutate@const=llm-cost-budget"],
    "__cap_llm_budget(max_cost: float | int) -> nil",
    "Set the LLM cost ceiling for the current execution."
);
capability_method!(
    llm_budget_remaining,
    "harness.llm.budget_remaining",
    ["state.observe@const=llm-cost-budget"],
    "__cap_llm_budget_remaining() -> float?",
    "Read the remaining LLM cost budget for the current execution."
);

capability_method!(
    tenant_id,
    "harness.tenant.id",
    ["host.read@const=tenant"],
    "__cap_tenant_id() -> string",
    "Read the required tenant identifier."
);
capability_method!(
    tenant_try_id,
    "harness.tenant.try_id",
    ["host.read@const=tenant"],
    "__cap_tenant_try_id() -> string?",
    "Read the optional tenant identifier."
);

capability_method!(
    auth_is_authenticated,
    "harness.auth.is_authenticated",
    ["host.read@const=auth"],
    "__cap_auth_is_authenticated() -> bool",
    "Test whether the request is authenticated."
);
capability_method!(
    auth_subject,
    "harness.auth.subject",
    ["host.read@const=auth"],
    "__cap_auth_subject() -> string",
    "Read the required authenticated subject."
);
capability_method!(
    auth_try_subject,
    "harness.auth.try_subject",
    ["host.read@const=auth"],
    "__cap_auth_try_subject() -> string?",
    "Read the optional authenticated subject."
);
capability_method!(
    auth_scheme,
    "harness.auth.scheme",
    ["host.read@const=auth"],
    "__cap_auth_scheme() -> string",
    "Read the required authentication scheme."
);
capability_method!(
    auth_try_scheme,
    "harness.auth.try_scheme",
    ["host.read@const=auth"],
    "__cap_auth_try_scheme() -> string?",
    "Read the optional authentication scheme."
);
capability_method!(
    auth_kind,
    "harness.auth.kind",
    ["host.read@const=auth"],
    "__cap_auth_kind() -> string",
    "Read the authentication kind."
);
capability_method!(
    auth_scopes,
    "harness.auth.scopes",
    ["host.read@const=auth"],
    "__cap_auth_scopes() -> list",
    "Read authenticated scopes."
);
capability_method!(
    auth_has_scope,
    "harness.auth.has_scope",
    ["host.read@const=auth"],
    "__cap_auth_has_scope(scope: string) -> bool",
    "Test an authenticated scope."
);
capability_method!(
    auth_oauth_storage_memory,
    "harness.auth.oauth_storage_memory",
    ["state.mutate@const=oauth-storage"],
    "__cap_auth_oauth_storage_memory() -> dict",
    "Create an in-memory OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_file,
    "harness.auth.oauth_storage_file",
    ["state.mutate@arg0"],
    "__cap_auth_oauth_storage_file(path: string, encryption_key: any) -> dict",
    "Create an encrypted file-backed OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_cloud,
    "harness.auth.oauth_storage_cloud",
    ["host.read@arg0"],
    "__cap_auth_oauth_storage_cloud(scope: string) -> dict",
    "Create a host-backed OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_get,
    "harness.auth.oauth_storage_get",
    ["state.read@dynamic", "fs.read@dynamic", "host.read@dynamic"],
    "__cap_auth_oauth_storage_get(store: dict, key: string) -> any",
    "Read from an OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_set,
    "harness.auth.oauth_storage_set",
    [
        "state.write@dynamic",
        "fs.write@dynamic",
        "host.write@dynamic"
    ],
    "__cap_auth_oauth_storage_set(store: dict, key: string, token: dict, ttl_seconds?: int) -> nil",
    "Write to an OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_delete,
    "harness.auth.oauth_storage_delete",
    [
        "state.mutate@dynamic",
        "fs.mutate@dynamic",
        "host.mutate@dynamic"
    ],
    "__cap_auth_oauth_storage_delete(store: dict, key: string) -> nil",
    "Delete from an OAuth token store handle."
);
capability_method!(
    auth_oauth_storage_with_refresh_lock,
    "harness.auth.oauth_storage_with_refresh_lock",
    ["state.mutate@dynamic", "host.mutate@dynamic"],
    "__cap_auth_oauth_storage_with_refresh_lock(store: dict, key: string, body: closure) -> any",
    "Run a closure under an OAuth refresh lock."
);
capability_method!(
    auth_oauth_registration_store,
    "harness.auth.oauth_registration_store",
    ["state.mutate@const=oauth-registration"],
    "__cap_auth_oauth_registration_store() -> dict",
    "Create an in-memory OAuth dynamic-registration store."
);
capability_method!(
    auth_oauth_register_client,
    "harness.auth.oauth_register_client",
    ["state.write@arg0"],
    "__cap_auth_oauth_register_client(store: dict, metadata: dict) -> dict",
    "Register a client in a dynamic-registration store."
);
capability_method!(
    auth_oauth_registered_client,
    "harness.auth.oauth_registered_client",
    ["state.read@arg0"],
    "__cap_auth_oauth_registered_client(store: dict, client_id: string) -> dict",
    "Read a registered OAuth client."
);
capability_method!(
    auth_oauth_registered_clients,
    "harness.auth.oauth_registered_clients",
    ["state.read@arg0"],
    "__cap_auth_oauth_registered_clients(store: dict) -> list",
    "List registered OAuth clients."
);

capability_method!(
    obs_span,
    "harness.obs.span",
    ["observability.observe@arg0"],
    "__cap_obs_span(name: string, attributes?: dict, body?: closure) -> any",
    "Run a closure inside an observability span."
);
capability_method!(
    obs_start_span,
    "harness.obs.start_span",
    ["observability.observe@arg0"],
    "__cap_obs_start_span(name: string, attributes?: dict) -> any",
    "Start an observability span."
);
capability_method!(
    obs_end_span,
    "harness.obs.end_span",
    ["observability.observe@dynamic"],
    "__cap_obs_end_span(span: any) -> nil",
    "End an observability span."
);
capability_method!(
    obs_log,
    "harness.obs.log",
    ["observability.write@arg0"],
    "__cap_obs_log(message: string, level?: string, fields?: dict) -> any",
    "Emit a structured log."
);
capability_method!(
    obs_log_debug,
    "harness.obs.log_debug",
    ["observability.write@const=log"],
    "__cap_obs_log_debug(message: any, fields?: dict) -> nil",
    "Emit a debug log line."
);
capability_method!(
    obs_log_info,
    "harness.obs.log_info",
    ["observability.write@const=log"],
    "__cap_obs_log_info(message: any, fields?: dict) -> nil",
    "Emit an info log line."
);
capability_method!(
    obs_log_warn,
    "harness.obs.log_warn",
    ["observability.write@const=log"],
    "__cap_obs_log_warn(message: any, fields?: dict) -> nil",
    "Emit a warning log line."
);
capability_method!(
    obs_log_error,
    "harness.obs.log_error",
    ["observability.write@const=log"],
    "__cap_obs_log_error(message: any, fields?: dict) -> nil",
    "Emit an error log line."
);
capability_method!(
    obs_set_level,
    "harness.obs.set_level",
    ["observability.mutate@const=log-level"],
    "__cap_obs_set_level(level: string) -> nil",
    "Set the minimum structured log level."
);
capability_method!(
    obs_log_json,
    "harness.obs.log_json",
    ["observability.write@const=log"],
    "__cap_obs_log_json(key: string, value?: any) -> nil",
    "Emit a structured JSON log line."
);
capability_method!(
    obs_counter,
    "harness.obs.counter",
    ["observability.write@arg0"],
    "__cap_obs_counter(name: string, value?: number, attributes?: dict) -> any",
    "Record a counter."
);
capability_method!(
    obs_histogram,
    "harness.obs.histogram",
    ["observability.write@arg0"],
    "__cap_obs_histogram(name: string, value: number, attributes?: dict) -> any",
    "Record a histogram value."
);
capability_method!(
    obs_gauge,
    "harness.obs.gauge",
    ["observability.write@arg0"],
    "__cap_obs_gauge(name: string, value: number, attributes?: dict) -> any",
    "Record a gauge value."
);
capability_method!(
    obs_request_id,
    "harness.obs.request_id",
    ["observability.read@const=request"],
    "__cap_obs_request_id() -> string",
    "Read the current request identifier."
);
capability_method!(
    obs_configure,
    "harness.obs.configure",
    ["observability.mutate@const=configuration"],
    "__cap_obs_configure(config?: dict) -> nil",
    "Configure observability routing for this run."
);
capability_method!(
    obs_auto_backend,
    "harness.obs.auto_backend",
    [
        "env.read@const=observability",
        "observability.read@const=configuration"
    ],
    "__cap_obs_auto_backend() -> dict",
    "Resolve the automatically selected observability backend."
);
capability_method!(
    obs_emit,
    "harness.obs.emit",
    ["observability.write@dynamic"],
    "__cap_obs_emit(record: dict) -> list",
    "Emit an arbitrary structured observation."
);
capability_method!(
    obs_events,
    "harness.obs.events",
    ["observability.read@const=event-buffer"],
    "__cap_obs_events() -> list",
    "Read captured observability events."
);
capability_method!(
    obs_events_take,
    "harness.obs.events_take",
    ["observability.mutate@const=event-buffer"],
    "__cap_obs_events_take() -> list",
    "Drain captured observability events."
);
capability_method!(
    obs_reset,
    "harness.obs.reset",
    ["observability.mutate@const=state"],
    "__cap_obs_reset() -> nil",
    "Reset observability configuration and captured state."
);

capability_method!(
    verdict_issue,
    "harness.verdict.issue",
    ["host.write@const=verdict"],
    "__cap_verdict_issue(result: any) -> any",
    "Issue an opaque verdict receipt for a real test result."
);
capability_method!(
    verdict_same_run,
    "harness.verdict.same_run",
    ["host.read@const=verdict"],
    "__cap_verdict_same_run(...receipts: any) -> bool",
    "Test whether verdict receipts belong to the active run."
);

// Agent sessions are durable orchestration state. The `HarnessAgent` handle is
// the sole language-level authority for creating, observing, and mutating that
// state; the `__host_agent_*` builtins are private implementation details.
capability_method!(
    agent_state_init,
    "harness.agent.state_init",
    ["fs.mutate@arg0"],
    "__cap_agent_state_init(root: string, options?: dict) -> resource",
    "Create or reopen a durable agent-state namespace."
);
capability_method!(
    agent_state_resume,
    "harness.agent.state_resume",
    ["fs.read@arg0", "fs.mutate@arg0"],
    "__cap_agent_state_resume(root: string, session_id: string, options?: dict) -> resource",
    "Resume an existing durable agent-state namespace."
);
capability_method!(
    agent_state_write,
    "harness.agent.state_write",
    ["state.write@arg0"],
    "__cap_agent_state_write(handle: resource, key: string, content: string) -> nil",
    "Write one durable agent-state entry."
);
capability_method!(
    agent_state_read,
    "harness.agent.state_read",
    ["state.read@arg0"],
    "__cap_agent_state_read(handle: resource, key: string) -> string?",
    "Read one durable agent-state entry."
);
capability_method!(
    agent_state_list,
    "harness.agent.state_list",
    ["state.read@arg0"],
    "__cap_agent_state_list(handle: resource) -> list",
    "List durable agent-state entries."
);
capability_method!(
    agent_state_delete,
    "harness.agent.state_delete",
    ["state.mutate@arg0"],
    "__cap_agent_state_delete(handle: resource, key: string) -> nil",
    "Delete one durable agent-state entry."
);
capability_method!(
    agent_state_handoff,
    "harness.agent.state_handoff",
    ["state.write@arg0"],
    "__cap_agent_state_handoff(handle: resource, summary: dict) -> nil",
    "Persist a typed durable handoff artifact."
);
capability_method!(
    agent_session_flush,
    "harness.agent.session_flush",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_flush(session_id: string) -> nil",
    "Flush a live agent transcript."
);
capability_method!(
    agent_emit_event,
    "harness.agent.emit_event",
    // Match `__host_agent_emit_event` (effects = []): session event ingress is a
    // runtime-owned journal write, not a model-facing state.write tool effect.
    // Claiming state.write@arg0 rejected ambient/typed emit under agent-loop
    // execution policy and aborted turns before they could reply.
    [],
    "__cap_agent_emit_event(session_id: string, event_type: string, payload: dict) -> nil",
    "Append an agent session event."
);
capability_method!(
    agent_session_init,
    "harness.agent.session_init",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_init(message: string, system?: string|nil, options?: dict|nil) -> {session_id: string, run_id: string, task: string, system: string|nil, max_iterations: int, max_verify_attempts: int, done: bool, result: any?}",
    "Initialize an agent execution session."
);
capability_method!(
    agent_session_finalize,
    "harness.agent.session_finalize",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_finalize(session_id: string, status: dict) -> dict",
    "Finalize an agent execution session."
);
capability_method!(
    agent_session_messages,
    "harness.agent.session_messages",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_messages(session_id: string) -> list",
    "Read an agent session's messages."
);
capability_method!(
    agent_session_visible_messages,
    "harness.agent.session_visible_messages",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_visible_messages(session_id: string, messages?: list|nil, append_only?: bool) -> list",
    "Project an agent session's provider-visible messages."
);
capability_method!(
    agent_session_commit_directives,
    "harness.agent.session_commit_directives",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_commit_directives(session_id: string) -> int",
    "Commit the active directive envelope into durable session history."
);
capability_method!(
    agent_session_record_assistant,
    "harness.agent.session_record_assistant",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_record_assistant(session_id: string, llm_result: dict) -> nil",
    "Record an assistant result."
);
capability_method!(
    agent_session_pop_last_assistant,
    "harness.agent.session_pop_last_assistant",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_pop_last_assistant(session_id: string) -> dict",
    "Remove and return the last assistant message."
);
capability_method!(
    agent_session_record_tool_results,
    "harness.agent.session_record_tool_results",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_record_tool_results(session_id: string, dispatch: list) -> nil",
    "Record dispatched tool results."
);
capability_method!(
    agent_session_pair_orphaned_tool_use,
    "harness.agent.session_pair_orphaned_tool_use",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_pair_orphaned_tool_use(session_id: string, feedback: string) -> int",
    "Pair orphaned tool calls with synthetic results."
);
capability_method!(
    agent_session_record_usage,
    "harness.agent.session_record_usage",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_record_usage(session_id: string, llm_result: dict) -> dict",
    "Record model usage."
);
capability_method!(agent_reminder_providers_fire, "harness.agent.reminder_providers_fire",     // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [], "__cap_agent_reminder_providers_fire(session_id: string, event: string, payload?: dict|nil, options?: dict|nil) -> dict", "Run registered reminder providers for a session event.");
capability_method!(
    agent_session_drain_feedback,
    "harness.agent.session_drain_feedback",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_drain_feedback(session_id: string) -> list",
    "Drain queued session feedback."
);
capability_method!(
    agent_session_drain_command_updates,
    "harness.agent.session_drain_command_updates",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_drain_command_updates(session_id: string) -> list",
    "Drain queued command updates."
);
capability_method!(
    agent_session_await_inbox,
    "harness.agent.session_await_inbox",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_await_inbox(session_id: string, timeout_ms: int) -> bool",
    "Wait for agent inbox activity."
);
capability_method!(agent_session_drain_host_injections, "harness.agent.session_drain_host_injections",     // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [], "__cap_agent_session_drain_host_injections(session_id: string, delivery: string, seam: string) -> list", "Drain host injections at a delivery seam.");
capability_method!(
    agent_session_totals,
    "harness.agent.session_totals",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_totals(session_id: string) -> dict",
    "Read aggregate session totals."
);
capability_method!(
    agent_session_inject_feedback,
    "harness.agent.session_inject_feedback",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_inject_feedback(session_id: string, kind: string, content: string) -> nil",
    "Inject session feedback."
);
capability_method!(
    agent_session_inject_reminder,
    "harness.agent.session_inject_reminder",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_inject_reminder(session_id: string, options: dict) -> string",
    "Inject a session reminder."
);
capability_method!(agent_session_post_event, "harness.agent.session_post_event",     // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [], "__cap_agent_session_post_event(session_id: string, kind: string, content: string, source?: string|nil) -> nil", "Post a session event.");
capability_method!(
    agent_session_apply_reminder_post_turn,
    "harness.agent.session_apply_reminder_post_turn",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_apply_reminder_post_turn(session_id: string, turn?: dict|nil) -> dict",
    "Apply post-turn reminder policy."
);
capability_method!(
    agent_session_set_active_skills,
    "harness.agent.session_set_active_skills",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_set_active_skills(session_id: string, skills: list) -> nil",
    "Set active session skills."
);
capability_method!(
    agent_session_active_skills,
    "harness.agent.session_active_skills",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_active_skills(session_id: string) -> list",
    "Read active session skills."
);
capability_method!(agent_session_record_skill_event, "harness.agent.session_record_skill_event",     // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [], "__cap_agent_session_record_skill_event(session_id: string, kind: string, metadata: dict) -> nil", "Record a skill lifecycle event.");
capability_method!(
    agent_session_compact_if_needed,
    "harness.agent.session_compact_if_needed",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_compact_if_needed(session_id: string, options: dict) -> dict",
    "Compact a session when its policy requires it."
);
capability_method!(agent_session_replace_messages, "harness.agent.session_replace_messages",     // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [], "__cap_agent_session_replace_messages(session_id: string, messages: list, summary?: any) -> nil", "Replace session messages after compaction.");
capability_method!(
    agent_budget_pre_call_blocked,
    "harness.agent.budget_pre_call_blocked",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_budget_pre_call_blocked(session_id: string, envelope: dict) -> bool",
    "Evaluate the session budget before a model call."
);
capability_method!(
    agent_record_native_tool_fallback,
    "harness.agent.record_native_tool_fallback",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_record_native_tool_fallback(session_id: string, payload: dict) -> nil",
    "Record a native-tool fallback."
);
capability_method!(
    agent_record_compaction,
    "harness.agent.record_compaction",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_record_compaction(session_id: string, payload: dict) -> nil",
    "Record a compaction event."
);
capability_method!(
    agent_session_project_turn,
    "harness.agent.session_project_turn",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_project_turn(session_id: string, options?: dict|nil) -> dict",
    "Project the current session turn."
);
capability_method!(
    agent_session_claim_tool_format,
    "harness.agent.session_claim_tool_format",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_claim_tool_format(session_id: string, tool_format: string) -> dict",
    "Claim the session tool format."
);
capability_method!(
    agent_daemon_snapshot,
    "harness.agent.daemon_snapshot",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_daemon_snapshot(session_id: string, options: dict) -> dict",
    "Read an agent daemon snapshot."
);
capability_method!(
    agent_session_push_bridge_injection,
    "harness.agent.session_push_bridge_injection",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_push_bridge_injection(session_id: string, options: dict) -> string",
    "Queue a bridge injection."
);
capability_method!(
    agent_session_push_user_message,
    "harness.agent.session_push_user_message",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_push_user_message(session_id: string, options: dict) -> string",
    "Queue a user message."
);
capability_method!(
    agent_session_pending_injections,
    "harness.agent.session_pending_injections",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_pending_injections(session_id: string) -> list",
    "Read pending session injections."
);
capability_method!(
    agent_session_revoke_reminder,
    "harness.agent.session_revoke_reminder",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_revoke_reminder(session_id: string, reminder_id: string) -> bool",
    "Revoke a pending reminder."
);
capability_method!(
    agent_session_drain_bridge_injections,
    "harness.agent.session_drain_bridge_injections",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_session_drain_bridge_injections(session_id: string, checkpoint: dict) -> list",
    "Drain bridge injections at a checkpoint."
);
capability_method!(
    agent_daemon_wait,
    "harness.agent.daemon_wait",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_daemon_wait(session_id: string, timeout_ms: int) -> dict",
    "Wait for agent daemon activity."
);
capability_method!(
    agent_capture_events,
    "harness.agent.capture_events",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_capture_events(session_id: string, body: closure) -> dict",
    "Capture typed events emitted while a session body runs."
);
capability_method!(
    agent_parse_tool_calls,
    "harness.agent.parse_tool_calls",
    // Match the corresponding `__host_agent_*` runtime_internal builtin
    // (effects = []): live session journal / reminder / budget seams are
    // runtime-owned, not model-facing state effects. Inflated contracts
    // reject under agent-loop execution policy and abort turns mid-flight.
    [],
    "__cap_agent_parse_tool_calls(text: string, tools?: {_type: \"tool_registry\", tools: list}?, tool_format?: string?) -> dict",
    "Parse textual tool calls and stamp session-unique call identifiers."
);
capability_method!(
    agent_transcript_inject_reminder,
    "harness.agent.transcript_inject_reminder",
    [
        "state.mutate@arg0",
        "random.mutate@const=reminder-id",
        "observability.write@const=reminder-lifecycle"
    ],
    "__cap_agent_transcript_inject_reminder(transcript: list | dict | Transcript, options: dict) -> dict",
    "Inject a pending system reminder into a transcript."
);
capability_method!(
    agent_transcript_clear_reminders,
    "harness.agent.transcript_clear_reminders",
    [
        "state.mutate@arg0",
        "observability.write@const=reminder-lifecycle"
    ],
    "__cap_agent_transcript_clear_reminders(transcript: list | dict | Transcript, selector: dict) -> dict",
    "Remove pending system reminders selected by id, tag, or dedupe key."
);
capability_method!(
    agent_worker_spawn,
    "harness.agent.worker_spawn",
    ["worker.mutate@dynamic"],
    "__cap_agent_worker_spawn(config: dict) -> any",
    "Spawn a delegated worker from a normalized worker configuration."
);
capability_method!(
    agent_parse_resume_conditions,
    "harness.agent.parse_resume_conditions",
    ["worker.observe@const=resume-conditions"],
    "__cap_agent_parse_resume_conditions(conditions?: dict) -> ResumeConditions?",
    "Validate and normalize delegated-worker resumption conditions."
);
capability_method!(
    agent_worker_send_input,
    "harness.agent.worker_send_input",
    ["worker.write@arg0"],
    "__cap_agent_worker_send_input(worker: any, task: any) -> any",
    "Send input to a delegated worker."
);
capability_method!(
    agent_worker_trigger,
    "harness.agent.worker_trigger",
    ["worker.write@arg0"],
    "__cap_agent_worker_trigger(worker: any, payload: any) -> any",
    "Deliver a trigger payload to a delegated worker."
);
capability_method!(
    agent_worker_wait,
    "harness.agent.worker_wait",
    ["worker.observe@arg0"],
    "__cap_agent_worker_wait(worker_or_pool_task: any) -> any",
    "Wait for a delegated worker or pool task to reach a terminal state."
);
capability_method!(
    agent_worker_stop,
    "harness.agent.worker_stop",
    ["worker.mutate@arg0"],
    "__cap_agent_worker_stop(worker: any, options?: dict) -> any",
    "Stop a delegated worker, optionally preserving a graceful handoff."
);
capability_method!(
    agent_worker_close,
    "harness.agent.worker_close",
    ["worker.mutate@arg0"],
    "__cap_agent_worker_close(worker: any) -> any",
    "Close a delegated worker and release its runtime resources."
);
capability_method!(
    agent_worker_suspend,
    "harness.agent.worker_suspend",
    ["worker.mutate@arg0"],
    "__cap_agent_worker_suspend(worker: any, reason?: string, options?: dict) -> any",
    "Suspend a delegated worker and persist its resumable snapshot."
);
capability_method!(
    agent_worker_resume,
    "harness.agent.worker_resume",
    ["worker.mutate@arg0"],
    "__cap_agent_worker_resume(worker_or_snapshot: any, options?: dict) -> any",
    "Resume a suspended delegated worker or persisted worker snapshot."
);
capability_method!(
    agent_worker_list,
    "harness.agent.worker_list",
    ["worker.observe@const=delegated-workers"],
    "__cap_agent_worker_list() -> list",
    "List delegated workers owned by the current runtime."
);
capability_method!(
    agent_pool_create,
    "harness.agent.pool_create",
    ["worker.mutate@const=agent-pools", "fs.write@dynamic"],
    "__cap_agent_pool_create(options?: dict|nil) -> dict",
    "Create a bounded agent pool and return its derived handle."
);
capability_method!(
    agent_pool_get,
    "harness.agent.pool_get",
    ["worker.observe@arg0"],
    "__cap_agent_pool_get(name_or_id: string|dict) -> dict|nil",
    "Look up an agent pool by name or identifier."
);
capability_method!(
    agent_pool_list,
    "harness.agent.pool_list",
    ["worker.observe@const=agent-pools"],
    "__cap_agent_pool_list() -> list",
    "List agent pools owned by the current runtime."
);
capability_method!(
    agent_pool_wait,
    "harness.agent.pool_wait",
    ["worker.observe@arg0"],
    "__cap_agent_pool_wait(handle_or_handles: string|dict|list) -> dict",
    "Wait for one or more pool tasks to reach terminal state."
);
capability_method!(
    agent_pool_simulate_restart,
    "harness.agent.pool_simulate_restart",
    ["worker.mutate@const=agent-pools"],
    "__cap_agent_pool_simulate_restart() -> nil",
    "Reset in-process pool state for deterministic durability tests."
);

capability_method!(
    agent_open,
    "harness.agent.open",
    ["state.mutate@const=agent-sessions"],
    "__cap_agent_open(id?: string, opts?: dict) -> string",
    "Open or create an agent session."
);
capability_method!(
    agent_workspace_anchor,
    "harness.agent.workspace_anchor",
    ["state.read@arg0"],
    "__cap_agent_workspace_anchor(id: string) -> any",
    "Read a session workspace anchor."
);
capability_method!(
    agent_set_workspace_anchor,
    "harness.agent.set_workspace_anchor",
    ["state.write@arg0"],
    "__cap_agent_set_workspace_anchor(id: string, anchor: any) -> bool",
    "Set a session workspace anchor."
);
capability_method!(
    agent_workspace_policy,
    "harness.agent.workspace_policy",
    ["state.read@arg0"],
    "__cap_agent_workspace_policy(id: string) -> dict",
    "Read a session workspace policy."
);
capability_method!(
    agent_set_workspace_policy,
    "harness.agent.set_workspace_policy",
    ["state.write@arg0"],
    "__cap_agent_set_workspace_policy(id: string, policy: dict) -> bool",
    "Set a session workspace policy."
);
capability_method!(
    agent_add_root,
    "harness.agent.add_root",
    ["state.write@arg0"],
    "__cap_agent_add_root(id: string, root: string, opts?: dict) -> dict",
    "Add a session workspace root."
);
capability_method!(
    agent_remove_root,
    "harness.agent.remove_root",
    ["state.mutate@arg0"],
    "__cap_agent_remove_root(id: string, root: string) -> dict",
    "Remove a session workspace root."
);
capability_method!(
    agent_list_roots,
    "harness.agent.list_roots",
    ["state.read@arg0"],
    "__cap_agent_list_roots(id: string) -> dict",
    "List session workspace roots."
);
capability_method!(
    agent_exists,
    "harness.agent.exists",
    ["state.read@arg0"],
    "__cap_agent_exists(id: string) -> bool",
    "Test whether an agent session exists."
);
capability_method!(
    agent_length,
    "harness.agent.length",
    ["state.read@arg0"],
    "__cap_agent_length(id: string) -> int",
    "Read a session transcript length."
);
capability_method!(
    agent_snapshot,
    "harness.agent.snapshot",
    ["state.read@arg0"],
    "__cap_agent_snapshot(id: string) -> any",
    "Read a session snapshot."
);
capability_method!(
    agent_ancestry,
    "harness.agent.ancestry",
    ["state.read@arg0"],
    "__cap_agent_ancestry(id: string) -> dict",
    "Read session ancestry."
);
capability_method!(
    agent_current_id,
    "harness.agent.current_id",
    ["state.read@const=current-agent-session"],
    "__cap_agent_current_id() -> string?",
    "Read the current agent session identifier.",
    "llm.call"
);
capability_method!(
    agent_record_changed_path,
    "harness.agent.record_changed_path",
    ["state.write@arg1"],
    "__cap_agent_record_changed_path(path: string, session_id?: string) -> bool",
    "Record a changed path for a session."
);
capability_method!(
    agent_actor_chain,
    "harness.agent.actor_chain",
    ["state.read@arg0"],
    "__cap_agent_actor_chain(id?: string) -> dict?",
    "Read the agent actor chain."
);
capability_method!(
    agent_tool_format,
    "harness.agent.tool_format",
    ["state.read@arg0"],
    "__cap_agent_tool_format(id: string) -> string?",
    "Read the claimed tool format."
);
capability_method!(
    agent_system_prompt,
    "harness.agent.system_prompt",
    ["state.read@arg0"],
    "__cap_agent_system_prompt(id: string) -> string?",
    "Read the session system prompt."
);
capability_method!(
    agent_scratchpad,
    "harness.agent.scratchpad",
    ["state.read@arg0"],
    "__cap_agent_scratchpad(id: string) -> dict?",
    "Read the session scratchpad."
);
capability_method!(
    agent_set_scratchpad,
    "harness.agent.set_scratchpad",
    ["state.write@arg0"],
    "__cap_agent_set_scratchpad(id: string, scratchpad: dict, opts?: dict) -> dict",
    "Set the session scratchpad."
);
capability_method!(
    agent_clear_scratchpad,
    "harness.agent.clear_scratchpad",
    ["state.mutate@arg0"],
    "__cap_agent_clear_scratchpad(id: string, opts?: dict) -> dict",
    "Clear the session scratchpad."
);
capability_method!(
    agent_claim_tool_format,
    "harness.agent.claim_tool_format",
    ["state.mutate@arg0"],
    "__cap_agent_claim_tool_format(id: string, tool_format: string) -> nil",
    "Claim a session tool format."
);
capability_method!(
    agent_reset,
    "harness.agent.reset",
    ["state.mutate@arg0"],
    "__cap_agent_reset(id: string) -> nil",
    "Reset a session."
);
capability_method!(
    agent_fork,
    "harness.agent.fork",
    ["state.read@arg0", "state.write@arg1"],
    "__cap_agent_fork(src: string, dst?: string) -> string",
    "Fork a session."
);
capability_method!(
    agent_fork_at,
    "harness.agent.fork_at",
    ["state.read@arg0", "state.write@arg2"],
    "__cap_agent_fork_at(src: string, keep_first: int, dst?: string) -> string",
    "Fork a session at a transcript position."
);
capability_method!(
    agent_rollback,
    "harness.agent.rollback",
    ["state.mutate@arg0"],
    "__cap_agent_rollback(id: string) -> dict",
    "Roll back the last session mutation."
);
capability_method!(
    agent_redo,
    "harness.agent.redo",
    ["state.mutate@arg0"],
    "__cap_agent_redo(id: string) -> dict",
    "Redo a rolled-back session mutation."
);
capability_method!(
    agent_close,
    "harness.agent.close",
    ["state.mutate@arg0"],
    "__cap_agent_close(id: string, status?: any) -> nil",
    "Close a session."
);
capability_method!(
    agent_trim,
    "harness.agent.trim",
    ["state.mutate@arg0"],
    "__cap_agent_trim(id: string, keep_last: int) -> nil",
    "Trim a session transcript."
);
capability_method!(
    agent_attach,
    "harness.agent.attach",
    ["state.write@arg0"],
    "__cap_agent_attach(id: string, client_id: string, opts?: dict) -> dict",
    "Attach a live client."
);
capability_method!(
    agent_takeover,
    "harness.agent.takeover",
    ["state.mutate@arg0"],
    "__cap_agent_takeover(id: string, client_id: string, opts?: dict) -> dict",
    "Transfer session ownership to a client."
);
capability_method!(
    agent_detach,
    "harness.agent.detach",
    ["state.mutate@arg0"],
    "__cap_agent_detach(id: string, client_id: string, opts?: dict) -> dict",
    "Detach a live client."
);
capability_method!(
    agent_heartbeat,
    "harness.agent.heartbeat",
    ["state.write@arg0"],
    "__cap_agent_heartbeat(id: string, client_id: string, opts?: dict) -> dict",
    "Record a client heartbeat."
);
capability_method!(
    agent_live_clients,
    "harness.agent.live_clients",
    ["state.read@arg0"],
    "__cap_agent_live_clients(id: string) -> list",
    "List live session clients."
);
capability_method!(agent_client_inject_prompt, "harness.agent.client_inject_prompt", ["state.write@arg0"], "__cap_agent_client_inject_prompt(id: string, client_id: string, content: any, opts?: dict) -> nil", "Inject a client prompt.");
capability_method!(agent_route_permission, "harness.agent.route_permission", ["state.mutate@arg0"], "__cap_agent_route_permission(id: string, client_id: string, request: any, opts?: dict) -> dict", "Route a permission request.");
capability_method!(
    agent_inject,
    "harness.agent.inject",
    ["state.write@arg0"],
    "__cap_agent_inject(id: string, message: any) -> nil",
    "Inject a session message."
);
capability_method!(
    agent_post_event,
    "harness.agent.post_event",
    ["state.write@arg0"],
    "__cap_agent_post_event(id: string, kind: string, content: any, source?: any) -> nil",
    "Post a durable session event."
);
capability_method!(
    agent_drain_inbox,
    "harness.agent.drain_inbox",
    ["state.mutate@arg0"],
    "__cap_agent_drain_inbox(id: string) -> list",
    "Drain a session inbox."
);
capability_method!(
    agent_seed_from_jsonl,
    "harness.agent.seed_from_jsonl",
    ["fs.read@arg0", "state.write@const=agent-sessions"],
    "__cap_agent_seed_from_jsonl(jsonl_path: string, opts?: dict) -> string",
    "Seed a session from a JSONL transcript."
);
capability_method!(
    agent_reanchor,
    "harness.agent.reanchor",
    ["state.mutate@arg0"],
    "__cap_agent_reanchor(id: string, new_anchor: any, opts?: dict) -> dict",
    "Reanchor a session workspace."
);
capability_method!(
    agent_compact,
    "harness.agent.compact",
    ["state.mutate@arg0"],
    "__cap_agent_compact(id: string, opts?: dict) -> dict",
    "Compact a session transcript."
);
capability_method!(
    agent_self_review,
    "harness.agent.self_review",
    [
        "llm.write@dynamic",
        "state.write@const=review-audit",
        "state.write@const=trust-graph"
    ],
    "__cap_agent_self_review(diff: string, rubric?: any, max_rounds?: int) -> dict",
    "Run model-assisted review and record its audit and trust evidence."
);
capability_method!(
    agent_compact_transcript,
    "harness.agent.compact_transcript",
    ["state.mutate@arg0", "llm.write@dynamic"],
    "__cap_agent_compact_transcript(transcript: dict, options?: dict) -> dict",
    "Compact an immutable transcript through the runtime lifecycle."
);
