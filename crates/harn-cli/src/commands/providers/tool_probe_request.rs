use crate::cli::ProviderToolProbeArgs;

pub(crate) fn render(args: &ProviderToolProbeArgs) -> i32 {
    match harn_vm::llm::tool_conformance::tool_conformance_request_report_json_for_format(
        args.provider.clone(),
        args.model.clone(),
        args.base_url.clone(),
        args.mode.tool_probe_modes(),
        args.requested_tool_probe_format(),
        args.probe_case.tool_probe_case(),
        args.request_profile.tool_probe_request_profile(),
        args.marker.clone(),
    ) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
pub(crate) fn resolve_tool_probe_wire_model(
    provider: &str,
    selector: &str,
) -> Result<String, String> {
    let resolved = harn_vm::llm_config::resolve_model_info(selector);
    if (resolved.alias.is_some()
        || harn_vm::llm_config::model_catalog_entry(&resolved.id).is_some())
        && resolved.provider != provider
    {
        return Err(format!(
            "error: model selector `{selector}` resolves to provider `{}`, not requested provider `{provider}`",
            resolved.provider
        ));
    }
    Ok(harn_vm::llm_config::wire_model_id(&resolved.id))
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_probe_wire_model;

    #[test]
    fn resolves_alias_to_provider_wire_model() {
        assert_eq!(
            resolve_tool_probe_wire_model("vercel_ai_gateway", "vercel-gpt-5.4-nano")
                .expect("alias resolves"),
            "openai/gpt-5.4-nano"
        );
    }

    #[test]
    fn rejects_alias_for_a_different_provider() {
        let error = resolve_tool_probe_wire_model("openai", "vercel-gpt-5.4-nano")
            .expect_err("provider mismatch");
        assert!(error.contains("vercel_ai_gateway"));
    }
}
