capability_method!(
    testing_clear,
    "harness.testing.clear",
    ["state.mutate@const=capability-fixtures"],
    "__cap_testing_clear() -> nil",
    "Clear and enable this Harness instance's capability fixtures."
);
capability_method!(
    testing_push_scope,
    "harness.testing.push_scope",
    ["state.mutate@const=capability-fixtures"],
    "__cap_testing_push_scope() -> nil",
    "Start an isolated nested capability-fixture scope on this Harness."
);
capability_method!(
    testing_pop_scope,
    "harness.testing.pop_scope",
    ["state.mutate@const=capability-fixtures"],
    "__cap_testing_pop_scope() -> nil",
    "Restore the preceding capability-fixture scope on this Harness."
);
capability_method!(
    testing_respond,
    "harness.testing.respond",
    ["state.write@arg0"],
    "__cap_testing_respond(capability: string, method: string, value: any, when?: dict, repeat?: bool) -> nil",
    "Queue one successful response for a closed capability method."
);
capability_method!(
    testing_respond_error,
    "harness.testing.respond_error",
    ["state.write@arg0"],
    "__cap_testing_respond_error(capability: string, method: string, message: string, when?: dict, repeat?: bool) -> nil",
    "Queue one error response for a closed capability method."
);
capability_method!(
    testing_calls,
    "harness.testing.calls",
    ["state.read@const=capability-fixtures"],
    "__cap_testing_calls() -> list",
    "Read calls captured by this Harness instance's capability fixtures."
);
capability_method!(
    testing_clock_set,
    "harness.testing.clock_set",
    ["clock.mutate@const=test-clock"],
    "__cap_testing_clock_set(unix_ms: int) -> nil",
    "Install a virtual clock scoped to this Harness and pin its wall time."
);
capability_method!(
    testing_clock_advance,
    "harness.testing.clock_advance",
    ["clock.mutate@const=test-clock"],
    "__cap_testing_clock_advance(milliseconds: int) -> int",
    "Advance this Harness's virtual clock and return its Unix milliseconds."
);
capability_method!(
    testing_clock_reset,
    "harness.testing.clock_reset",
    ["clock.mutate@const=test-clock"],
    "__cap_testing_clock_reset() -> nil",
    "Remove this Harness's virtual clock override."
);
capability_method!(
    testing_http_mock,
    "harness.testing.http_mock",
    ["state.write@arg1"],
    "__cap_testing_http_mock(method: string, url_pattern: string, response?: dict) -> nil",
    "Register or replace an HTTP response fixture."
);
capability_method!(
    testing_http_mock_clear,
    "harness.testing.http_mock_clear",
    ["state.mutate@const=http-fixtures"],
    "__cap_testing_http_mock_clear() -> nil",
    "Clear HTTP response fixtures and captured calls."
);
capability_method!(
    testing_http_mock_calls,
    "harness.testing.http_mock_calls",
    ["state.read@const=http-fixtures"],
    "__cap_testing_http_mock_calls(options?: dict) -> list",
    "Read HTTP calls captured by the fixture transport."
);
capability_method!(
    testing_transport_mock_clear,
    "harness.testing.transport_mock_clear",
    ["state.mutate@const=streaming-transport-fixtures"],
    "__cap_testing_transport_mock_clear() -> nil",
    "Clear SSE and WebSocket transport fixtures and captured calls."
);
capability_method!(
    testing_transport_mock_calls,
    "harness.testing.transport_mock_calls",
    ["state.read@const=streaming-transport-fixtures"],
    "__cap_testing_transport_mock_calls() -> list",
    "Read SSE and WebSocket calls captured by transport fixtures."
);
capability_method!(
    testing_sse_mock,
    "harness.testing.sse_mock",
    ["state.write@const=sse-mocks"],
    "__cap_testing_sse_mock(url_pattern: string, events?: any) -> nil",
    "Install a deterministic SSE client mock."
);
capability_method!(
    testing_sse_server_mock_receive,
    "harness.testing.sse_server_mock_receive",
    ["state.mutate@arg0"],
    "__cap_testing_sse_server_mock_receive(stream: dict) -> dict",
    "Receive one buffered SSE server event in a test."
);
capability_method!(
    testing_sse_server_mock_disconnect,
    "harness.testing.sse_server_mock_disconnect",
    ["state.mutate@arg0"],
    "__cap_testing_sse_server_mock_disconnect(stream: dict) -> bool",
    "Simulate an SSE server peer disconnect."
);
capability_method!(
    testing_websocket_mock,
    "harness.testing.websocket_mock",
    ["state.write@const=websocket-mocks"],
    "__cap_testing_websocket_mock(url_pattern: string, messages?: any) -> nil",
    "Install a deterministic WebSocket client mock."
);
capability_method!(
    testing_stdin_set,
    "harness.testing.stdin_set",
    ["stdio.mutate@const=stdin-fixture"],
    "__cap_testing_stdin_set(text: string) -> nil",
    "Install deterministic standard input for this test execution."
);
capability_method!(
    testing_stdin_reset,
    "harness.testing.stdin_reset",
    ["stdio.mutate@const=stdin-fixture"],
    "__cap_testing_stdin_reset() -> nil",
    "Remove deterministic standard input."
);
capability_method!(
    testing_tty_set,
    "harness.testing.tty_set",
    ["stdio.mutate@arg0"],
    "__cap_testing_tty_set(stream: string, is_tty: bool) -> nil",
    "Override terminal detection for one standard stream."
);
capability_method!(
    testing_tty_reset,
    "harness.testing.tty_reset",
    ["stdio.mutate@const=tty-fixture"],
    "__cap_testing_tty_reset() -> nil",
    "Remove terminal-detection overrides."
);
capability_method!(
    testing_capture_stderr_start,
    "harness.testing.capture_stderr_start",
    ["stdio.mutate@const=stderr-capture"],
    "__cap_testing_capture_stderr_start() -> nil",
    "Start capturing standard error for a test."
);
capability_method!(
    testing_capture_stderr_take,
    "harness.testing.capture_stderr_take",
    ["stdio.mutate@const=stderr-capture"],
    "__cap_testing_capture_stderr_take() -> string",
    "Stop standard-error capture and return its contents."
);
capability_method!(
    tools_composition_execute,
    "harness.tools.composition_execute",
    ["tool.mutate@dynamic"],
    "__cap_tools_composition_execute(snippet: string, manifest: dict, options?: dict) -> dict",
    "Execute a bounded composition over explicitly supplied tool bindings."
);
capability_method!(
    rules_visit,
    "harness.rules.visit",
    ["host.read@const=rules-engine"],
    "__cap_rules_visit(params: dict) -> dict",
    "Run a rule matcher and invoke its explicit visitor closure for each match."
);

capability_method!(
    embed_text,
    "harness.embed.text",
    [
        "llm.write@arg1.model_hint",
        "fs.read@arg1.root",
        "fs.write@arg1.root"
    ],
    "__cap_embed_text(text: string, options?: dict) -> dict",
    "Embed text through the configured model with the content-addressed cache."
);

capability_method!(
    memory_open,
    "harness.memory.open",
    ["fs.write@arg0", "llm.write@arg1.embed_model_hint"],
    "__cap_memory_open(namespace: string, options?: dict) -> dict",
    "Configure a durable memory namespace."
);
capability_method!(
    memory_store,
    "harness.memory.store",
    ["fs.write@arg0", "llm.write@arg4.embed_model_hint"],
    "__cap_memory_store(namespace: string, key: string, value: any, tags?: any, options?: dict) -> dict",
    "Append a durable memory observation."
);
capability_method!(
    memory_recall,
    "harness.memory.recall",
    [
        "fs.read@arg0",
        "fs.write@arg0",
        "llm.write@arg3.embed_model_hint"
    ],
    "__cap_memory_recall(namespace: string, query: string, limit?: int, options?: dict) -> list",
    "Recall records from durable memory."
);
capability_method!(
    memory_summarize,
    "harness.memory.summarize",
    ["fs.read@arg0"],
    "__cap_memory_summarize(namespace: string, window?: any, options?: dict) -> dict",
    "Summarize a durable memory namespace."
);
capability_method!(
    memory_forget,
    "harness.memory.forget",
    ["fs.write@arg0"],
    "__cap_memory_forget(namespace: string, predicate: any, options?: dict) -> dict",
    "Append a durable memory forget event."
);
capability_method!(
    memory_update,
    "harness.memory.update",
    ["fs.write@arg0", "llm.write@arg3.embed_model_hint"],
    "__cap_memory_update(namespace: string, id: string, patch: dict, options?: dict) -> dict",
    "Append a durable memory update event."
);
capability_method!(
    memory_list,
    "harness.memory.list",
    ["fs.read@arg0"],
    "__cap_memory_list(namespace: string, options?: dict) -> list",
    "List active records in durable memory."
);

capability_method!(
    sqlite_open,
    "harness.sqlite.open",
    ["fs.read@arg0", "fs.write@arg0"],
    "__cap_sqlite_open(path: string, options?: dict) -> resource",
    "Open a sandboxed SQLite database handle."
);
capability_method!(
    postgres_connect,
    "harness.postgres.connect",
    ["network.write@const=postgres"],
    "__cap_postgres_connect(url: string, options?: dict) -> resource",
    "Open a PostgreSQL connection handle."
);
capability_method!(
    postgres_pool,
    "harness.postgres.pool",
    ["network.write@const=postgres"],
    "__cap_postgres_pool(url: string, options?: dict) -> resource",
    "Open a PostgreSQL pool handle."
);

capability_method!(
    system_platform,
    "harness.system.platform",
    ["host.read@const=platform"],
    "__cap_system_platform() -> dict",
    "Read operating-system information."
);
capability_method!(
    system_host_conditions,
    "harness.system.host_conditions",
    ["host.read@const=contention"],
    "__cap_system_host_conditions() -> HostConditionsSnapshot",
    "Sample portable host contention observations."
);
capability_method!(
    system_sandbox_active_backend,
    "harness.system.sandbox_active_backend",
    ["host.read@const=sandbox"],
    "__cap_system_sandbox_active_backend() -> string",
    "Read the active platform sandbox backend."
);
capability_method!(
    system_sandbox_backend_available,
    "harness.system.sandbox_backend_available",
    ["host.read@const=sandbox"],
    "__cap_system_sandbox_backend_available() -> bool",
    "Test whether the active platform sandbox backend is available."
);
capability_method!(
    system_sandbox_active_profile,
    "harness.system.sandbox_active_profile",
    ["host.read@const=sandbox"],
    "__cap_system_sandbox_active_profile() -> string",
    "Read the active sandbox enforcement profile."
);
capability_method!(
    code_index_file_hash_snapshot,
    "harness.code_index.file_hash_snapshot",
    ["fs.read@arg0.paths"],
    "__cap_code_index_file_hash_snapshot(request: {paths: list<string>}) -> VerificationFileHashSnapshot",
    "Capture content hashes and index sequence metadata for files."
);
capability_method!(
    system_identity,
    "harness.system.identity",
    ["host.read@const=process-identity"],
    "__cap_system_identity() -> {username: string, hostname: string?, pid: int}",
    "Read the current user, host name, and process id."
);
capability_method!(
    system_cpu,
    "harness.system.cpu",
    ["host.read@const=cpu"],
    "__cap_system_cpu() -> dict",
    "Read CPU information."
);
capability_method!(
    system_memory,
    "harness.system.memory",
    ["host.read@const=memory"],
    "__cap_system_memory() -> dict",
    "Read memory information."
);
capability_method!(
    system_gpus,
    "harness.system.gpus",
    ["host.read@const=gpu"],
    "__cap_system_gpus() -> list",
    "Read GPU information."
);
capability_method!(
    system_temperature,
    "harness.system.temperature",
    ["host.read@const=temperature"],
    "__cap_system_temperature() -> dict",
    "Read temperature sensors."
);
capability_method!(
    system_processes,
    "harness.system.processes",
    ["host.read@const=process-table"],
    "__cap_system_processes() -> list",
    "Read process information."
);

capability_method!(
    secrets_read,
    "harness.secrets.read",
    ["secret.read@arg0"],
    "__cap_secrets_read(name: string, scope?: any) -> string",
    "Read a text secret."
);
capability_method!(
    secrets_read_bytes,
    "harness.secrets.read_bytes",
    ["secret.read@arg0"],
    "__cap_secrets_read_bytes(name: string, scope?: any) -> bytes",
    "Read a binary secret."
);
capability_method!(
    secrets_write,
    "harness.secrets.write",
    ["secret.write@arg0"],
    "__cap_secrets_write(name: string, value: any, scope?: any) -> nil",
    "Write a secret."
);
capability_method!(
    secrets_delete,
    "harness.secrets.delete",
    ["secret.mutate@arg0"],
    "__cap_secrets_delete(name: string, scope?: any) -> nil",
    "Delete a secret."
);
capability_method!(
    secrets_rotate,
    "harness.secrets.rotate",
    ["secret.mutate@arg0"],
    "__cap_secrets_rotate(name: string, value: any, scope?: any) -> nil",
    "Rotate a secret."
);
capability_method!(
    secrets_lease,
    "harness.secrets.lease",
    ["secret.read@arg0"],
    "__cap_secrets_lease(name: string, ttl_ms?: int, scope?: any) -> any",
    "Lease a text secret."
);
capability_method!(
    secrets_lease_bytes,
    "harness.secrets.lease_bytes",
    ["secret.read@arg0"],
    "__cap_secrets_lease_bytes(name: string, ttl_ms?: int, scope?: any) -> any",
    "Lease a binary secret."
);
