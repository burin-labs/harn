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
    stdio_read_stdin,
    "harness.stdio.read_stdin",
    ["stdio.read@const=stdin"],
    "__cap_stdio_read_stdin() -> string",
    "Read the remaining standard input."
);
capability_method!(
    stdio_is_stdin_tty,
    "harness.stdio.is_stdin_tty",
    ["stdio.read@const=terminal"],
    "__cap_stdio_is_stdin_tty() -> bool",
    "Test whether standard input is attached to a terminal."
);
capability_method!(
    stdio_is_stdout_tty,
    "harness.stdio.is_stdout_tty",
    ["stdio.read@const=terminal"],
    "__cap_stdio_is_stdout_tty() -> bool",
    "Test whether standard output is attached to a terminal."
);
capability_method!(
    stdio_is_stderr_tty,
    "harness.stdio.is_stderr_tty",
    ["stdio.read@const=terminal"],
    "__cap_stdio_is_stderr_tty() -> bool",
    "Test whether standard error is attached to a terminal."
);
capability_method!(
    stdio_prompt,
    "harness.stdio.prompt",
    ["stdio.write@const=stdout", "stdio.read@const=stdin"],
    "__cap_stdio_prompt(message: string) -> any",
    "Write a prompt and read one response line."
);
capability_method!(
    stdio_log,
    "harness.stdio.log",
    ["stdio.write@const=stdout"],
    "__cap_stdio_log(message: any) -> nil",
    "Write a Harn-prefixed log line."
);
capability_method!(stdio_progress, "harness.stdio.progress", ["stdio.write@const=stdout"], "__cap_stdio_progress(phase: string, message: string, progress_or_options?: any, total?: int) -> nil", "Write a human-readable progress line.");

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
    term_set_color_mode,
    "harness.term.set_color_mode",
    ["stdio.mutate@const=terminal-style"],
    "__cap_term_set_color_mode(mode: string) -> nil",
    "Set ANSI color handling for this execution."
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
    clock_elapsed,
    "harness.clock.elapsed",
    ["clock.read@const=monotonic"],
    "__cap_clock_elapsed() -> int",
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
    fs_read_lines_append_page_result,
    "harness.fs.read_lines_append_page_result",
    ["fs.read@arg0"],
    "__cap_fs_read_lines_append_page_result(path: string, options?: dict) -> Result<dict, dict>",
    "Incrementally read newline-committed lines without throwing."
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
    fs_workspace_root,
    "harness.fs.workspace_root",
    ["fs.read@const=workspace-root"],
    "__cap_fs_workspace_root() -> string",
    "Read the active project root, falling back to the execution directory."
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
    tools_mcp_roots,
    "harness.tools.mcp_roots",
    ["mcp.read@const=roots"],
    "__cap_tools_mcp_roots() -> list",
    "Read roots exposed to MCP clients."
);
capability_method!(
    tools_mcp_connect,
    "harness.tools.mcp_connect",
    ["mcp.mutate@arg0", "process.write@arg0"],
    "__cap_tools_mcp_connect(command: string, args?: list, options?: dict) -> mcp_client",
    "Connect to an MCP server over stdio."
);
capability_method!(
    tools_mcp_bootstrap,
    "harness.tools.mcp_bootstrap",
    [
        "mcp.mutate@arg0",
        "process.write@dynamic",
        "network.write@dynamic"
    ],
    "__cap_tools_mcp_bootstrap(session_id: string, specs?: list|nil) -> dict",
    "Bootstrap and attach a declared set of MCP servers to an agent session."
);
capability_method!(
    tools_mcp_ensure_active,
    "harness.tools.mcp_ensure_active",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_ensure_active(name: string) -> mcp_client",
    "Acquire a registered MCP server."
);
capability_method!(
    tools_mcp_release,
    "harness.tools.mcp_release",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_release(name: string) -> nil",
    "Release a registered MCP server."
);
capability_method!(
    tools_mcp_registry_status,
    "harness.tools.mcp_registry_status",
    ["mcp.read@const=registry"],
    "__cap_tools_mcp_registry_status() -> list",
    "Inspect registered MCP servers."
);
capability_method!(
    tools_mcp_reauth_expired,
    "harness.tools.mcp_reauth_expired",
    ["mcp.mutate@const=oauth"],
    "__cap_tools_mcp_reauth_expired() -> list",
    "Refresh expired MCP OAuth sessions."
);
capability_method!(
    tools_mcp_server_card,
    "harness.tools.mcp_server_card",
    ["mcp.read@arg0", "network.read@arg0", "fs.read@arg0"],
    "__cap_tools_mcp_server_card(target: string) -> dict",
    "Load an MCP Server Card."
);
capability_method!(
    tools_mcp_list_tools,
    "harness.tools.mcp_list_tools",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_list_tools(client: mcp_client) -> list",
    "List tools exposed by an MCP server."
);
capability_method!(
    tools_mcp_call,
    "harness.tools.mcp_call",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_call(client: mcp_client, tool: string, arguments?: dict) -> any",
    "Call an MCP tool."
);
capability_method!(
    tools_mcp_server_info,
    "harness.tools.mcp_server_info",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_server_info(client: mcp_client) -> dict",
    "Inspect an MCP connection."
);
capability_method!(
    tools_mcp_disconnect,
    "harness.tools.mcp_disconnect",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_disconnect(client: mcp_client) -> nil",
    "Disconnect an MCP client."
);
capability_method!(
    tools_mcp_list_resources,
    "harness.tools.mcp_list_resources",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_list_resources(client: mcp_client) -> list",
    "List MCP resources."
);
capability_method!(
    tools_mcp_read_resource,
    "harness.tools.mcp_read_resource",
    ["mcp.read@arg1"],
    "__cap_tools_mcp_read_resource(client: mcp_client, uri: string) -> any",
    "Read an MCP resource."
);
capability_method!(
    tools_mcp_list_resource_templates,
    "harness.tools.mcp_list_resource_templates",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_list_resource_templates(client: mcp_client) -> list",
    "List MCP resource templates."
);
capability_method!(
    tools_mcp_list_prompts,
    "harness.tools.mcp_list_prompts",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_list_prompts(client: mcp_client) -> list",
    "List MCP prompts."
);
capability_method!(
    tools_mcp_get_prompt,
    "harness.tools.mcp_get_prompt",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_get_prompt(client: mcp_client, name: string, arguments?: dict) -> dict",
    "Render an MCP prompt."
);
capability_method!(
    tools_mcp_tools,
    "harness.tools.mcp_tools",
    ["mcp.write@const=server"],
    "__cap_tools_mcp_tools(registry: dict) -> nil",
    "Expose a tool registry from an MCP server."
);
capability_method!(
    tools_mcp_server_metadata,
    "harness.tools.mcp_server_metadata",
    ["mcp.write@const=server"],
    "__cap_tools_mcp_server_metadata(metadata: dict) -> nil",
    "Set MCP server metadata."
);
capability_method!(
    tools_mcp_resource,
    "harness.tools.mcp_resource",
    ["mcp.write@const=server"],
    "__cap_tools_mcp_resource(resource: dict) -> nil",
    "Expose a static MCP resource."
);
capability_method!(
    tools_mcp_resource_template,
    "harness.tools.mcp_resource_template",
    ["mcp.write@const=server"],
    "__cap_tools_mcp_resource_template(resource: dict) -> nil",
    "Expose an MCP resource template."
);
capability_method!(
    tools_mcp_prompt,
    "harness.tools.mcp_prompt",
    ["mcp.write@const=server"],
    "__cap_tools_mcp_prompt(prompt: dict) -> nil",
    "Expose an MCP prompt."
);
capability_method!(
    tools_mcp_elicit,
    "harness.tools.mcp_elicit",
    ["mcp.mutate@const=client"],
    "__cap_tools_mcp_elicit(request: dict) -> dict",
    "Request structured input from an MCP client."
);
capability_method!(
    tools_mcp_client_roots,
    "harness.tools.mcp_client_roots",
    ["mcp.read@const=client"],
    "__cap_tools_mcp_client_roots() -> list",
    "Request roots from an MCP client."
);
capability_method!(
    tools_mcp_report_progress,
    "harness.tools.mcp_report_progress",
    ["mcp.write@const=client"],
    "__cap_tools_mcp_report_progress(progress: float|int, options?: dict) -> bool",
    "Report progress to an MCP client."
);
capability_method!(
    tools_mcp_host_spawn,
    "harness.tools.mcp_host_spawn",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_host_spawn(spec: dict, options?: dict) -> string",
    "Start a supervised MCP server."
);
capability_method!(
    tools_mcp_host_tools,
    "harness.tools.mcp_host_tools",
    ["mcp.read@arg0"],
    "__cap_tools_mcp_host_tools(name: string) -> list",
    "List tools from a supervised MCP server."
);
capability_method!(
    tools_mcp_host_call,
    "harness.tools.mcp_host_call",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_host_call(name: string, tool: string, arguments?: dict) -> any",
    "Call a tool on a supervised MCP server."
);
capability_method!(
    tools_mcp_host_stop,
    "harness.tools.mcp_host_stop",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_host_stop(name: string) -> nil",
    "Stop a supervised MCP server."
);
capability_method!(
    tools_mcp_host_reload,
    "harness.tools.mcp_host_reload",
    ["mcp.mutate@arg0"],
    "__cap_tools_mcp_host_reload(name: string) -> nil",
    "Reload a supervised MCP server."
);
capability_method!(
    tools_mcp_host_discover,
    "harness.tools.mcp_host_discover",
    ["mcp.read@const=host"],
    "__cap_tools_mcp_host_discover() -> list",
    "Discover supervised MCP servers."
);
capability_method!(
    tools_mcp_host_status,
    "harness.tools.mcp_host_status",
    ["mcp.read@const=host"],
    "__cap_tools_mcp_host_status() -> list",
    "Inspect supervised MCP server status."
);
