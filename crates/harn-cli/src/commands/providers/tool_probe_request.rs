use crate::cli::ProviderToolProbeArgs;

pub(crate) fn render(args: &ProviderToolProbeArgs) -> i32 {
    match harn_vm::llm::tool_conformance::tool_conformance_request_report_json(
        args.provider.clone(),
        args.model.clone(),
        args.base_url.clone(),
        args.mode.tool_probe_modes(),
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
