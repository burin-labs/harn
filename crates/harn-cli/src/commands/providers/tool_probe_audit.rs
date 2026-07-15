use std::process;

use crate::cli::ProviderToolProbeAuditArgs;

pub(crate) fn run(args: ProviderToolProbeAuditArgs) {
    let report = harn_vm::llm::tool_conformance::tool_conformance_request_catalog_audit(
        args.probe_cases
            .into_iter()
            .map(|probe_case| probe_case.tool_probe_case())
            .collect(),
        args.mode.tool_probe_modes(),
    );
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("internal error: failed to render tool-probe audit report: {error}");
                process::exit(1);
            }
        }
    } else {
        println!(
            "provider tool-probe audit: {}/{} request validations passed across {} catalog routes",
            report.validation_pass_count, report.request_count, report.route_count
        );
        if report.validation_fail_count > 0 {
            for failure in report.failures.iter().take(10) {
                println!(
                    "- {}:{} {} {}: {}",
                    failure.provider,
                    failure.model,
                    failure.probe_case,
                    failure.mode,
                    failure.issues.join("; ")
                );
            }
        }
    }
    if report.validation_fail_count > 0 {
        process::exit(1);
    }
}
