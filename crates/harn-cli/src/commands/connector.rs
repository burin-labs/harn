use crate::cli::{ConnectorArgs, ConnectorCommand};

pub(crate) async fn handle_connector_command(args: ConnectorArgs) -> Result<(), String> {
    match args.command {
        ConnectorCommand::Check(check) => {
            let report = super::package_verify::check_connector_package(&check).await?;
            if check.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|error| format!("failed to render connector report: {error}"))?
                );
            } else {
                super::package_verify::print_connector_report(&report);
            }
            Ok(())
        }
    }
}
