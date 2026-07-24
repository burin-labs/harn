use std::process;

use crate::cli::{ProviderToolCalibrateArgs, ProviderToolProbeFormatArg};

pub(crate) async fn run(args: ProviderToolCalibrateArgs) {
    let exit_code = dispatch(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch(args: ProviderToolCalibrateArgs) -> i32 {
    let routes = match parse_routes(&args.routes) {
        Ok(routes) => routes,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let formats = if args.tool_formats.is_empty() {
        vec![
            ProviderToolProbeFormatArg::Native,
            ProviderToolProbeFormatArg::Json,
            ProviderToolProbeFormatArg::Text,
        ]
    } else {
        args.tool_formats.clone()
    };
    let requested_cases = if args.probe_cases.is_empty() {
        harn_vm::llm::tool_conformance::ToolProbeCase::catalog_request_audit_cases()
    } else {
        args.probe_cases
            .iter()
            .map(|case| case.tool_probe_case())
            .collect()
    };

    let mut reports = Vec::new();
    for (provider, requested_model) in routes {
        let model = match crate::commands::providers::resolve_tool_probe_wire_model(
            &provider,
            &requested_model,
        ) {
            Ok(model) => model,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
        for format in &formats {
            for probe_case in requested_cases
                .iter()
                .copied()
                .filter(|case| case.is_live_applicable(&provider, &model))
                .filter(|case| {
                    *case
                        != harn_vm::llm::tool_conformance::ToolProbeCase::SignedThinkingToolResultFollowup
                        || *format == ProviderToolProbeFormatArg::Native
                })
            {
                let mut options =
                    harn_vm::llm::tool_conformance::ToolConformanceProbeOptions::new(
                        provider.clone(),
                        model.clone(),
                    );
                options.tool_format = format.tool_probe_format();
                options.strict_tool_format = true;
                options.modes = args.mode.tool_probe_modes();
                options.probe_case = probe_case;
                options.repeat = usize::from(args.repeat);
                options.timeout_secs = args.timeout_secs;
                reports.push(
                    harn_vm::llm::tool_conformance::run_tool_conformance_probe(options).await,
                );
            }
        }
    }

    let store = harn_vm::llm::tool_scorecard::fitness_store_from_tool_reports(&reports);
    if let Err(error) = write_store(&args.output, &store) {
        eprintln!("{error}");
        return 1;
    }
    if args.json {
        match serde_json::to_string_pretty(&store) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: failed to render tool-format fitness: {error}");
                return 1;
            }
        }
    } else {
        println!(
            "wrote {} records and {} route recommendations to {}",
            store.records.len(),
            store.recommendations.len(),
            args.output.display()
        );
    }
    0
}

fn parse_routes(routes: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut parsed = Vec::with_capacity(routes.len());
    for route in routes {
        let Some((provider, model)) = route.split_once(':') else {
            return Err(format!(
                "error: invalid route {route:?}; expected provider:model"
            ));
        };
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(format!(
                "error: invalid route {route:?}; provider and model must be non-empty"
            ));
        }
        parsed.push((provider.to_string(), model.to_string()));
    }
    parsed.sort();
    parsed.dedup();
    Ok(parsed)
}

fn write_store(
    output: &std::path::Path,
    store: &harn_vm::llm::tool_scorecard::ToolFormatFitnessStore,
) -> Result<(), String> {
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("error: failed to create {}: {error}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("error: failed to serialize tool-format fitness: {error}"))?;
    bytes.push(b'\n');
    harn_vm::atomic_io::atomic_write(output, &bytes)
        .map_err(|error| format!("error: failed to write {}: {error}", output.display()))
}
