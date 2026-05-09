use std::fs;
use std::path::Path;

use crate::cli::{WorkflowArgs, WorkflowCommand};

pub(crate) fn handle(args: WorkflowArgs) -> Result<i32, String> {
    match args.command {
        WorkflowCommand::Validate(args) => {
            let bundle = harn_vm::orchestration::load_workflow_bundle(Path::new(&args.bundle))
                .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
            let report = harn_vm::orchestration::validate_workflow_bundle(&bundle);
            if args.json {
                print_json(&report)?;
            } else if report.valid {
                println!(
                    "valid workflow bundle {} ({})",
                    report.bundle_id, report.graph_digest
                );
            } else {
                println!("invalid workflow bundle {}", report.bundle_id);
                for diagnostic in &report.errors {
                    println!("error {}: {}", diagnostic.path, diagnostic.message);
                }
                for diagnostic in &report.warnings {
                    println!("warning {}: {}", diagnostic.path, diagnostic.message);
                }
            }
            Ok(if report.valid { 0 } else { 1 })
        }
        WorkflowCommand::Preview(args) => {
            let bundle = harn_vm::orchestration::load_workflow_bundle(Path::new(&args.bundle))
                .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
            let preview = harn_vm::orchestration::preview_workflow_bundle(&bundle);
            if args.json {
                print_json(&preview)?;
            } else if args.mermaid {
                println!("{}", preview.mermaid);
            } else {
                println!(
                    "workflow bundle {} graph {}",
                    preview.bundle_id, preview.graph_digest
                );
                println!("nodes: {}", preview.nodes.len());
                println!("edges: {}", preview.edges.len());
                println!("triggers: {}", preview.triggers.len());
                println!("connectors: {}", preview.connectors.len());
            }
            Ok(if preview.validation.valid { 0 } else { 1 })
        }
        WorkflowCommand::Run(args) => {
            let bundle = harn_vm::orchestration::load_workflow_bundle(Path::new(&args.bundle))
                .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
            let receipt = harn_vm::orchestration::run_workflow_bundle(
                &bundle,
                harn_vm::orchestration::WorkflowBundleRunRequest {
                    trigger_id: args.trigger_id,
                    event_id: args.event_id,
                },
            )
            .map_err(|report| validation_error_message(&report))?;
            if let Some(path) = args.receipt_out.as_ref() {
                let text = serde_json::to_string_pretty(&receipt)
                    .map_err(|error| format!("failed to serialize receipt: {error}"))?;
                fs::write(path, text)
                    .map_err(|error| format!("failed to write receipt {path}: {error}"))?;
            }
            if args.json {
                print_json(&receipt)?;
            } else {
                println!(
                    "completed workflow bundle {} run {}",
                    receipt.bundle_id, receipt.run_id
                );
                println!("graph: {}", receipt.graph_digest);
                println!("nodes: {}", receipt.executed_nodes.len());
            }
            Ok(0)
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let output = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to serialize JSON output: {error}"))?;
    println!("{output}");
    Ok(())
}

fn validation_error_message(
    report: &harn_vm::orchestration::WorkflowBundleValidationReport,
) -> String {
    let mut lines = vec![format!("invalid workflow bundle {}", report.bundle_id)];
    for diagnostic in &report.errors {
        lines.push(format!("{}: {}", diagnostic.path, diagnostic.message));
    }
    lines.join("\n")
}
