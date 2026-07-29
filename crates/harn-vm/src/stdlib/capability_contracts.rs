//! Canonical source contracts for capability methods.
//!
//! Runtime handlers remain free to use direct Rust implementations or hidden
//! builtins. The language surface is declared once here and projected into
//! typing, effect analysis, policy, receipts, and documentation through the
//! builtin manifest.

use super::macros::harn_capability_method as capability_method;

capability_method!(
    stdio_print,
    "harness.stdio.print",
    ["stdio.write@const=stdout"],
    "__cap_stdio_print(value?: any) -> nil",
    "Write a value to standard output."
);
capability_method!(
    stdio_println,
    "harness.stdio.println",
    ["stdio.write@const=stdout"],
    "__cap_stdio_println(value?: any) -> nil",
    "Write a value and newline to standard output."
);
capability_method!(
    stdio_eprint,
    "harness.stdio.eprint",
    ["stdio.write@const=stderr"],
    "__cap_stdio_eprint(value?: any) -> nil",
    "Write a value to standard error."
);
capability_method!(
    stdio_eprintln,
    "harness.stdio.eprintln",
    ["stdio.write@const=stderr"],
    "__cap_stdio_eprintln(value?: any) -> nil",
    "Write a value and newline to standard error."
);
capability_method!(
    stdio_read_line,
    "harness.stdio.read_line",
    ["stdio.read@const=stdin"],
    "__cap_stdio_read_line(options?: dict) -> any",
    "Read one line from standard input."
);
capability_method!(
    stdio_prompt,
    "harness.stdio.prompt",
    ["stdio.write@const=stdout", "stdio.read@const=stdin"],
    "__cap_stdio_prompt(message: string) -> any",
    "Write a prompt and read one response line."
);

capability_method!(
    term_width,
    "harness.term.width",
    ["stdio.read@const=terminal"],
    "__cap_term_width() -> int",
    "Read the terminal width."
);
capability_method!(
    term_height,
    "harness.term.height",
    ["stdio.read@const=terminal"],
    "__cap_term_height() -> int",
    "Read the terminal height."
);
capability_method!(
    term_read_password,
    "harness.term.read_password",
    ["stdio.write@const=stderr", "stdio.read@const=stdin"],
    "__cap_term_read_password(prompt?: string) -> string",
    "Read a password without echo."
);
capability_method!(
    term_is_tty,
    "harness.term.is_tty",
    ["stdio.read@arg0"],
    "__cap_term_is_tty(stream: string) -> bool",
    "Return whether a standard stream is attached to a terminal."
);

capability_method!(
    clock_now_ms,
    "harness.clock.now_ms",
    ["clock.read@const=wall"],
    "__cap_clock_now_ms() -> int",
    "Read wall-clock Unix time in milliseconds."
);
capability_method!(
    clock_timestamp,
    "harness.clock.timestamp",
    ["clock.read@const=wall"],
    "__cap_clock_timestamp() -> float",
    "Read wall-clock Unix time in seconds."
);
capability_method!(
    clock_monotonic_ms,
    "harness.clock.monotonic_ms",
    ["clock.read@const=monotonic"],
    "__cap_clock_monotonic_ms() -> int",
    "Read monotonic elapsed milliseconds."
);
capability_method!(
    clock_sleep_ms,
    "harness.clock.sleep_ms",
    ["clock.observe@const=monotonic"],
    "__cap_clock_sleep_ms(ms: int) -> nil",
    "Suspend for a duration in milliseconds."
);
capability_method!(
    clock_date_iso,
    "harness.clock.date_iso",
    ["clock.read@const=wall"],
    "__cap_clock_date_iso() -> string",
    "Read wall time as an RFC 3339 timestamp."
);
capability_method!(
    clock_now,
    "harness.clock.now",
    ["clock.read@const=wall"],
    "__cap_clock_now() -> dict",
    "Read wall time as a structured UTC date."
);

capability_method!(
    fs_read_text,
    "harness.fs.read_text",
    ["fs.read@arg0"],
    "__cap_fs_read_text(path: string) -> string",
    "Read a UTF-8 file."
);
capability_method!(
    fs_read_text_result,
    "harness.fs.read_text_result",
    ["fs.read@arg0"],
    "__cap_fs_read_text_result(path: string) -> Result<string, dict>",
    "Read a UTF-8 file without throwing."
);
capability_method!(
    fs_read_bytes,
    "harness.fs.read_bytes",
    ["fs.read@arg0"],
    "__cap_fs_read_bytes(path: string) -> bytes",
    "Read a binary file."
);
capability_method!(
    fs_write_text,
    "harness.fs.write_text",
    ["fs.write@arg0"],
    "__cap_fs_write_text(path: string, content: string) -> nil",
    "Write a UTF-8 file."
);
capability_method!(
    fs_write_bytes,
    "harness.fs.write_bytes",
    ["fs.write@arg0"],
    "__cap_fs_write_bytes(path: string, content: bytes) -> nil",
    "Write a binary file."
);
capability_method!(
    fs_replace_text,
    "harness.fs.replace_text",
    ["fs.mutate@arg0"],
    "__cap_fs_replace_text(path: string, content: string, options?: dict) -> dict",
    "Conditionally replace a UTF-8 file."
);
capability_method!(fs_replace_text_result, "harness.fs.replace_text_result", ["fs.mutate@arg0"], "__cap_fs_replace_text_result(path: string, content: string, options?: dict) -> Result<dict, dict>", "Conditionally replace a UTF-8 file without throwing.");
capability_method!(
    fs_replace_bytes,
    "harness.fs.replace_bytes",
    ["fs.mutate@arg0"],
    "__cap_fs_replace_bytes(path: string, content: bytes, options?: dict) -> dict",
    "Conditionally replace a binary file."
);
capability_method!(fs_replace_bytes_result, "harness.fs.replace_bytes_result", ["fs.mutate@arg0"], "__cap_fs_replace_bytes_result(path: string, content: bytes, options?: dict) -> Result<dict, dict>", "Conditionally replace a binary file without throwing.");
capability_method!(
    fs_exists,
    "harness.fs.exists",
    ["fs.read@arg0"],
    "__cap_fs_exists(path: string) -> bool",
    "Test whether a path exists."
);
capability_method!(
    fs_status,
    "harness.fs.status",
    ["fs.read@arg0"],
    "__cap_fs_status(path: string, access?: string) -> dict",
    "Read typed path status."
);
capability_method!(
    fs_delete,
    "harness.fs.delete",
    ["fs.mutate@arg0"],
    "__cap_fs_delete(path: string) -> nil",
    "Delete a file."
);
capability_method!(
    fs_append,
    "harness.fs.append",
    ["fs.write@arg0"],
    "__cap_fs_append(path: string, content: string) -> nil",
    "Append text to a file."
);
capability_method!(
    fs_append_locked,
    "harness.fs.append_locked",
    ["fs.write@arg0"],
    "__cap_fs_append_locked(path: string, content: string, options?: dict) -> nil",
    "Append text while holding an advisory lock."
);
capability_method!(
    fs_list_dir,
    "harness.fs.list_dir",
    ["fs.read@arg0"],
    "__cap_fs_list_dir(path?: string) -> list",
    "List directory entries."
);
capability_method!(
    fs_mkdir,
    "harness.fs.mkdir",
    ["fs.write@arg0"],
    "__cap_fs_mkdir(path: string, recursive?: bool) -> nil",
    "Create a directory."
);
capability_method!(
    fs_copy,
    "harness.fs.copy",
    ["fs.read@arg0", "fs.write@arg1"],
    "__cap_fs_copy(source: string, destination: string) -> nil",
    "Copy a file."
);
capability_method!(
    fs_temp_dir,
    "harness.fs.temp_dir",
    ["fs.read@const=system-temp"],
    "__cap_fs_temp_dir() -> string",
    "Read the system temporary directory."
);
capability_method!(
    fs_workspace_temp_dir,
    "harness.fs.workspace_temp_dir",
    ["fs.read@const=workspace-temp"],
    "__cap_fs_workspace_temp_dir() -> string",
    "Read the workspace temporary directory."
);
capability_method!(
    fs_mkdtemp,
    "harness.fs.mkdtemp",
    ["fs.write@const=system-temp"],
    "__cap_fs_mkdtemp(prefix?: string) -> string",
    "Create a unique system temporary directory."
);
capability_method!(
    fs_mkdtemp_in_workspace,
    "harness.fs.mkdtemp_in_workspace",
    ["fs.write@const=workspace-temp"],
    "__cap_fs_mkdtemp_in_workspace(prefix?: string) -> string",
    "Create a unique workspace temporary directory."
);
capability_method!(
    fs_stat,
    "harness.fs.stat",
    ["fs.read@arg0"],
    "__cap_fs_stat(path: string) -> dict",
    "Read file metadata."
);
capability_method!(
    fs_rename,
    "harness.fs.rename",
    ["fs.mutate@arg0", "fs.write@arg1"],
    "__cap_fs_rename(source: string, destination: string) -> nil",
    "Move or rename a file."
);
capability_method!(
    fs_read_lines,
    "harness.fs.read_lines",
    ["fs.read@arg0"],
    "__cap_fs_read_lines(path: string) -> list",
    "Read a UTF-8 file as lines."
);
capability_method!(
    fs_read_lines_page_result,
    "harness.fs.read_lines_page_result",
    ["fs.read@arg0"],
    "__cap_fs_read_lines_page_result(path: string, options?: dict) -> Result<dict, dict>",
    "Read a page of lines without throwing."
);
capability_method!(
    fs_walk,
    "harness.fs.walk",
    ["fs.read@arg0"],
    "__cap_fs_walk(path: string, options?: dict) -> list",
    "Walk a directory tree."
);
capability_method!(
    fs_glob,
    "harness.fs.glob",
    ["fs.read@arg1"],
    "__cap_fs_glob(pattern: string, base_or_options?: any, options?: dict) -> list",
    "Match filesystem paths."
);
capability_method!(
    fs_find_text,
    "harness.fs.find_text",
    ["fs.read@arg0"],
    "__cap_fs_find_text(root: string, pattern: string, options?: dict) -> any",
    "Search files for text."
);
capability_method!(
    fs_find_evidence,
    "harness.fs.find_evidence",
    ["fs.read@each0"],
    "__cap_fs_find_evidence(roots: list, patterns: list, options?: dict) -> any",
    "Search files for evidence patterns."
);
capability_method!(
    fs_package_snapshot_open,
    "harness.fs.package_snapshot_open",
    ["fs.read@arg0", "state.write@const=package-snapshot"],
    "__cap_fs_package_snapshot_open(root: string) -> dict",
    "Open an immutable package snapshot handle."
);
capability_method!(
    fs_package_snapshot_close,
    "harness.fs.package_snapshot_close",
    ["state.mutate@const=package-snapshot"],
    "__cap_fs_package_snapshot_close(handle: any) -> nil",
    "Close a package snapshot handle."
);
capability_method!(
    fs_cwd,
    "harness.fs.cwd",
    ["fs.read@const=execution-root"],
    "__cap_fs_cwd() -> string",
    "Read the active execution directory."
);
capability_method!(
    fs_source_dir,
    "harness.fs.source_dir",
    ["fs.read@const=source-root"],
    "__cap_fs_source_dir() -> string",
    "Read the directory containing the active source module."
);
capability_method!(
    fs_project_root,
    "harness.fs.project_root",
    ["fs.read@const=project-root"],
    "__cap_fs_project_root() -> string?",
    "Read the active Harn project root when one is available."
);
capability_method!(
    fs_home_dir,
    "harness.fs.home_dir",
    ["fs.read@const=home"],
    "__cap_fs_home_dir() -> string",
    "Read the current user's home directory."
);
capability_method!(
    fs_runtime_paths,
    "harness.fs.runtime_paths",
    ["fs.read@const=runtime-roots"],
    "__cap_fs_runtime_paths() -> {execution_root: string, asset_root: string, state_root: string, run_root: string, worktree_root: string}",
    "Resolve the runtime-owned execution, asset, state, run, and worktree roots."
);
capability_method!(
    fs_render_prompt,
    "harness.fs.render_prompt",
    ["fs.read@arg0"],
    "__cap_fs_render_prompt(path: string, bindings?: dict) -> string",
    "Load and render a prompt asset."
);
capability_method!(
    fs_render_prompt_with_provenance,
    "harness.fs.render_prompt_with_provenance",
    ["fs.read@arg0"],
    "__cap_fs_render_prompt_with_provenance(path: string, bindings?: dict) -> dict",
    "Load and render a prompt asset together with source provenance."
);
capability_method!(
    fs_render_template,
    "harness.fs.render_template",
    ["fs.read@dynamic"],
    "__cap_fs_render_template(template: string, bindings?: dict) -> string",
    "Render inline prompt-template source, resolving referenced assets through this filesystem capability."
);

capability_method!(
    env_get,
    "harness.env.get",
    ["env.read@arg0"],
    "__cap_env_get(name: string) -> string?",
    "Read an environment variable."
);
capability_method!(
    env_get_or,
    "harness.env.get_or",
    ["env.read@arg0"],
    "__cap_env_get_or(name: string, default: any) -> any",
    "Read an environment variable with a default."
);

capability_method!(
    random_f64,
    "harness.random.f64",
    ["random.read"],
    "__cap_random_f64() -> float",
    "Generate a random floating-point value."
);
capability_method!(
    random_u64,
    "harness.random.u64",
    ["random.read"],
    "__cap_random_u64() -> int",
    "Generate a random non-negative integer."
);
capability_method!(
    random_range,
    "harness.random.range",
    ["random.read"],
    "__cap_random_range(min: int, max: int) -> int",
    "Generate a random integer in a range."
);
capability_method!(
    random_choice,
    "harness.random.choice",
    ["random.read"],
    "__cap_random_choice(values: list) -> any",
    "Choose one random list element."
);
capability_method!(
    random_shuffle,
    "harness.random.shuffle",
    ["random.read"],
    "__cap_random_shuffle(values: list) -> list",
    "Return a shuffled list."
);
capability_method!(
    random_uuid,
    "harness.random.uuid",
    ["random.read"],
    "__cap_random_uuid() -> string",
    "Generate a random version 4 UUID."
);
capability_method!(
    random_uuid_v7,
    "harness.random.uuid_v7",
    ["clock.read@const=wall", "random.read"],
    "__cap_random_uuid_v7() -> string",
    "Generate a time-ordered version 7 UUID."
);
capability_method!(
    random_bytes,
    "harness.random.bytes",
    ["random.read"],
    "__cap_random_bytes(length: int) -> bytes",
    "Generate cryptographically secure random bytes."
);

capability_method!(
    channels_append,
    "harness.channels.append",
    ["channel.write@arg0"],
    "__cap_channels_append(name: string, payload: any, options?: dict) -> dict",
    "Append one event to a durable transcript channel."
);
capability_method!(
    channels_events,
    "harness.channels.events",
    ["channel.read@arg0"],
    "__cap_channels_events(name: string, options?: dict) -> list",
    "Read events from a durable transcript channel."
);
capability_method!(
    channels_subscribe,
    "harness.channels.subscribe",
    ["channel.observe@arg0"],
    "__cap_channels_subscribe(name: string, options?: dict) -> stream",
    "Subscribe to a durable transcript channel."
);
capability_method!(
    channels_consumer_cursor,
    "harness.channels.consumer_cursor",
    ["channel.read@arg0"],
    "__cap_channels_consumer_cursor(name: string, consumer_id: string, options?: dict) -> int?",
    "Read one durable channel consumer cursor."
);
capability_method!(
    channels_ack,
    "harness.channels.ack",
    ["channel.write@arg0"],
    "__cap_channels_ack(name: string, consumer_id: string, cursor: int, options?: dict) -> dict",
    "Advance one durable channel consumer cursor."
);
capability_method!(
    channels_flush_aggregations,
    "harness.channels.flush_aggregations",
    ["channel.mutate@const=trigger-aggregations"],
    "__cap_channels_flush_aggregations() -> nil",
    "Flush expired trigger aggregation windows."
);

capability_method!(
    tools_list_registered,
    "harness.tools.list_registered",
    ["tool.read@const=registry"],
    "__cap_tools_list_registered() -> list",
    "List tools registered with the active host."
);
capability_method!(
    tools_invoke,
    "harness.tools.invoke",
    ["tool.mutate@arg0"],
    "__cap_tools_invoke(name: string, args?: any) -> any",
    "Invoke a registered host tool."
);
capability_method!(tools_dispatch_agent_call, "harness.tools.dispatch_agent_call", ["tool.mutate@dynamic"], "__cap_tools_dispatch_agent_call(call: dict, tools?: {_type: \"tool_registry\", tools: list}?, options?: dict?) -> dict", "Dispatch one parsed agent tool call.");
capability_method!(tools_dispatch_agent_batch, "harness.tools.dispatch_agent_batch", ["tool.mutate@dynamic"], "__cap_tools_dispatch_agent_batch(calls: list, tools?: {_type: \"tool_registry\", tools: list}?, options?: dict?) -> list", "Dispatch a batch of parsed agent tool calls.");

capability_method!(
    net_get,
    "harness.net.get",
    ["network.read@arg0"],
    "__cap_net_get(url: string, options?: dict) -> dict",
    "Send an HTTP GET request."
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
    net_unix_socket_json_request,
    "harness.net.unix_socket_json_request",
    ["network.mutate@arg0"],
    "__cap_net_unix_socket_json_request(path: string, request: any, options?: dict) -> dict",
    "Exchange one JSON-line request over a Unix domain socket."
);

capability_method!(
    process_run,
    "harness.process.run",
    ["process.write@arg0.program", "process.write@arg0.command"],
    "__cap_process_run(command: dict) -> dict",
    "Run a structured child process and capture its result."
);
capability_method!(
    process_default_shell,
    "harness.process.default_shell",
    ["process.read@const=shell-configuration"],
    "__cap_process_default_shell() -> dict",
    "Read the host's selected command shell."
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
    "__cap_interaction_request_approval(...args: any) -> dict",
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
    "__cap_project_context_profile(path?: string, options?: dict) -> dict",
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
    ["fs.read@arg0", "llm.write@dynamic"],
    "__cap_project_enrich(path?: string, options?: dict) -> dict",
    "Enrich project evidence with the configured model."
);
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
    "__cap_testing_respond(capability: string, method: string, value: any) -> nil",
    "Queue one successful response for a closed capability method."
);
capability_method!(
    testing_respond_error,
    "harness.testing.respond_error",
    ["state.write@arg0"],
    "__cap_testing_respond_error(capability: string, method: string, message: string) -> nil",
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
    "__cap_postgres_connect(url: string, options?: dict) -> dict",
    "Open a PostgreSQL connection handle."
);
capability_method!(
    postgres_pool,
    "harness.postgres.pool",
    ["network.write@const=postgres"],
    "__cap_postgres_pool(url: string, options?: dict) -> dict",
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

capability_method!(
    llm_catalog,
    "harness.llm.catalog",
    ["llm.read@const=catalog"],
    "__cap_llm_catalog() -> list",
    "Read the model catalog."
);
capability_method!(
    llm_catalog_refresh,
    "harness.llm.catalog_refresh",
    ["llm.mutate@const=catalog"],
    "__cap_llm_catalog_refresh() -> list",
    "Refresh and read the model catalog."
);
capability_method!(
    llm_providers,
    "harness.llm.providers",
    ["llm.read@const=providers"],
    "__cap_llm_providers() -> list",
    "Read provider status."
);
capability_method!(
    llm_call,
    "harness.llm.call",
    ["llm.write@arg2.provider", "llm.write@arg2.model"],
    "__cap_llm_call(prompt: string, system?: string, options?: dict) -> dict",
    "Execute one routed model call."
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
    agent_session_flush,
    "harness.agent.session_flush",
    ["state.mutate@arg0"],
    "__cap_agent_session_flush(session_id: string) -> nil",
    "Flush a live agent transcript."
);
capability_method!(
    agent_emit_event,
    "harness.agent.emit_event",
    ["state.write@arg0"],
    "__cap_agent_emit_event(session_id: string, event_type: string, payload: dict) -> nil",
    "Append an agent session event."
);
capability_method!(
    agent_session_init,
    "harness.agent.session_init",
    ["state.mutate@const=agent-sessions"],
    "__cap_agent_session_init(message: string, system?: string|nil, options?: dict|nil) -> string",
    "Initialize an agent execution session."
);
capability_method!(
    agent_session_finalize,
    "harness.agent.session_finalize",
    ["state.mutate@arg0"],
    "__cap_agent_session_finalize(session_id: string, status: dict) -> dict",
    "Finalize an agent execution session."
);
capability_method!(
    agent_session_messages,
    "harness.agent.session_messages",
    ["state.read@arg0"],
    "__cap_agent_session_messages(session_id: string) -> list",
    "Read an agent session's messages."
);
capability_method!(
    agent_session_record_assistant,
    "harness.agent.session_record_assistant",
    ["state.write@arg0"],
    "__cap_agent_session_record_assistant(session_id: string, llm_result: dict) -> nil",
    "Record an assistant result."
);
capability_method!(
    agent_session_pop_last_assistant,
    "harness.agent.session_pop_last_assistant",
    ["state.mutate@arg0"],
    "__cap_agent_session_pop_last_assistant(session_id: string) -> dict",
    "Remove and return the last assistant message."
);
capability_method!(
    agent_session_record_tool_results,
    "harness.agent.session_record_tool_results",
    ["state.write@arg0"],
    "__cap_agent_session_record_tool_results(session_id: string, dispatch: dict) -> nil",
    "Record dispatched tool results."
);
capability_method!(
    agent_session_pair_orphaned_tool_use,
    "harness.agent.session_pair_orphaned_tool_use",
    ["state.mutate@arg0"],
    "__cap_agent_session_pair_orphaned_tool_use(session_id: string, feedback: string) -> int",
    "Pair orphaned tool calls with synthetic results."
);
capability_method!(
    agent_session_record_usage,
    "harness.agent.session_record_usage",
    ["state.write@arg0"],
    "__cap_agent_session_record_usage(session_id: string, llm_result: dict) -> dict",
    "Record model usage."
);
capability_method!(agent_reminder_providers_fire, "harness.agent.reminder_providers_fire", ["state.mutate@arg0"], "__cap_agent_reminder_providers_fire(session_id: string, event: string, payload?: dict|nil, options?: dict|nil) -> dict", "Run registered reminder providers for a session event.");
capability_method!(
    agent_session_drain_feedback,
    "harness.agent.session_drain_feedback",
    ["state.mutate@arg0"],
    "__cap_agent_session_drain_feedback(session_id: string) -> list",
    "Drain queued session feedback."
);
capability_method!(
    agent_session_drain_command_updates,
    "harness.agent.session_drain_command_updates",
    ["state.mutate@arg0"],
    "__cap_agent_session_drain_command_updates(session_id: string) -> list",
    "Drain queued command updates."
);
capability_method!(
    agent_session_await_inbox,
    "harness.agent.session_await_inbox",
    ["state.observe@arg0"],
    "__cap_agent_session_await_inbox(session_id: string, timeout_ms: int) -> bool",
    "Wait for agent inbox activity."
);
capability_method!(agent_session_drain_host_injections, "harness.agent.session_drain_host_injections", ["state.mutate@arg0"], "__cap_agent_session_drain_host_injections(session_id: string, delivery: string, seam: string) -> list", "Drain host injections at a delivery seam.");
capability_method!(
    agent_session_totals,
    "harness.agent.session_totals",
    ["state.read@arg0"],
    "__cap_agent_session_totals(session_id: string) -> dict",
    "Read aggregate session totals."
);
capability_method!(
    agent_session_inject_feedback,
    "harness.agent.session_inject_feedback",
    ["state.write@arg0"],
    "__cap_agent_session_inject_feedback(session_id: string, kind: string, content: string) -> nil",
    "Inject session feedback."
);
capability_method!(
    agent_session_inject_reminder,
    "harness.agent.session_inject_reminder",
    ["state.write@arg0"],
    "__cap_agent_session_inject_reminder(session_id: string, options: dict) -> string",
    "Inject a session reminder."
);
capability_method!(agent_session_post_event, "harness.agent.session_post_event", ["state.write@arg0"], "__cap_agent_session_post_event(session_id: string, kind: string, content: string, source?: string|nil) -> nil", "Post a session event.");
capability_method!(
    agent_session_apply_reminder_post_turn,
    "harness.agent.session_apply_reminder_post_turn",
    ["state.mutate@arg0"],
    "__cap_agent_session_apply_reminder_post_turn(session_id: string, turn?: dict|nil) -> dict",
    "Apply post-turn reminder policy."
);
capability_method!(
    agent_session_set_active_skills,
    "harness.agent.session_set_active_skills",
    ["state.write@arg0"],
    "__cap_agent_session_set_active_skills(session_id: string, skills: list) -> nil",
    "Set active session skills."
);
capability_method!(
    agent_session_active_skills,
    "harness.agent.session_active_skills",
    ["state.read@arg0"],
    "__cap_agent_session_active_skills(session_id: string) -> list",
    "Read active session skills."
);
capability_method!(agent_session_record_skill_event, "harness.agent.session_record_skill_event", ["state.write@arg0"], "__cap_agent_session_record_skill_event(session_id: string, kind: string, metadata: dict) -> nil", "Record a skill lifecycle event.");
capability_method!(
    agent_session_compact_if_needed,
    "harness.agent.session_compact_if_needed",
    ["state.mutate@arg0"],
    "__cap_agent_session_compact_if_needed(session_id: string, options: dict) -> dict",
    "Compact a session when its policy requires it."
);
capability_method!(agent_session_replace_messages, "harness.agent.session_replace_messages", ["state.mutate@arg0"], "__cap_agent_session_replace_messages(session_id: string, messages: list, summary?: any) -> nil", "Replace session messages after compaction.");
capability_method!(
    agent_budget_pre_call_blocked,
    "harness.agent.budget_pre_call_blocked",
    ["state.read@arg0"],
    "__cap_agent_budget_pre_call_blocked(session_id: string, envelope: dict) -> bool",
    "Evaluate the session budget before a model call."
);
capability_method!(
    agent_record_native_tool_fallback,
    "harness.agent.record_native_tool_fallback",
    ["state.write@arg0"],
    "__cap_agent_record_native_tool_fallback(session_id: string, payload: dict) -> nil",
    "Record a native-tool fallback."
);
capability_method!(
    agent_record_compaction,
    "harness.agent.record_compaction",
    ["state.write@arg0"],
    "__cap_agent_record_compaction(session_id: string, payload: dict) -> nil",
    "Record a compaction event."
);
capability_method!(
    agent_session_project_turn,
    "harness.agent.session_project_turn",
    ["state.read@arg0"],
    "__cap_agent_session_project_turn(session_id: string, options?: dict|nil) -> dict",
    "Project the current session turn."
);
capability_method!(
    agent_session_claim_tool_format,
    "harness.agent.session_claim_tool_format",
    ["state.mutate@arg0"],
    "__cap_agent_session_claim_tool_format(session_id: string, tool_format: string) -> dict",
    "Claim the session tool format."
);
capability_method!(
    agent_daemon_snapshot,
    "harness.agent.daemon_snapshot",
    ["state.read@arg0"],
    "__cap_agent_daemon_snapshot(session_id: string, options: dict) -> dict",
    "Read an agent daemon snapshot."
);
capability_method!(
    agent_session_push_bridge_injection,
    "harness.agent.session_push_bridge_injection",
    ["state.write@arg0"],
    "__cap_agent_session_push_bridge_injection(session_id: string, options: dict) -> string",
    "Queue a bridge injection."
);
capability_method!(
    agent_session_push_user_message,
    "harness.agent.session_push_user_message",
    ["state.write@arg0"],
    "__cap_agent_session_push_user_message(session_id: string, options: dict) -> string",
    "Queue a user message."
);
capability_method!(
    agent_session_pending_injections,
    "harness.agent.session_pending_injections",
    ["state.read@arg0"],
    "__cap_agent_session_pending_injections(session_id: string) -> list",
    "Read pending session injections."
);
capability_method!(
    agent_session_revoke_reminder,
    "harness.agent.session_revoke_reminder",
    ["state.mutate@arg0"],
    "__cap_agent_session_revoke_reminder(session_id: string, reminder_id: string) -> bool",
    "Revoke a pending reminder."
);
capability_method!(
    agent_session_drain_bridge_injections,
    "harness.agent.session_drain_bridge_injections",
    ["state.mutate@arg0"],
    "__cap_agent_session_drain_bridge_injections(session_id: string, checkpoint: dict) -> list",
    "Drain bridge injections at a checkpoint."
);
capability_method!(
    agent_daemon_wait,
    "harness.agent.daemon_wait",
    ["state.observe@arg0"],
    "__cap_agent_daemon_wait(session_id: string, timeout_ms: int) -> dict",
    "Wait for agent daemon activity."
);
capability_method!(
    agent_capture_events,
    "harness.agent.capture_events",
    ["state.observe@arg0"],
    "__cap_agent_capture_events(session_id: string, body: closure) -> dict",
    "Capture typed events emitted while a session body runs."
);

capability_method!(
    agent_open,
    "harness.agent.open",
    ["state.mutate@const=agent-sessions"],
    "__cap_agent_open(id?: string, opts?: dict) -> any",
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
    "Read the current agent session identifier."
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
