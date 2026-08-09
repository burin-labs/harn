use serde_json::{json, Value};

use super::{ToolProbeFormat, TOOL_PROBE_TOOL_NAME};

pub(super) fn probe_tool_contract(tool_format: ToolProbeFormat) -> Result<Option<String>, String> {
    if tool_format == ToolProbeFormat::Native {
        return Ok(None);
    }
    let tool_listing = format!(
        "### {TOOL_PROBE_TOOL_NAME}\nReturn the supplied marker unchanged.\nParameters: {}",
        json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        })
    );
    let render = |path: &str, bindings: Value| -> Result<String, String> {
        let template = harn_stdlib::get_stdlib_prompt_asset(path)
            .ok_or_else(|| format!("missing stdlib prompt asset `{path}`"))?;
        let crate::value::VmValue::Dict(bindings) = crate::schema::json_to_vm_value(&bindings)
        else {
            return Err("tool-probe prompt bindings must be a dict".to_string());
        };
        crate::stdlib::template::render_template_to_string(
            template,
            Some(bindings.as_ref()),
            None,
            None,
        )
    };
    if tool_format == ToolProbeFormat::Json {
        return render(
            "agent/prompts/tool_contract_json.harn.prompt",
            json!({
                "done_sentinel": null,
                "body_hint": "Use the declared parameter names exactly.",
                "expanded_schemas": tool_listing,
            }),
        )
        .map(Some);
    }
    let response_protocol = render(
        "agent/prompts/tool_contract_text_response_protocol.harn.prompt",
        json!({
            "done_sentinel": null,
            "body_hint": "Use the declared parameter names exactly.",
        }),
    )?;
    render(
        "agent/prompts/tool_contract_text.harn.prompt",
        json!({
            "mode": "text",
            "native_mode": false,
            "require_action": false,
            "done_sentinel": null,
            "include_task_ledger_help": false,
            "tool_examples": "",
            "shared_types": "",
            "expanded_schemas": tool_listing,
            "compact_schemas": "",
            "native_contract": "",
            "action_native_contract": "",
            "task_ledger_contract": "",
            "text_response_protocol": response_protocol,
            "action_text_contract": "",
        }),
    )
    .map(Some)
}
