//! Connector, host, tool, and shell-facing builtin signatures.

use super::shapes::TOOL_REGISTRY;
use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT,
    TY_DICT_OR_NIL, TY_INT, TY_LIST, TY_NIL, TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};

const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);
const TY_TOOL_REGISTRY: Ty = Ty::Union(&[TY_DICT, TY_CLOSURE]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // Tool-hook catalogue primitive (TH-01 / epic #1884). `ToolRule` and
    // `Catalogue` are tagged dicts; the registry composes catalogues so
    // `preset_run_command` (TH-02) and downstream mode callbacks can layer
    // matching/rewrite logic on top without redefining the data model.
    BuiltinSignature::simple("catalogue", &[Param::new("config", TY_DICT)], TY_DICT),
    BuiltinSignature::simple(
        "connector_call",
        &[
            Param::new("provider", TY_STRING),
            Param::new("method", TY_STRING),
            Param::optional("params", TY_DICT),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple(
        "connector_shared_verify_jwt_inline",
        &[
            Param::new("token", TY_STRING),
            Param::new("jwks", TY_DICT),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("egress_policy", &[Param::new("config", TY_DICT)], TY_DICT),
    BuiltinSignature::variadic("exec", &[Param::new("command", TY_STRING)], TY_DICT),
    BuiltinSignature::variadic(
        "exec_at",
        &[
            Param::new("dir", TY_STRING),
            Param::new("command", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("git.conflicts", &[Param::new("repo", TY_STRING)], TY_DICT),
    BuiltinSignature::simple(
        "git.diff",
        &[
            Param::new("repo", TY_STRING),
            Param::optional("selector", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.fetch",
        &[
            Param::new("repo", TY_STRING),
            Param::new("remote", TY_STRING),
            Param::optional("refspecs", TY_LIST),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.merge_base",
        &[
            Param::new("repo", TY_STRING),
            Param::new("left", TY_STRING),
            Param::new("right", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.push",
        &[
            Param::new("repo", TY_STRING),
            Param::new("remote", TY_STRING),
            Param::new("refspec", TY_STRING),
            Param::optional("lease", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.rebase",
        &[
            Param::new("repo", TY_STRING),
            Param::new("base_ref", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.repo.discover",
        &[Param::new("path", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple("git.status", &[Param::new("repo", TY_STRING)], TY_DICT),
    BuiltinSignature::simple(
        "git.worktree.create",
        &[
            Param::new("repo", TY_STRING),
            Param::new("branch", TY_STRING),
            Param::new("path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "git.worktree.remove",
        &[
            Param::new("path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("harn.mcp.roots", &[], TY_LIST),
    BuiltinSignature::simple(
        "host_call",
        &[
            Param::new("name", TY_STRING),
            Param::optional("params", TY_DICT),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple("host_capabilities", &[], TY_DICT),
    BuiltinSignature::simple(
        "host_has",
        &[
            Param::new("capability", TY_STRING),
            Param::optional("operation", TY_STRING),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "host_mock",
        &[
            Param::new("capability", TY_STRING),
            Param::new("operation", TY_STRING),
            Param::optional("result_or_config", TY_ANY),
            Param::optional("params", TY_DICT),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple("host_mock_calls", &[], TY_LIST),
    BuiltinSignature::simple("host_mock_clear", &[], TY_NIL),
    BuiltinSignature::simple("host_mock_pop_scope", &[], TY_NIL),
    BuiltinSignature::simple("host_mock_push_scope", &[], TY_NIL),
    BuiltinSignature::simple(
        "host_tool_call",
        &[
            Param::new("name", TY_STRING),
            Param::optional("args", TY_ANY),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple("host_tool_list", &[], TY_LIST),
    BuiltinSignature::simple(
        "http_delete",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_download",
        &[
            Param::new("url", TY_STRING),
            Param::new("dst_path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_get",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_header",
        &[
            Param::new("source", Ty::Union(&[TY_DICT, TY_LIST])),
            Param::new("name", TY_STRING),
        ],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::simple(
        "http_mock",
        &[
            Param::new("method", TY_STRING),
            Param::new("url_pattern", TY_STRING),
            Param::optional("response", TY_DICT),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "http_mock_calls",
        &[Param::optional("options", TY_DICT)],
        TY_LIST,
    ),
    BuiltinSignature::simple("http_mock_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "http_patch",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_post",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_put",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_request",
        &[
            Param::new("method", TY_STRING),
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response",
        &[
            Param::optional("status", TY_INT),
            Param::optional("body", TY_ANY),
            Param::optional("headers", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_bytes",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_json",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_text",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_after",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_before",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_on_shutdown",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_readiness",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_ready",
        &[Param::new("server", TY_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_request",
        &[
            Param::new("server", TY_DICT),
            Param::new("request", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_route",
        &[
            Param::new("server", TY_DICT),
            Param::new("method", TY_STRING),
            Param::new("template", TY_STRING),
            Param::new("handler", TY_CLOSURE),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_security_headers",
        &[Param::new("tls_config", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_set_ready",
        &[Param::new("server", TY_DICT), Param::new("ready", TY_BOOL)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_shutdown",
        &[Param::new("server", TY_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_test",
        &[
            Param::new("server", TY_DICT),
            Param::new("request", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_tls_edge",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_tls_pem",
        &[
            Param::new("cert_path", TY_STRING),
            Param::new("key_path", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("http_server_tls_plain", &[], TY_DICT),
    BuiltinSignature::simple(
        "http_server_tls_self_signed_dev",
        &[Param::optional("hosts", Ty::Union(&[TY_LIST, TY_STRING]))],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_session",
        &[Param::optional("options", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "http_session_close",
        &[Param::new("session", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_session_request",
        &[
            Param::new("session", TY_STRING),
            Param::new("method", TY_STRING),
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_stream_close",
        &[Param::new("stream", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_stream_info",
        &[Param::new("stream", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_stream_open",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "http_stream_read",
        &[
            Param::new("stream", TY_STRING),
            Param::optional("max_bytes", TY_INT),
        ],
        TY_BYTES_OR_NIL,
    ),
    BuiltinSignature::simple("list_providers_native", &[], TY_LIST),
    BuiltinSignature::simple(
        "load_skill",
        &[Param::new("request", TY_STRING_OR_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "mcp_call",
        &[
            Param::new("client", TY_ANY),
            Param::new("tool_name", TY_STRING),
            Param::optional("arguments", TY_DICT),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple(
        "mcp_connect",
        &[
            Param::new("command", TY_STRING),
            Param::optional("args", TY_LIST),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple("mcp_disconnect", &[Param::new("client", TY_ANY)], TY_NIL),
    BuiltinSignature::simple("mcp_elicit", &[Param::new("config", TY_DICT)], TY_DICT),
    BuiltinSignature::simple(
        "mcp_ensure_active",
        &[Param::new("name", TY_STRING)],
        TY_ANY,
    ),
    BuiltinSignature::simple(
        "mcp_get_prompt",
        &[
            Param::new("client", TY_ANY),
            Param::new("name", TY_STRING),
            Param::optional("arguments", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("mcp_list_prompts", &[Param::new("client", TY_ANY)], TY_LIST),
    BuiltinSignature::simple(
        "mcp_list_resource_templates",
        &[Param::new("client", TY_ANY)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "mcp_list_resources",
        &[Param::new("client", TY_ANY)],
        TY_LIST,
    ),
    BuiltinSignature::simple("mcp_list_tools", &[Param::new("client", TY_ANY)], TY_LIST),
    BuiltinSignature::simple("mcp_prompt", &[Param::new("config", TY_DICT)], TY_NIL),
    BuiltinSignature::simple(
        "mcp_read_resource",
        &[Param::new("client", TY_ANY), Param::new("uri", TY_STRING)],
        Ty::Union(&[TY_STRING, TY_LIST, TY_NIL]),
    ),
    BuiltinSignature::simple("mcp_registry_status", &[], TY_LIST),
    BuiltinSignature::simple("mcp_release", &[Param::new("name", TY_STRING)], TY_NIL),
    BuiltinSignature::simple(
        "mcp_report_progress",
        &[
            Param::new("progress", TY_NUMBER),
            Param::optional("opts", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple("mcp_resource", &[Param::new("config", TY_DICT)], TY_NIL),
    BuiltinSignature::simple(
        "mcp_resource_template",
        &[Param::new("config", TY_DICT)],
        TY_NIL,
    ),
    BuiltinSignature::simple("mcp_roots", &[], TY_LIST),
    BuiltinSignature::simple("mcp_serve", &[Param::new("registry", TY_DICT)], TY_NIL),
    BuiltinSignature::simple(
        "mcp_server_card",
        &[Param::new("target", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple("mcp_server_info", &[Param::new("client", TY_ANY)], TY_DICT),
    BuiltinSignature::simple("mcp_tools", &[Param::new("registry", TY_DICT)], TY_NIL),
    BuiltinSignature::simple(
        "prompt_user",
        &[Param::optional("message", TY_STRING)],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::simple(
        "provider_capabilities",
        &[
            Param::new("provider", TY_STRING),
            Param::optional("model", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("provider_capabilities_clear", &[], TY_BOOL),
    BuiltinSignature::simple(
        "provider_capabilities_install",
        &[Param::new("toml_src", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "provider_register",
        &[Param::new("name", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple("runtime_paths", &[], TY_DICT),
    BuiltinSignature::simple("sandbox_active_backend", &[], TY_STRING),
    BuiltinSignature::simple("sandbox_active_profile", &[], TY_STRING),
    BuiltinSignature::simple("sandbox_backend_available", &[], TY_BOOL),
    BuiltinSignature::simple("shell", &[Param::new("command", TY_STRING)], TY_DICT),
    BuiltinSignature::simple(
        "shell_at",
        &[
            Param::new("dir", TY_STRING),
            Param::new("command", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("skill_count", &[Param::new("registry", TY_DICT)], TY_INT),
    BuiltinSignature::simple(
        "skill_define",
        &[
            Param::new("registry", TY_DICT),
            Param::new("name", TY_STRING),
            Param::new("config", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "skill_describe",
        &[Param::new("registry", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "skill_find",
        &[
            Param::new("registry", TY_DICT),
            Param::new("name", TY_STRING),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("skill_list", &[Param::new("registry", TY_DICT)], TY_LIST),
    BuiltinSignature::simple("skill_registry", &[], TY_DICT),
    BuiltinSignature::simple(
        "skill_remove",
        &[
            Param::new("registry", TY_DICT),
            Param::new("name", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "skill_render",
        &[
            Param::new("skill", Ty::Union(&[TY_DICT, TY_STRING])),
            Param::optional("arguments", TY_LIST),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "skill_select",
        &[
            Param::new("registry", TY_DICT),
            Param::new("names", TY_LIST),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "skill_who_signed",
        &[
            Param::new("registry", TY_DICT),
            Param::new("name", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "skills_catalog_entries",
        &[Param::new("registry", TY_DICT)],
        TY_LIST,
    ),
    BuiltinSignature::simple("sse_close", &[Param::new("stream", TY_STRING)], TY_BOOL),
    BuiltinSignature::simple(
        "sse_connect",
        &[
            Param::optional("method", TY_STRING),
            Param::optional("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "sse_event",
        &[
            Param::new("event", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "sse_mock",
        &[
            Param::new("url_pattern", TY_STRING),
            Param::optional("events", TY_LIST),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "sse_receive",
        &[
            Param::new("stream", TY_STRING),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "sse_server_cancel",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::optional("reason", TY_STRING),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_cancelled",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_close",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_disconnected",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_flush",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_heartbeat",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::optional("comment", TY_STRING),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_mock_disconnect",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_mock_receive",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "sse_server_response",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "sse_server_send",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::new("event", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_status",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_bind",
        &[Param::optional("registry", TY_TOOL_REGISTRY)],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "__host_current_tool_registry",
        &[],
        Ty::Union(&[TOOL_REGISTRY, TY_NIL]),
    ),
    BuiltinSignature::simple(
        "tool_count",
        &[Param::new("registry", TY_TOOL_REGISTRY)],
        TY_INT,
    ),
    BuiltinSignature::simple("tool_def", &[Param::new("name", TY_STRING)], TY_DICT),
    BuiltinSignature::simple(
        "tool_define",
        &[
            Param::new("registry", TY_TOOL_REGISTRY),
            Param::new("name", TY_STRING),
            Param::new("description", TY_STRING),
            Param::new("config", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_describe",
        &[Param::new("registry", TY_TOOL_REGISTRY)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "tool_find",
        &[
            Param::new("registry", TY_TOOL_REGISTRY),
            Param::new("name", TY_STRING),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "tool_format_result",
        &[Param::new("name", TY_STRING), Param::new("result", TY_ANY)],
        TY_STRING,
    ),
    BuiltinSignature::simple("__tool_hooks_classifier_cache_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "__tool_hooks_classifier_cache_get",
        &[
            Param::new("key", TY_STRING),
            Param::optional("now_ms", TY_INT),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple(
        "__tool_hooks_classifier_cache_put",
        &[
            Param::new("key", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("now_ms", TY_INT),
            Param::optional("ttl_ms", TY_INT),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "tool_hooks_emit_audit",
        &[
            Param::new("kind", TY_STRING),
            Param::optional("payload", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_hooks_filter",
        &[
            Param::new("registry", TY_DICT),
            Param::optional("stacks", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_hooks_inject_reminder",
        &[Param::new("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_hooks_list",
        &[Param::new("registry", TY_DICT)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "tool_hooks_match",
        &[
            Param::new("registry", TY_DICT),
            Param::new("command", TY_STRING),
            Param::optional("context", TY_ANY),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "tool_hooks_register",
        &[
            Param::new("registry", TY_DICT),
            Param::new("catalogue", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("tool_hooks_registry", &[], TY_DICT),
    BuiltinSignature::simple(
        "tool_hooks_unregister",
        &[
            Param::new("registry", TY_DICT),
            Param::new("catalogue_id", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_list",
        &[Param::new("registry", TY_TOOL_REGISTRY)],
        TY_LIST,
    ),
    BuiltinSignature::simple("tool_parse_call", &[Param::new("text", TY_STRING)], TY_LIST),
    BuiltinSignature::simple(
        "tool_prompt",
        &[Param::new("registry", TY_TOOL_REGISTRY)],
        TY_STRING,
    ),
    BuiltinSignature::simple("tool_ref", &[Param::new("name", TY_STRING)], TY_STRING),
    BuiltinSignature::simple("tool_registry", &[], TOOL_REGISTRY),
    BuiltinSignature::simple(
        "tool_remove",
        &[
            Param::new("registry", TY_TOOL_REGISTRY),
            Param::new("name", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("tool_rule", &[Param::new("config", TY_DICT)], TY_DICT),
    BuiltinSignature::simple(
        "tool_schema",
        &[
            Param::new("registry", TY_TOOL_REGISTRY),
            Param::optional("components", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_select",
        &[
            Param::new("registry", TY_TOOL_REGISTRY),
            Param::new("names", TY_LIST),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_surface_validate",
        &[
            Param::optional("surface", TY_ANY),
            Param::optional("input", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "tool_synth_invoke",
        &[Param::new("id", TY_STRING), Param::optional("args", TY_ANY)],
        TY_ANY,
    ),
    BuiltinSignature::simple("tool_synthesis_cache", &[], TY_LIST),
    BuiltinSignature::simple("tool_synthesis_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "tool_synthesize",
        &[Param::new("spec", TY_DICT)],
        TY_CLOSURE,
    ),
    BuiltinSignature::simple("transport_mock_calls", &[], TY_LIST),
    BuiltinSignature::simple("transport_mock_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "websocket_accept",
        &[
            Param::new("server", TY_STRING_OR_DICT),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_close",
        &[Param::new("socket", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_connect",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "websocket_mock",
        &[
            Param::new("url_pattern", TY_STRING),
            Param::optional("config", TY_ANY),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_receive",
        &[
            Param::new("socket", TY_STRING_OR_DICT),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_route",
        &[
            Param::new("server", TY_STRING_OR_DICT),
            Param::new("path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_send",
        &[
            Param::new("socket", TY_STRING_OR_DICT),
            Param::new("message", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_server",
        &[
            Param::optional("bind", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "websocket_server_close",
        &[Param::new("server", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
];
