use crate::value::{DictMap, VmDictExt, VmError, VmValue};

/// Normalize the model-visible tool surface before any prompt, progressive
/// disclosure, provider schema, or call-option consumer can read it. Raw
/// `harness.llm.call(...)` and the agent loop therefore share the same typed
/// exposure boundary.
pub fn project_agent_tools(
    options: &mut Option<DictMap>,
) -> Result<Option<crate::llm::api::ToolSearchConfig>, VmError> {
    let search = super::tool_search::parse_tool_search_option(options.as_ref())?;
    let Some(options) = options.as_mut() else {
        return Ok(search);
    };
    let Some(tools) = options
        .get("tools")
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned()
    else {
        return Ok(search);
    };
    let projected = crate::tool_registry::project_tools_for_audience(
        &tools,
        crate::tool_registry::ToolAudience::Agent,
    )?;
    let projected = if search.is_some() {
        guidance_with_disclosed_schema(projected)
    } else {
        projected
    };
    options.insert(crate::value::intern_key("tools"), projected);
    Ok(search)
}

/// A deferred capability's instructions travel with its schema, never in the
/// global system prompt. Native search keeps this descriptor stable; client
/// search exposes the same descriptor when it promotes the tool. Consuming
/// `guidance` makes this projection idempotent and prevents double injection.
fn guidance_with_disclosed_schema(tools: VmValue) -> VmValue {
    let Some(entries) = super::system_prompt::tool_entry_list(Some(&tools)) else {
        return tools;
    };
    let entries = entries
        .into_iter()
        .map(|entry| {
            let Some(dict) = entry.as_dict() else {
                return entry;
            };
            if !matches!(dict.get("defer_loading"), Some(VmValue::Bool(true))) {
                return entry;
            }
            let Some(guidance) = dict.get("guidance").map(VmValue::display) else {
                return entry;
            };
            let mut tool = (**dict).clone();
            tool.remove("guidance");
            if !guidance.trim().is_empty() {
                let description = dict
                    .get("description")
                    .map(VmValue::display)
                    .unwrap_or_default();
                tool.put_str(
                    "description",
                    format!("{}\n\n{}", description.trim_end(), guidance.trim()),
                );
            }
            VmValue::dict(tool)
        })
        .collect::<Vec<_>>();
    match tools {
        VmValue::Dict(wrapper) => {
            let mut registry = (*wrapper).clone();
            registry.put("tools", VmValue::list(entries));
            VmValue::dict(registry)
        }
        _ => VmValue::list(entries),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::options::extract_llm_options;
    use crate::value::VmDictExt;

    fn tool(name: &str, guidance: &str, audiences: &[&str]) -> VmValue {
        let mut governance = DictMap::new();
        governance.put(
            "audiences",
            VmValue::List(
                audiences
                    .iter()
                    .map(|audience| VmValue::String((*audience).into()))
                    .collect::<Vec<_>>()
                    .into(),
            ),
        );
        let mut tool = DictMap::new();
        tool.put_str("name", name);
        tool.put_str("description", guidance);
        tool.put_str("guidance", guidance);
        tool.put("parameters", VmValue::dict(DictMap::new()));
        tool.put("governance", VmValue::dict(governance));
        VmValue::dict(tool)
    }

    #[test]
    fn direct_llm_options_project_agent_governance_before_prompt_and_schema() {
        let mut registry = DictMap::new();
        registry.put_str("_type", "tool_registry");
        registry.put(
            "tools",
            VmValue::List(
                vec![
                    tool("agent_visible", "PUBLIC_DIRECT_GUIDANCE", &["agent"]),
                    tool("operator_hidden", "PRIVATE_DIRECT_GUIDANCE", &["cli"]),
                ]
                .into(),
            ),
        );
        let mut options = DictMap::new();
        options.put_str("provider", "mock");
        options.put_str("model", "no-cache-model");
        options.put("tools", VmValue::dict(registry));
        // Tool guidance projection is under test, not the tool channel: model
        // the agent loop's call, which renders the text-channel contract.
        options.put_bool("_tool_contract_rendered", true);
        let opts = extract_llm_options(&[
            VmValue::String("hello".into()),
            VmValue::Nil,
            VmValue::dict(options),
        ])
        .expect("direct call options");

        let system = opts
            .system
            .expect("allowed guidance assembles a system prompt");
        assert!(system.contains("PUBLIC_DIRECT_GUIDANCE"));
        assert!(!system.contains("PRIVATE_DIRECT_GUIDANCE"));
        let projected = opts.tools.expect("projected tools retained");
        let names = projected
            .as_dict()
            .and_then(|registry| registry.get("tools"))
            .and_then(|tools| match tools {
                VmValue::List(tools) => Some(
                    tools
                        .iter()
                        .filter_map(|tool| {
                            tool.as_dict()
                                .and_then(|tool| tool.get("name"))
                                .map(VmValue::display)
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .expect("projected registry tools");
        assert_eq!(names, ["agent_visible"]);
    }
}
