use harn_vm::llm::capabilities::{self, ToolFormatDecision};
use harn_vm::llm_config::{self, ResolvedModel};

use super::{resolve_lora_provider, BaseModelReport, ToolCallingReport};

/// The same resolved training route feeds planning, corpus validation, export,
/// manifests, and training. Commands own their actions, not model defaults.
pub(super) struct LoraModelRoute {
    pub(super) resolved: ResolvedModel,
    pub(super) provider: String,
    pub(super) base: BaseModelReport,
    pub(super) tool_calling: ToolCallingReport,
    pub(super) decision: ToolFormatDecision,
}

impl LoraModelRoute {
    pub(super) fn resolve(selector: &str, provider: Option<&str>, tool_format: &str) -> Self {
        let resolved = llm_config::resolve_model_info(selector);
        let provider = resolve_lora_provider(provider, &resolved.provider);
        let catalog = llm_config::model_catalog_entry(&resolved.id);
        let capabilities = capabilities::lookup(&provider, &resolved.id);
        let default_tool_format = llm_config::default_tool_format(&resolved.id, &provider);
        let decision = if tool_format == "auto" {
            ToolFormatDecision {
                effective: default_tool_format.clone(),
                correction: None,
            }
        } else {
            capabilities::validate_tool_format(&provider, &resolved.id, tool_format)
        };
        let base = BaseModelReport {
            selector: selector.to_string(),
            id: resolved.id.clone(),
            provider: provider.clone(),
            resolved_alias: resolved.alias.clone(),
            tool_format: default_tool_format,
            tier: resolved.tier.clone(),
            family: resolved.family.clone(),
            lineage: resolved.lineage.clone(),
            catalog_name: catalog.as_ref().map(|model| model.name.clone()),
            context_window: catalog.as_ref().map(|model| model.context_window),
        };
        let tool_calling = ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        };
        Self {
            resolved,
            provider,
            base,
            tool_calling,
            decision,
        }
    }
}
