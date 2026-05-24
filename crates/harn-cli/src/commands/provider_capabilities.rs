use crate::cli::{ProviderArgs, ProviderCapabilitiesCommand, ProviderCommand};

pub(crate) fn run(args: ProviderArgs) -> Result<(), String> {
    match args.command {
        ProviderCommand::Capabilities(capabilities) => match capabilities.command {
            ProviderCapabilitiesCommand::Audit(audit) => run_audit(audit.json),
        },
    }
}

pub(crate) fn run_or_exit(args: ProviderArgs) {
    run(args).unwrap_or_else(|error| crate::command_error(&error));
}

fn run_audit(json: bool) -> Result<(), String> {
    let report = harn_vm::llm::capabilities::audit_catalogued_chat_model_tool_capabilities();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to render capability audit JSON: {error}"))?
        );
    } else if report.ok() {
        println!("{}", report.render_human());
    } else {
        eprintln!("{}", report.render_human());
    }
    report
        .ok()
        .then_some(())
        .ok_or_else(|| "provider capability audit failed".to_string())
}
