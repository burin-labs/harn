capability_method!(
    net_get,
    "harness.net.get",
    ["network.read@arg0"],
    "__cap_net_get(url: string, options?: dict) -> dict",
    "Send an HTTP GET request."
);
capability_method!(
    runtime_command_policy_push,
    "harness.runtime.command_policy_push",
    ["state.mutate@const=command-policy"],
    "command_policy_push(policy: dict) -> nil",
    "Push a command policy for the current execution scope."
);
capability_method!(
    runtime_command_policy_pop,
    "harness.runtime.command_policy_pop",
    ["state.mutate@const=command-policy"],
    "command_policy_pop() -> nil",
    "Pop the current execution's command policy."
);
capability_method!(
    runtime_with_command_policy,
    "harness.runtime.with_command_policy",
    ["state.mutate@const=runtime-policy"],
    "with_command_policy(policy: dict, fn: closure) -> any",
    "Run a closure under a scoped command policy."
);
capability_method!(
    runtime_with_autonomy_policy,
    "harness.runtime.with_autonomy_policy",
    ["state.mutate@const=runtime-policy"],
    "with_autonomy_policy(policy: dict, fn: closure) -> any",
    "Run a closure under a scoped autonomy policy."
);
capability_method!(
    net_egress_policy,
    "harness.net.egress_policy",
    ["state.mutate@const=egress-policy"],
    "__cap_net_egress_policy(config: dict) -> dict",
    "Install the outbound network policy for this execution."
);
capability_method!(
    net_post,
    "harness.net.post",
    ["network.write@arg0"],
    "__cap_net_post(url: string, body?: any, options?: dict) -> dict",
    "Send an HTTP POST request."
);
capability_method!(
    net_put,
    "harness.net.put",
    ["network.write@arg0"],
    "__cap_net_put(url: string, body?: any, options?: dict) -> dict",
    "Send an HTTP PUT request."
);
capability_method!(
    net_patch,
    "harness.net.patch",
    ["network.write@arg0"],
    "__cap_net_patch(url: string, body?: any, options?: dict) -> dict",
    "Send an HTTP PATCH request."
);
capability_method!(
    net_delete,
    "harness.net.delete",
    ["network.write@arg0"],
    "__cap_net_delete(url: string, options?: dict) -> dict",
    "Send an HTTP DELETE request."
);
capability_method!(
    net_request,
    "harness.net.request",
    ["network.mutate@arg1"],
    "__cap_net_request(method: string, url: string, options?: dict) -> dict",
    "Send a structured HTTP request."
);
capability_method!(
    net_download,
    "harness.net.download",
    ["network.read@arg0", "fs.write@arg1"],
    "__cap_net_download(url: string, destination: string, options?: dict) -> dict",
    "Download an HTTP response to a file."
);
capability_method!(
    net_stream_open,
    "harness.net.stream_open",
    ["network.read@arg0"],
    "__cap_net_stream_open(url: string, options?: dict) -> string",
    "Open a bounded streaming HTTP response."
);
capability_method!(
    net_stream_read,
    "harness.net.stream_read",
    ["network.read@arg0"],
    "__cap_net_stream_read(stream: string, max_bytes?: int) -> bytes?",
    "Read the next bounded chunk from an HTTP stream."
);
capability_method!(
    net_stream_info,
    "harness.net.stream_info",
    ["network.observe@arg0"],
    "__cap_net_stream_info(stream: string) -> dict",
    "Inspect HTTP stream metadata."
);
capability_method!(
    net_stream_close,
    "harness.net.stream_close",
    ["network.mutate@arg0"],
    "__cap_net_stream_close(stream: string) -> bool",
    "Close an HTTP stream."
);
capability_method!(
    net_session,
    "harness.net.session",
    ["network.mutate@const=http-session"],
    "__cap_net_session(options?: dict) -> string",
    "Create a reusable HTTP client session."
);
capability_method!(
    net_session_request,
    "harness.net.session_request",
    ["network.mutate@arg2"],
    "__cap_net_session_request(session: string, method: string, url: string, options?: dict) -> dict",
    "Send an HTTP request through a reusable session."
);
capability_method!(
    net_session_close,
    "harness.net.session_close",
    ["network.mutate@arg0"],
    "__cap_net_session_close(session: string) -> bool",
    "Close a reusable HTTP client session."
);
capability_method!(
    net_server,
    "harness.net.server",
    ["network.mutate@const=http-server"],
    "__cap_net_server(options?: dict) -> dict",
    "Create an in-process HTTP server resource."
);
capability_method!(
    net_server_route,
    "harness.net.server_route",
    ["network.mutate@arg0"],
    "__cap_net_server_route(server: dict, method: string, path: string, handler: closure, options?: dict) -> dict",
    "Add a route to an HTTP server."
);
capability_method!(
    net_server_before,
    "harness.net.server_before",
    ["network.mutate@arg0"],
    "__cap_net_server_before(server: dict, handler: closure) -> dict",
    "Add an HTTP server before hook."
);
capability_method!(
    net_server_after,
    "harness.net.server_after",
    ["network.mutate@arg0"],
    "__cap_net_server_after(server: dict, handler: closure) -> dict",
    "Add an HTTP server after hook."
);
capability_method!(
    net_server_request,
    "harness.net.server_request",
    ["network.mutate@arg0"],
    "__cap_net_server_request(server: dict, request: dict) -> dict",
    "Dispatch a request through an HTTP server."
);
capability_method!(
    net_server_test,
    "harness.net.server_test",
    ["network.mutate@arg0"],
    "__cap_net_server_test(server: dict, request: dict) -> dict",
    "Dispatch an in-process test request through an HTTP server."
);
capability_method!(
    net_server_set_ready,
    "harness.net.server_set_ready",
    ["network.mutate@arg0"],
    "__cap_net_server_set_ready(server: dict, ready: bool) -> bool",
    "Set HTTP server readiness."
);
capability_method!(
    net_server_readiness,
    "harness.net.server_readiness",
    ["network.mutate@arg0"],
    "__cap_net_server_readiness(server: dict, handler: closure) -> dict",
    "Install an HTTP server readiness handler."
);
capability_method!(
    net_server_ready,
    "harness.net.server_ready",
    ["network.observe@arg0"],
    "__cap_net_server_ready(server: dict) -> bool",
    "Read HTTP server readiness."
);
capability_method!(
    net_server_on_shutdown,
    "harness.net.server_on_shutdown",
    ["network.mutate@arg0"],
    "__cap_net_server_on_shutdown(server: dict, handler: closure) -> dict",
    "Install an HTTP server shutdown hook."
);
capability_method!(
    net_server_shutdown,
    "harness.net.server_shutdown",
    ["network.mutate@arg0"],
    "__cap_net_server_shutdown(server: dict) -> bool",
    "Shut down an HTTP server."
);
capability_method!(
    net_server_tls_plain,
    "harness.net.server_tls_plain",
    ["network.observe@const=tls-config"],
    "__cap_net_server_tls_plain() -> dict",
    "Build a plaintext HTTP listener configuration."
);
capability_method!(
    net_server_tls_edge,
    "harness.net.server_tls_edge",
    ["network.observe@const=tls-config"],
    "__cap_net_server_tls_edge(options?: dict) -> dict",
    "Build a TLS-at-the-edge listener configuration."
);
capability_method!(
    net_server_tls_pem,
    "harness.net.server_tls_pem",
    ["fs.read@arg0", "fs.read@arg1"],
    "__cap_net_server_tls_pem(cert_path: string, key_path: string) -> dict",
    "Validate PEM inputs and build a direct TLS listener configuration."
);
capability_method!(
    net_server_tls_self_signed_dev,
    "harness.net.server_tls_self_signed_dev",
    ["random.mutate@const=tls-private-key"],
    "__cap_net_server_tls_self_signed_dev(hosts?: list | string) -> dict",
    "Generate a development-only self-signed TLS listener configuration."
);
capability_method!(
    net_server_security_headers,
    "harness.net.server_security_headers",
    ["network.observe@const=tls-config"],
    "__cap_net_server_security_headers(tls_config: dict) -> dict",
    "Project security headers from a TLS listener configuration."
);
capability_method!(
    net_unix_socket_json_request,
    "harness.net.unix_socket_json_request",
    ["network.mutate@arg0"],
    "__cap_net_unix_socket_json_request(path: string, request: any, options?: dict) -> UnixSocketJsonResult",
    "Exchange one JSON-line request over a Unix domain socket."
);
capability_method!(
    net_jsonrpc_call,
    "harness.net.jsonrpc_call",
    ["network.write@arg0"],
    "__cap_net_jsonrpc_call(url: string, method: string, params?: any, options?: dict|nil) -> any",
    "Send one JSON-RPC 2.0 request."
);
capability_method!(
    net_jsonrpc_batch,
    "harness.net.jsonrpc_batch",
    ["network.write@arg0"],
    "__cap_net_jsonrpc_batch(url: string, calls: list, options?: dict|nil) -> list",
    "Send a JSON-RPC 2.0 batch."
);
capability_method!(
    net_sse_connect,
    "harness.net.sse_connect",
    ["network.read@arg1"],
    "__cap_net_sse_connect(method: string, url: string, options?: dict) -> dict",
    "Open a bounded Server-Sent Events client stream."
);
capability_method!(
    net_sse_receive,
    "harness.net.sse_receive",
    ["network.read@arg0"],
    "__cap_net_sse_receive(stream: dict, timeout_ms?: int) -> dict?",
    "Receive one event from an SSE client stream."
);
capability_method!(
    net_sse_close,
    "harness.net.sse_close",
    ["network.mutate@arg0"],
    "__cap_net_sse_close(stream: dict) -> bool",
    "Close an SSE client stream."
);
capability_method!(
    net_sse_server_response,
    "harness.net.sse_server_response",
    ["network.mutate@const=sse-server"],
    "__cap_net_sse_server_response(options?: dict) -> dict",
    "Create an SSE server response stream."
);
capability_method!(
    net_sse_server_send,
    "harness.net.sse_server_send",
    ["network.write@arg0"],
    "__cap_net_sse_server_send(stream: dict, event: any, options?: dict) -> bool",
    "Send one SSE server event."
);
capability_method!(
    net_sse_server_heartbeat,
    "harness.net.sse_server_heartbeat",
    ["network.write@arg0"],
    "__cap_net_sse_server_heartbeat(stream: dict, comment?: any) -> bool",
    "Send an SSE heartbeat."
);
capability_method!(
    net_sse_server_flush,
    "harness.net.sse_server_flush",
    ["network.write@arg0"],
    "__cap_net_sse_server_flush(stream: dict) -> bool",
    "Flush an SSE server stream."
);
capability_method!(
    net_sse_server_close,
    "harness.net.sse_server_close",
    ["network.mutate@arg0"],
    "__cap_net_sse_server_close(stream: dict) -> bool",
    "Close an SSE server stream."
);
capability_method!(
    net_sse_server_cancel,
    "harness.net.sse_server_cancel",
    ["network.mutate@arg0"],
    "__cap_net_sse_server_cancel(stream: dict, reason?: any) -> bool",
    "Cancel an SSE server stream."
);
capability_method!(
    net_sse_server_status,
    "harness.net.sse_server_status",
    ["network.observe@arg0"],
    "__cap_net_sse_server_status(stream: dict) -> dict",
    "Inspect an SSE server stream."
);
capability_method!(
    net_sse_server_disconnected,
    "harness.net.sse_server_disconnected",
    ["network.observe@arg0"],
    "__cap_net_sse_server_disconnected(stream: dict) -> bool",
    "Return whether an SSE server peer disconnected."
);
capability_method!(
    net_sse_server_cancelled,
    "harness.net.sse_server_cancelled",
    ["network.observe@arg0"],
    "__cap_net_sse_server_cancelled(stream: dict) -> bool",
    "Return whether an SSE server stream was cancelled."
);
capability_method!(
    net_websocket_connect,
    "harness.net.websocket_connect",
    ["network.mutate@arg0"],
    "__cap_net_websocket_connect(url: string, options?: dict) -> dict",
    "Open a WebSocket connection."
);
capability_method!(
    net_websocket_server,
    "harness.net.websocket_server",
    ["network.mutate@arg0"],
    "__cap_net_websocket_server(bind?: string, options?: dict) -> dict",
    "Create a WebSocket server."
);
capability_method!(
    net_websocket_route,
    "harness.net.websocket_route",
    ["network.mutate@arg0"],
    "__cap_net_websocket_route(server: dict, path: string, options?: dict) -> bool",
    "Add a route to a WebSocket server."
);
capability_method!(
    net_websocket_accept,
    "harness.net.websocket_accept",
    ["network.mutate@arg0"],
    "__cap_net_websocket_accept(server: dict, timeout_ms?: int) -> dict?",
    "Accept a WebSocket connection."
);
capability_method!(
    net_websocket_send,
    "harness.net.websocket_send",
    ["network.write@arg0"],
    "__cap_net_websocket_send(socket: dict, message: any, options?: dict) -> bool",
    "Send a WebSocket frame."
);
capability_method!(
    net_websocket_receive,
    "harness.net.websocket_receive",
    ["network.read@arg0"],
    "__cap_net_websocket_receive(socket: dict, timeout_ms?: int) -> dict?",
    "Receive a WebSocket frame."
);
capability_method!(
    net_websocket_close,
    "harness.net.websocket_close",
    ["network.mutate@arg0"],
    "__cap_net_websocket_close(socket: dict) -> bool",
    "Close a WebSocket connection."
);
capability_method!(
    net_websocket_server_close,
    "harness.net.websocket_server_close",
    ["network.mutate@arg0"],
    "__cap_net_websocket_server_close(server: dict) -> bool",
    "Close a WebSocket server."
);
capability_method!(
    system_vision_ocr,
    "harness.system.vision_ocr",
    [
        "fs.read@arg0.path",
        "fs.read@arg0.storage.path",
        "process.write@const=tesseract",
        "state.write@const=vision-ocr-audit"
    ],
    "__cap_system_vision_ocr(image: string|dict, options?: dict) -> StructuredText",
    "Recognize structured text through the configured OCR backend."
);
capability_method!(
    system_security_policy,
    "harness.system.security_policy",
    ["state.mutate@const=security-policy"],
    "__cap_system_security_policy(config: dict) -> dict",
    "Install a security policy for the current execution scope."
);
capability_method!(
    system_security_stamp_directive,
    "harness.system.security_stamp_directive",
    ["secret.read@const=directive-signing-key"],
    "__cap_system_security_stamp_directive(content: string, emitter?: string) -> string",
    "Stamp an orchestration directive with runtime-owned provenance."
);
capability_method!(
    system_security_verify_directive,
    "harness.system.security_verify_directive",
    ["secret.read@const=directive-signing-key"],
    "__cap_system_security_verify_directive(content: string) -> dict",
    "Verify the runtime-owned provenance of an orchestration directive."
);

capability_method!(
    process_run,
    "harness.process.run",
    ["process.write@arg0.program", "process.write@arg0.command"],
    "__cap_process_run(command: dict) -> @PROCESS_RESULT",
    "Run a structured child process and capture its result."
);
capability_method!(
    process_exec,
    "harness.process.exec",
    ["process.write@arg0"],
    "__cap_process_exec(...command: string) -> dict",
    "Execute a program and argument vector."
);
capability_method!(
    process_shell,
    "harness.process.shell",
    ["process.write@arg0"],
    "__cap_process_shell(command: string) -> dict",
    "Execute a command through the configured shell."
);
capability_method!(
    process_exec_at,
    "harness.process.exec_at",
    ["fs.read@arg0", "process.write@arg1"],
    "__cap_process_exec_at(directory: string, ...command: string) -> dict",
    "Execute a program and argument vector in a working directory."
);
capability_method!(
    process_shell_at,
    "harness.process.shell_at",
    ["fs.read@arg0", "process.write@arg1"],
    "__cap_process_shell_at(directory: string, command: string) -> dict",
    "Execute a shell command in a working directory."
);
capability_method!(
    process_default_shell,
    "harness.process.default_shell",
    ["process.read@const=shell-configuration"],
    "__cap_process_default_shell() -> dict",
    "Read the host's selected command shell."
);
capability_method!(
    process_git_repo_discover,
    "harness.process.git_repo_discover",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_repo_discover(path: string) -> GitDiscoverReceipt",
    "Discover repository metadata for a path."
);
capability_method!(process_git_worktree_create, "harness.process.git_worktree_create", ["process.write@const=git", "fs.mutate@arg0", "fs.mutate@arg2"], "__cap_process_git_worktree_create(repo: string, branch: string, path: string, options?: dict) -> GitWorktreeCreateReceipt", "Create a Git worktree.");
capability_method!(
    process_git_worktree_remove,
    "harness.process.git_worktree_remove",
    ["process.write@const=git", "fs.mutate@arg0"],
    "__cap_process_git_worktree_remove(path: string, options?: dict) -> GitWorktreeRemoveReceipt",
    "Remove a Git worktree."
);
capability_method!(
    process_git_fetch,
    "harness.process.git_fetch",
    [
        "process.write@const=git",
        "network.read@arg1",
        "fs.mutate@arg0"
    ],
    "__cap_process_git_fetch(repo: string, remote: string, refspecs?: list) -> GitFetchReceipt",
    "Fetch Git refs."
);
capability_method!(
    process_git_rebase,
    "harness.process.git_rebase",
    ["process.write@const=git", "fs.mutate@arg0"],
    "__cap_process_git_rebase(repo: string, base_ref: string) -> GitRebaseReceipt",
    "Rebase a Git checkout."
);
capability_method!(
    process_git_status,
    "harness.process.git_status",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_status(repo: string) -> GitStatusReceipt",
    "Read Git status."
);
capability_method!(
    process_git_conflicts,
    "harness.process.git_conflicts",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_conflicts(repo: string) -> GitConflictsReceipt",
    "Read unresolved Git conflicts."
);
capability_method!(
    process_git_push,
    "harness.process.git_push",
    [
        "process.write@const=git",
        "network.write@arg1",
        "fs.read@arg0"
    ],
    "__cap_process_git_push(repo: string, remote: string, refspec: string, lease?: any) -> GitPushReceipt",
    "Push a Git ref."
);
capability_method!(
    process_git_diff,
    "harness.process.git_diff",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_diff(repo: string, selector?: any) -> GitDiffReceipt",
    "Read a Git diff."
);
capability_method!(
    process_git_merge_base,
    "harness.process.git_merge_base",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_merge_base(repo: string, left: string, right: string) -> GitMergeBaseReceipt",
    "Find a Git merge base."
);
capability_method!(
    process_git_tag_list,
    "harness.process.git_tag_list",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_tag_list(repo: string, options?: dict) -> GitTagListReceipt",
    "List Git tags."
);
capability_method!(
    process_git_describe,
    "harness.process.git_describe",
    ["process.write@const=git", "fs.read@arg0"],
    "__cap_process_git_describe(repo: string, options?: dict) -> GitDescribeReceipt",
    "Describe a Git revision."
);
capability_method!(
    process_git_ls_remote,
    "harness.process.git_ls_remote",
    ["process.write@const=git", "network.read@arg1"],
    "__cap_process_git_ls_remote(repo: string, remote: string, options?: dict) -> GitLsRemoteReceipt",
    "List refs from a Git remote."
);
capability_method!(
    process_list_shells,
    "harness.process.list_shells",
    ["process.read@const=shell-configuration"],
    "__cap_process_list_shells() -> dict",
    "List command shells available through the host."
);
capability_method!(
    process_shell_invocation,
    "harness.process.shell_invocation",
    ["process.read@const=shell-configuration"],
    "__cap_process_shell_invocation(request: dict) -> dict",
    "Resolve a shell command into its program and argument vector."
);

capability_method!(
    runtime_context,
    "harness.runtime.context",
    ["state.read@const=runtime-context"],
    "__cap_runtime_context() -> dict",
    "Read the current logical task and orchestration context."
);
capability_method!(
    runtime_context_values,
    "harness.runtime.context_values",
    ["state.read@const=runtime-context-values"],
    "__cap_runtime_context_values() -> dict",
    "Read the current task-local context values."
);
capability_method!(
    runtime_context_get,
    "harness.runtime.context_get",
    ["state.read@arg0"],
    "__cap_runtime_context_get(key: string, default?: any) -> any",
    "Read one task-local context value."
);
capability_method!(
    runtime_context_set,
    "harness.runtime.context_set",
    ["state.write@arg0"],
    "__cap_runtime_context_set(key: string, value: any) -> any",
    "Set one task-local context value and return its previous value."
);
capability_method!(
    runtime_context_clear,
    "harness.runtime.context_clear",
    ["state.mutate@arg0"],
    "__cap_runtime_context_clear(key: string) -> any",
    "Clear one task-local context value and return its previous value."
);
capability_method!(
    runtime_task,
    "harness.runtime.task",
    ["host.read@const=runtime-task"],
    "__cap_runtime_task() -> string",
    "Read the active task text."
);
capability_method!(
    runtime_pipeline_input,
    "harness.runtime.pipeline_input",
    ["host.read@const=pipeline-input"],
    "__cap_runtime_pipeline_input() -> any",
    "Read the active pipeline input."
);
capability_method!(
    runtime_prompt_content,
    "harness.runtime.prompt_content",
    ["host.read@const=prompt-content"],
    "__cap_runtime_prompt_content() -> list",
    "Read normalized active prompt content."
);
capability_method!(
    runtime_flow_evaluate_invariants,
    "harness.runtime.flow_evaluate_invariants",
    ["fs.read@arg2.path"],
    "__cap_runtime_flow_evaluate_invariants(source: string, slice: dict, options?: dict) -> dict",
    "Evaluate Flow invariants through the runtime predicate engine."
);
capability_method!(
    runtime_store_get,
    "harness.runtime.store_get",
    ["state.read@arg0"],
    "__cap_runtime_store_get(key: string) -> any",
    "Read a value from the run store."
);
capability_method!(
    runtime_store_set,
    "harness.runtime.store_set",
    ["state.write@arg0"],
    "__cap_runtime_store_set(key: string, value: any) -> nil",
    "Write a value to the run store."
);
capability_method!(
    runtime_store_delete,
    "harness.runtime.store_delete",
    ["state.mutate@arg0"],
    "__cap_runtime_store_delete(key: string) -> nil",
    "Delete a value from the run store."
);
capability_method!(
    runtime_store_list,
    "harness.runtime.store_list",
    ["state.read@const=run-store"],
    "__cap_runtime_store_list() -> list",
    "List run-store keys."
);
capability_method!(
    runtime_store_save,
    "harness.runtime.store_save",
    ["state.write@const=run-store"],
    "__cap_runtime_store_save() -> nil",
    "Persist the run store."
);
capability_method!(
    runtime_store_clear,
    "harness.runtime.store_clear",
    ["state.mutate@const=run-store"],
    "__cap_runtime_store_clear() -> nil",
    "Clear the run store."
);
capability_method!(
    runtime_dry_run,
    "harness.runtime.dry_run",
    ["host.read@const=dry-run"],
    "__cap_runtime_dry_run() -> bool",
    "Read whether the active execution is a dry run."
);
capability_method!(
    runtime_approved_plan,
    "harness.runtime.approved_plan",
    ["host.read@const=approved-plan"],
    "__cap_runtime_approved_plan() -> string",
    "Read the host-approved plan."
);
capability_method!(
    runtime_record_run,
    "harness.runtime.record_run",
    ["state.write@arg0.path"],
    "__cap_runtime_record_run(record: dict) -> nil",
    "Record run metadata in the active host."
);
capability_method!(
    runtime_exit,
    "harness.runtime.exit",
    ["process.mutate@const=current-runtime"],
    "__cap_runtime_exit(code?: int) -> never",
    "Terminate the current Harn execution with an exit code."
);
capability_method!(
    runtime_host_capabilities,
    "harness.runtime.host_capabilities",
    ["host.read@const=capability-registry"],
    "__cap_runtime_host_capabilities() -> dict",
    "Inspect the host capabilities installed for this run."
);
capability_method!(
    runtime_host_has,
    "harness.runtime.host_has",
    ["host.read@arg0"],
    "__cap_runtime_host_has(capability: string, operation?: string) -> bool",
    "Test whether the active host supplies a capability operation."
);
capability_method!(
    runtime_sync_mutex_acquire,
    "harness.runtime.sync_mutex_acquire",
    ["state.mutate@arg0"],
    "__cap_runtime_sync_mutex_acquire(key?: string, timeout_ms?: int) -> any",
    "Acquire a runtime-owned named mutex permit."
);
capability_method!(
    runtime_introspection,
    "harness.runtime.introspection",
    ["host.read@const=runtime-tools"],
    "__cap_runtime_introspection() -> dict",
    "Read the runtime introspection tool snapshot."
);

capability_method!(
    interaction_ask,
    "harness.interaction.ask",
    ["host.write@const=human-interaction"],
    "__cap_interaction_ask(question: any, kind?: any) -> string",
    "Ask the user for input through the host."
);
capability_method!(
    interaction_ask_user,
    "harness.interaction.ask_user",
    ["host.write@const=human-question"],
    "__cap_interaction_ask_user(prompt: string, options?: dict) -> any",
    "Ask a typed human-in-the-loop question."
);
capability_method!(
    interaction_request_approval,
    "harness.interaction.request_approval",
    ["host.write@const=human-approval"],
    "__cap_interaction_request_approval(...args: any) -> ApprovalRecord",
    "Request a human approval decision."
);
capability_method!(
    interaction_dual_control,
    "harness.interaction.dual_control",
    ["host.write@const=human-dual-control"],
    "__cap_interaction_dual_control(n: int, m: int, action: closure, approvers?: list) -> dict",
    "Execute an action after an M-of-N approval decision."
);
capability_method!(
    interaction_escalate_to,
    "harness.interaction.escalate_to",
    ["host.write@arg0"],
    "__cap_interaction_escalate_to(role: string, reason: string) -> dict",
    "Escalate work to a human role."
);

capability_method!(
    project_metadata_get,
    "harness.project.metadata_get",
    ["state.read@arg0.dir"],
    "__cap_project_metadata_get(request: dict) -> any",
    "Read project metadata."
);
capability_method!(
    project_metadata_inspect,
    "harness.project.metadata_inspect",
    ["state.read@arg0.dir"],
    "__cap_project_metadata_inspect(request: dict) -> any",
    "Inspect project metadata."
);
capability_method!(
    project_metadata_set,
    "harness.project.metadata_set",
    ["state.mutate@arg0.dir"],
    "__cap_project_metadata_set(request: dict) -> any",
    "Update project metadata."
);
capability_method!(
    project_metadata_save,
    "harness.project.metadata_save",
    ["state.write@arg0.dir"],
    "__cap_project_metadata_save(request: dict) -> any",
    "Persist project metadata."
);
capability_method!(
    project_metadata_stale,
    "harness.project.metadata_stale",
    ["state.read@arg0.dir", "fs.read@arg0.dir"],
    "__cap_project_metadata_stale(request?: dict) -> dict",
    "Test whether project metadata is stale."
);
capability_method!(
    project_metadata_refresh_hashes,
    "harness.project.metadata_refresh_hashes",
    ["state.mutate@arg0.dir"],
    "__cap_project_metadata_refresh_hashes(request: dict) -> any",
    "Refresh project metadata hashes."
);
capability_method!(
    project_metadata_entries,
    "harness.project.metadata_entries",
    ["state.read@arg0.namespace"],
    "__cap_project_metadata_entries(request?: dict) -> list",
    "List local project metadata entries."
);
capability_method!(
    project_metadata_status,
    "harness.project.metadata_status",
    ["state.read@arg0.namespace"],
    "__cap_project_metadata_status(request?: dict) -> dict",
    "Summarize project metadata coverage."
);
capability_method!(
    project_content_hash,
    "harness.project.content_hash",
    ["fs.read@arg0"],
    "__cap_project_content_hash(path: string) -> string",
    "Compute the project metadata content hash for a directory."
);
capability_method!(
    project_path_metadata_get,
    "harness.project.path_metadata_get",
    ["state.read@arg0.path"],
    "__cap_project_path_metadata_get(request: dict) -> any",
    "Read metadata attached to an exact project path."
);
capability_method!(
    project_path_metadata_set,
    "harness.project.path_metadata_set",
    ["state.mutate@arg0.path"],
    "__cap_project_path_metadata_set(request: dict) -> nil",
    "Write metadata attached to an exact project path."
);
capability_method!(
    project_path_metadata_entries,
    "harness.project.path_metadata_entries",
    ["state.read@arg0.namespace"],
    "__cap_project_path_metadata_entries(request?: dict) -> list",
    "List exact-path and directory metadata entries."
);
capability_method!(
    project_scan_directory,
    "harness.project.scan_directory",
    ["fs.read@arg0"],
    "__cap_project_scan_directory(path?: string, options?: dict) -> list",
    "Scan a project directory recursively."
);
capability_method!(
    project_scan,
    "harness.project.scan",
    ["fs.read@arg0"],
    "__cap_project_scan(path?: string, options?: dict) -> dict",
    "Detect the project configuration rooted at a directory."
);
capability_method!(
    project_context_profile,
    "harness.project.context_profile",
    ["fs.read@arg0"],
    "__cap_project_context_profile(path?: string, options?: dict) -> ProjectContextProfile",
    "Resolve the context profile for a project directory."
);
capability_method!(
    project_scan_tree,
    "harness.project.scan_tree",
    ["fs.read@arg0"],
    "__cap_project_scan_tree(path?: string, options?: dict) -> dict",
    "Scan project configuration throughout a directory tree."
);
capability_method!(
    project_fingerprint,
    "harness.project.fingerprint",
    ["fs.read@arg0"],
    "__cap_project_fingerprint(path?: string) -> ProjectFingerprint",
    "Detect the project languages and build-system signals rooted at a path."
);
capability_method!(
    project_walk_tree,
    "harness.project.walk_tree",
    ["fs.read@arg0"],
    "__cap_project_walk_tree(path?: string, options?: dict) -> list",
    "Walk the directories participating in a project scan."
);
capability_method!(
    project_catalog,
    "harness.project.catalog",
    [],
    "__cap_project_catalog() -> list",
    "Return the built-in project signal catalog."
);
capability_method!(
    project_enrich,
    "harness.project.enrich",
    [
        "fs.read@arg0",
        "fs.write@arg0",
        "process.read@dynamic",
        "llm.write@dynamic"
    ],
    "__cap_project_enrich(path?: string, options?: dict) -> dict",
    "Enrich project evidence with the configured model."
);
