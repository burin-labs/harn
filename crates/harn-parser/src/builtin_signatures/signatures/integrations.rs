//! Connector, host, tool, and shell-facing builtin signatures.

use super::{BuiltinReturn, BuiltinSig, UNION_BYTES_NIL, UNION_DICT_NIL, UNION_STRING_NIL};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "connector_call",
        return_type: None,
    },
    BuiltinSig {
        name: "exec",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "exec_at",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "host_call",
        return_type: None,
    },
    BuiltinSig {
        name: "host_capabilities",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "host_has",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "host_tool_call",
        return_type: None,
    },
    BuiltinSig {
        name: "host_tool_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "host_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "host_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "host_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "http_delete",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_download",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_get",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "http_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "http_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "http_patch",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_post",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_put",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_request",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_session",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "http_session_close",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "http_session_request",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_stream_close",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "http_stream_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_stream_open",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "http_stream_read",
        return_type: Some(BuiltinReturn::Union(UNION_BYTES_NIL)),
    },
    BuiltinSig {
        name: "list_providers_native",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "load_skill",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "mcp_call",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_connect",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_disconnect",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_ensure_active",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_get_prompt",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "mcp_list_prompts",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_list_resource_templates",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_list_resources",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_list_tools",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_prompt",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_read_resource",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_registry_status",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_release",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_resource",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_resource_template",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_serve",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_server_card",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "mcp_server_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "mcp_tools",
        return_type: None,
    },
    BuiltinSig {
        name: "prompt_user",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "provider_capabilities",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "provider_capabilities_clear",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "provider_capabilities_install",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "provider_register",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "runtime_paths",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "shell",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "shell_at",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "sse_close",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "sse_connect",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sse_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "sse_receive",
        return_type: Some(BuiltinReturn::Union(UNION_DICT_NIL)),
    },
    BuiltinSig {
        name: "skill_count",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "skill_define",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "skill_describe",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "skill_find",
        return_type: None,
    },
    BuiltinSig {
        name: "skill_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "skill_registry",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "skill_remove",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "skill_render",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "skill_select",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "skills_catalog_entries",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "tool_bind",
        return_type: None,
    },
    BuiltinSig {
        name: "tool_count",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "tool_def",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_define",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_describe",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_find",
        return_type: None,
    },
    BuiltinSig {
        name: "tool_format_result",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "tool_parse_call",
        return_type: None,
    },
    BuiltinSig {
        name: "tool_prompt",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_ref",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transport_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transport_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "tool_registry",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_remove",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_select",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "websocket_close",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "websocket_connect",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "websocket_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "websocket_receive",
        return_type: Some(BuiltinReturn::Union(UNION_DICT_NIL)),
    },
    BuiltinSig {
        name: "websocket_send",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
];
