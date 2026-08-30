use crate::value::{DictMap, VmError, VmValue};

/// Normalize the model-visible tool surface before any prompt, progressive
/// disclosure, provider schema, or call-option consumer can read it. Raw
/// `harness.llm.call(...)` and the agent loop therefore share the same typed
/// exposure boundary.
pub(crate) fn project_agent_tools(options: &mut Option<DictMap>) -> Result<(), VmError> {
    let Some(options) = options.as_mut() else {
        return Ok(());
    };
    let Some(tools) = options
        .get("tools")
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned()
    else {
        return Ok(());
    };
    let projected = crate::tool_registry::project_tools_for_audience(
        &tools,
        crate::tool_registry::ToolAudience::Agent,
    )?;
    options.insert(crate::value::intern_key("tools"), projected);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::options::extract_llm_options;
    use crate::value::VmDictExt;

    fn tool(name: &str, guidance: &str, audiences: &[&str]) -> VmValue {
        let mut governance = DictMap::new();
        governance.insert(
            "audiences".into(),
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
        tool.insert("parameters".into(), VmValue::dict(DictMap::new()));
        tool.insert("governance".into(), VmValue::dict(governance));
        VmValue::dict(tool)
    }

    #[test]
    fn direct_llm_options_project_agent_governance_before_prompt_and_schema() {
        let mut registry = DictMap::new();
        registry.put_str("_type", "tool_registry");
        registry.insert(
            "tools".into(),
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
        options.insert("tools".into(), VmValue::dict(registry));
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
