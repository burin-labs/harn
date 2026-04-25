use std::collections::BTreeMap;

use serde::Serialize;

use crate::orchestration::CapabilityPolicy;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorExportEffectClass {
    HotPathLocal,
    ConnectorOutbound,
    Activation,
}

#[derive(Clone, Debug, Default)]
pub struct HarnConnectorEffectPolicies {
    overrides: BTreeMap<String, Option<CapabilityPolicy>>,
}

impl HarnConnectorEffectPolicies {
    pub fn set_export_policy(
        &mut self,
        export: impl Into<String>,
        policy: CapabilityPolicy,
    ) -> &mut Self {
        self.overrides.insert(export.into(), Some(policy));
        self
    }

    pub fn trust_export(&mut self, export: impl Into<String>) -> &mut Self {
        self.overrides.insert(export.into(), None);
        self
    }

    pub fn clear_export_override(&mut self, export: &str) -> &mut Self {
        self.overrides.remove(export);
        self
    }

    pub(crate) fn policy_for_export(&self, export: &str) -> Option<CapabilityPolicy> {
        self.overrides
            .get(export)
            .cloned()
            .unwrap_or_else(|| default_connector_export_policy(export))
    }
}

pub fn connector_export_effect_class(export: &str) -> Option<ConnectorExportEffectClass> {
    match export {
        "normalize_inbound" => Some(ConnectorExportEffectClass::HotPathLocal),
        "poll_tick" | "call" => Some(ConnectorExportEffectClass::ConnectorOutbound),
        "activate" => Some(ConnectorExportEffectClass::Activation),
        _ => None,
    }
}

pub fn default_connector_export_policy(export: &str) -> Option<CapabilityPolicy> {
    let class = connector_export_effect_class(export)?;
    Some(policy_for_effect_class(class))
}

pub fn connector_export_denied_builtin_reason(export: &str, builtin: &str) -> Option<&'static str> {
    let class = connector_export_effect_class(export)?;
    match builtin_effect_group(builtin)? {
        BuiltinEffectGroup::Workspace => Some("ambient filesystem access is not allowed"),
        BuiltinEffectGroup::Process => Some("process execution is not allowed"),
        BuiltinEffectGroup::Llm => Some("LLM calls are not allowed"),
        BuiltinEffectGroup::Mcp => Some("MCP/process-backed connector access is not allowed"),
        BuiltinEffectGroup::Host => Some("host calls require an explicit host-owned surface"),
        BuiltinEffectGroup::Network | BuiltinEffectGroup::ConnectorCall => match class {
            ConnectorExportEffectClass::HotPathLocal => {
                Some("outbound network/client calls are not allowed on the ingress hot path")
            }
            ConnectorExportEffectClass::ConnectorOutbound
            | ConnectorExportEffectClass::Activation => None,
        },
    }
}

fn policy_for_effect_class(class: ConnectorExportEffectClass) -> CapabilityPolicy {
    let mut capabilities = BTreeMap::new();
    capabilities.insert(
        "connector".to_string(),
        match class {
            ConnectorExportEffectClass::HotPathLocal => vec![
                "secret_get".to_string(),
                "event_log_emit".to_string(),
                "metrics_inc".to_string(),
            ],
            ConnectorExportEffectClass::ConnectorOutbound
            | ConnectorExportEffectClass::Activation => {
                vec![
                    "call".to_string(),
                    "secret_get".to_string(),
                    "event_log_emit".to_string(),
                    "metrics_inc".to_string(),
                ]
            }
        },
    );

    CapabilityPolicy {
        capabilities,
        side_effect_level: Some(match class {
            ConnectorExportEffectClass::HotPathLocal => "read_only".to_string(),
            ConnectorExportEffectClass::ConnectorOutbound
            | ConnectorExportEffectClass::Activation => "network".to_string(),
        }),
        ..CapabilityPolicy::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuiltinEffectGroup {
    Workspace,
    Process,
    Network,
    Llm,
    Mcp,
    Host,
    ConnectorCall,
}

fn builtin_effect_group(builtin: &str) -> Option<BuiltinEffectGroup> {
    match builtin {
        "read_file"
        | "read_file_result"
        | "read_file_bytes"
        | "write_file"
        | "write_file_bytes"
        | "append_file"
        | "copy_file"
        | "delete_file"
        | "mkdir"
        | "list_dir"
        | "file_exists"
        | "stat"
        | "project_fingerprint"
        | "project_scan_native"
        | "project_scan_tree_native"
        | "project_walk_tree_native"
        | "project_catalog_native"
        | "__agent_state_init"
        | "__agent_state_resume"
        | "__agent_state_write"
        | "__agent_state_read"
        | "__agent_state_list"
        | "__agent_state_delete"
        | "__agent_state_handoff" => Some(BuiltinEffectGroup::Workspace),
        "exec" | "exec_at" | "shell" | "shell_at" => Some(BuiltinEffectGroup::Process),
        "http_get"
        | "http_post"
        | "http_put"
        | "http_patch"
        | "http_delete"
        | "http_download"
        | "http_request"
        | "http_session_request"
        | "http_stream_open"
        | "http_stream_read"
        | "http_stream_close"
        | "http_stream_info"
        | "sse_connect"
        | "sse_receive"
        | "websocket_connect"
        | "websocket_send"
        | "websocket_receive" => Some(BuiltinEffectGroup::Network),
        "llm_call" | "llm_call_safe" | "llm_completion" | "llm_stream" | "llm_healthcheck"
        | "agent_loop" => Some(BuiltinEffectGroup::Llm),
        "vision_ocr" => Some(BuiltinEffectGroup::Process),
        "mcp_connect"
        | "mcp_ensure_active"
        | "mcp_call"
        | "mcp_list_tools"
        | "mcp_list_resources"
        | "mcp_list_resource_templates"
        | "mcp_read_resource"
        | "mcp_list_prompts"
        | "mcp_get_prompt"
        | "mcp_server_info"
        | "mcp_disconnect" => Some(BuiltinEffectGroup::Mcp),
        "host_call" | "host_tool_call" | "host_tool_list" => Some(BuiltinEffectGroup::Host),
        "connector_call" => Some(BuiltinEffectGroup::ConnectorCall),
        _ => None,
    }
}
